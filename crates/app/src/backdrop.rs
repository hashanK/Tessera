//! The wallpaper behind the groups, for the Acrylic, Mica and Frosted materials.
//!
//! Windows' own blur (accent policy, system backdrops) does not work on per-pixel-alpha layered
//! windows such as the groups: tested in `spikes/blur`, it draws an opaque grey rectangle with
//! square corners. Groups only ever sit on the desktop, so what is behind them is the wallpaper;
//! it is captured once, at a quarter of its size, from the window Explorer draws it in, and
//! each group blurs its own small piece when it paints. Nothing is captured continuously: only
//! at first use and when the wallpaper or the display layout changes.
use std::ffi::c_void;
use tessera_core::Rect;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::*;

/// The wallpaper is kept at 1/FACTOR of its size. Blur hides the lost detail.
pub const FACTOR: i32 = 4;

pub struct Backdrop {
    /// Screen position of the captured area's top-left corner.
    pub origin: (i32, i32),
    pub w: i32,
    pub h: i32,
    /// Opaque BGRA, `w * h`.
    px: Vec<u8>,
}

/// A blurred piece of the wallpaper, ready to become a bitmap.
pub struct Piece {
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
    /// Where the requested screen rectangle lies within this piece, in piece pixels.
    pub src: (f32, f32, f32, f32),
}

/// The window the wallpaper is painted in: a top-level `WorkerW` without the icon view where
/// Explorer has split them, otherwise `Progman` itself (the icon view inside it is hidden while
/// Tessera runs, so it is not in the capture).
unsafe fn wallpaper_window() -> Option<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> windows::core::BOOL {
        let found = &mut *(lp.0 as *mut Option<HWND>);
        let mut class = [0u16; 32];
        let n = GetClassNameW(hwnd, &mut class) as usize;
        if String::from_utf16_lossy(&class[..n]) == "WorkerW"
            && IsWindowVisible(hwnd).as_bool()
            && FindWindowExW(Some(hwnd), None, w!("SHELLDLL_DefView"), None).is_err()
        {
            let mut r = RECT::default();
            let _ = GetWindowRect(hwnd, &mut r);
            if r.right - r.left > 200 && r.bottom - r.top > 200 {
                *found = Some(hwnd);
                return false.into();
            }
        }
        true.into()
    }
    let mut found: Option<HWND> = None;
    let _ = EnumWindows(Some(each), LPARAM(&mut found as *mut _ as isize));
    found.or_else(|| FindWindowW(w!("Progman"), None).ok())
}

