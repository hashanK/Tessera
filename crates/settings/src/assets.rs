//! What the appearance editor needs from Windows: the installed font families, the wallpaper
//! (for an honest preview of wallpaper materials), and file dialogs for theme files.
use std::ffi::c_void;
use std::path::PathBuf;
use windows::core::{w, BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Installed font family names (English names where a font has several), sorted.
pub fn fonts() -> Vec<String> {
    unsafe {
        let Ok(factory) = DWriteCreateFactory::<IDWriteFactory>(DWRITE_FACTORY_TYPE_SHARED) else { return Vec::new() };
        let mut collection = None;
        if factory.GetSystemFontCollection(&mut collection, false).is_err() {
            return Vec::new();
        }
        let Some(collection) = collection else { return Vec::new() };
        let mut names = Vec::new();
        for i in 0..collection.GetFontFamilyCount() {
            let Ok(family) = collection.GetFontFamily(i) else { continue };
            let Ok(strings) = family.GetFamilyNames() else { continue };
            let (mut index, mut exists) = (0u32, BOOL(0));
            let _ = strings.FindLocaleName(w!("en-us"), &mut index, &mut exists);
            if !exists.as_bool() {
                index = 0;
            }
            let Ok(len) = strings.GetStringLength(index) else { continue };
            let mut buf = vec![0u16; len as usize + 1];
            if strings.GetString(index, &mut buf).is_ok() {
                let name = String::from_utf16_lossy(&buf[..len as usize]);
                // Vertical-writing variants are listed with a leading '@'.
                if !name.starts_with('@') {
                    names.push(name);
                }
            }
        }
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        names
    }
}

/// The wallpaper as a small BMP data URL (about 640 pixels wide), or `None`.
/// Same source as the organiser's capture: the window Explorer paints the wallpaper in.
pub fn wallpaper_data_url() -> Option<String> {
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
        let ok = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(2)).as_bool();
        let mut bmp = Vec::new();
        if ok {
            let full = std::slice::from_raw_parts(bits as *const u8, (fw * fh * 4) as usize);
            bmp = encode_bmp(full, fw, fh);
        }
        SelectObject(dc, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(dc);
        ok.then(|| format!("data:image/bmp;base64,{}", base64(&bmp)))
    }
}

/// Shrink BGRA pixels by a whole factor to roughly 640 wide (averaging blocks) and encode as a
/// 24-bit top-down BMP.
fn encode_bmp(full: &[u8], fw: i32, fh: i32) -> Vec<u8> {
    let k = (fw / 640).max(1);
    let (w, h) = (fw / k, fh / k);
    let row = ((w * 3 + 3) / 4 * 4) as usize;
    let mut bmp = Vec::with_capacity(54 + row * h as usize);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((54 + row * h as usize) as u32).to_le_bytes());
    bmp.extend_from_slice(&[0, 0, 0, 0]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&w.to_le_bytes());
    bmp.extend_from_slice(&(-h).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    let n = (k * k) as u32;
    for y in 0..h {
        let start = bmp.len();
        for x in 0..w {
            let mut sum = [0u32; 3];
            for dy in 0..k {
                let p = (((y * k + dy) * fw + x * k) * 4) as usize;
                for dx in 0..k as usize {
                    for c in 0..3 {
                        sum[c] += full[p + dx * 4 + c] as u32;
                    }
                }
            }
            bmp.extend_from_slice(&[(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8]);
        }
        bmp.resize(start + row, 0);
    }
    bmp
}

unsafe fn wallpaper_window() -> Option<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        let found = &mut *(lp.0 as *mut Option<HWND>);
        let mut class = [0u16; 32];
        let n = GetClassNameW(hwnd, &mut class) as usize;
        if String::from_utf16_lossy(&class[..n]) == "WorkerW" && IsWindowVisible(hwnd).as_bool() && FindWindowExW(Some(hwnd), None, w!("SHELLDLL_DefView"), None).is_err() {
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

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            out.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// A standard Save or Open dialog for theme files. Returns the chosen path.
pub fn file_dialog(owner: HWND, save: bool, suggested: &str) -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    // File names can't contain these; a theme name might.
    let clean: String = suggested.chars().map(|c| if "\\/:*?\"<>|".contains(c) { '-' } else { c }).collect();
    let name: Vec<u16> = clean.encode_utf16().take(200).collect();
    buf[..name.len()].copy_from_slice(&name);
    let filter: Vec<u16> = "Tessera theme (*.json)\0*.json\0All files\0*.*\0\0".encode_utf16().collect();
    let title = HSTRING::from(if save { "Export theme" } else { "Import theme" });
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        lpstrDefExt: w!("json"),
        Flags: if save { OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR } else { OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR },
        ..Default::default()
    };
    let ok = unsafe { if save { GetSaveFileNameW(&mut ofn) } else { GetOpenFileNameW(&mut ofn) } };
    if !ok.as_bool() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len])))
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_known_values() {
        assert_eq!(super::base64(b"Ma"), "TWE=");
        assert_eq!(super::base64(b"Man"), "TWFu");
        assert_eq!(super::base64(b"M"), "TQ==");
    }
}