impl Backdrop {
    pub fn capture() -> Option<Backdrop> {
        unsafe {
            let hwnd = wallpaper_window()?;
            let mut r = RECT::default();
            GetWindowRect(hwnd, &mut r).ok()?;
            let (fw, fh) = (r.right - r.left, r.bottom - r.top);
            if fw <= 0 || fh <= 0 {
                return None;
            }
            let screen = GetDC(None);
            let dc = CreateCompatibleDC(Some(screen));
            ReleaseDC(None, screen);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: fw, biHeight: -fh, biPlanes: 1, biBitCount: 32, ..Default::default() },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let Ok(dib) = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(dc);
                return None;
            };
            let old = SelectObject(dc, dib.into());
            // PW_RENDERFULLCONTENT: ask DWM for the content, which also works for windows it composes.
            let ok = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(2)).as_bool();
            let full = std::slice::from_raw_parts(bits as *const u8, (fw * fh * 4) as usize);
            let backdrop = ok.then(|| Backdrop::shrink(full, fw, fh, (r.left, r.top)));
            SelectObject(dc, old);
            let _ = DeleteObject(dib.into());
            let _ = DeleteDC(dc);
            backdrop.filter(|b| !b.is_blank())
        }
    }

    /// Average FACTOR x FACTOR blocks.
    fn shrink(full: &[u8], fw: i32, fh: i32, origin: (i32, i32)) -> Backdrop {
        let (w, h) = ((fw / FACTOR).max(1), (fh / FACTOR).max(1));
        let mut px = vec![0u8; (w * h * 4) as usize];
        let n = (FACTOR * FACTOR) as u32;
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0u32; 3];
                for dy in 0..FACTOR {
                    let row = ((y * FACTOR + dy) * fw + x * FACTOR) as usize * 4;
                    for dx in 0..FACTOR as usize {
                        for c in 0..3 {
                            sum[c] += full[row + dx * 4 + c] as u32;
                        }
                    }
                }
                let i = ((y * w + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, 255]);
            }
        }
        Backdrop { origin, w, h, px }
    }

    /// An all-black capture means nothing was drawn (no wallpaper window content).
    fn is_blank(&self) -> bool {
        self.px.chunks_exact(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0)
    }

    /// Every fifth pixel in each direction, as RGB: enough to find the main colours.
    pub fn sample(&self) -> Vec<[u8; 3]> {
        let mut out = Vec::with_capacity(((self.w / 5 + 1) * (self.h / 5 + 1)) as usize);
        for y in (0..self.h).step_by(5) {
            for x in (0..self.w).step_by(5) {
                let i = ((y * self.w + x) * 4) as usize;
                out.push([self.px[i + 2], self.px[i + 1], self.px[i]]);
            }
        }
        out
    }

    /// Bytes held, for the memory budget.
    pub fn bytes(&self) -> usize {
        self.px.len()
    }

    /// The wallpaper under screen rectangle `r`, blurred by `radius` screen pixels.
    pub fn piece(&self, r: Rect, radius: f32) -> Option<Piece> {
        let f = FACTOR as f32;
        let (x0, y0) = ((r.x - self.origin.0) as f32 / f, (r.y - self.origin.1) as f32 / f);
        let (x1, y1) = (x0 + r.w as f32 / f, y0 + r.h as f32 / f);
        // Three box passes of radius k approximate a Gaussian; 1.7 maps the requested radius.
        let k = ((radius / f) / 1.7).round().max(0.0) as i32;
        let pad = 3 * k + 2;
        let (cx0, cy0) = (x0.floor() as i32 - pad, y0.floor() as i32 - pad);
        let (cx1, cy1) = (x1.ceil() as i32 + pad, y1.ceil() as i32 + pad);
        let (cw, ch) = (cx1 - cx0, cy1 - cy0);
        if cw <= 0 || ch <= 0 || cw > 4096 || ch > 4096 {
            return None;
        }
        // Copy with edge clamping, so a group at the screen edge still blurs cleanly.
        let mut buf = vec![0u8; (cw * ch * 4) as usize];
        for y in 0..ch {
            let sy = (cy0 + y).clamp(0, self.h - 1);
            for x in 0..cw {
                let sx = (cx0 + x).clamp(0, self.w - 1);
                let s = ((sy * self.w + sx) * 4) as usize;
                let d = ((y * cw + x) * 4) as usize;
                buf[d..d + 4].copy_from_slice(&self.px[s..s + 4]);
            }
        }
        for _ in 0..3 {
            box_blur(&mut buf, cw as usize, ch as usize, k as usize);
        }
        Some(Piece { w: cw as u32, h: ch as u32, bgra: buf, src: (x0 - cx0 as f32, y0 - cy0 as f32, x1 - cx0 as f32, y1 - cy0 as f32) })
    }
}

/// One horizontal and one vertical running-sum box blur of radius `k`, edges clamped.
pub fn box_blur(px: &mut [u8], w: usize, h: usize, k: usize) {
    if k == 0 || w == 0 || h == 0 {
        return;
    }
    let mut line = Vec::new();
    let div = (2 * k + 1) as u32;
    for pass in 0..2 {
        let (len, count) = if pass == 0 { (w, h) } else { (h, w) };
        let at = |i: usize, j: usize| if pass == 0 { (j * w + i) * 4 } else { (i * w + j) * 4 };
        for j in 0..count {
            line.clear();
            line.extend((0..len).flat_map(|i| {
                let p = at(i, j);
                [px[p], px[p + 1], px[p + 2], px[p + 3]]
            }));
            let get = |i: isize, c: usize| line[(i.clamp(0, len as isize - 1) as usize) * 4 + c] as u32;
            for c in 0..4 {
                let mut sum: u32 = (-(k as isize)..=k as isize).map(|i| get(i, c)).sum();
                for i in 0..len {
                    px[at(i, j) + c] = (sum / div) as u8;
                    sum += get(i as isize + k as isize + 1, c);
                    sum -= get(i as isize - k as isize, c);
                }
            }
        }
    }
}
