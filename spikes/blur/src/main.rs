use std::ffi::c_void;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[repr(C)]
struct AccentPolicy { state: u32, flags: u32, gradient: u32, animation: u32 }
#[repr(C)]
struct CompositionData { attribute: u32, data: *mut c_void, size: usize }

unsafe extern "system" fn proc_(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT { DefWindowProcW(h, m, w, l) }

/// A layered window whose content is a rounded rectangle at low alpha (like a translucent group).
unsafe fn layered(x: i32, y: i32, w: i32, h: i32, alpha: u8) -> HWND {
    let hwnd = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("BlurSpike"), w!("spike"), WS_POPUP | WS_VISIBLE, x, y, w, h, None, None, None, None).unwrap();
    let screen = GetDC(None);
    let dc = CreateCompatibleDC(Some(screen));
    let bmi = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: 40, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() }, ..Default::default() };
    let mut bits = std::ptr::null_mut();
    let dib = CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
    SelectObject(dc, dib.into());
    let px = std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize);
    let r = 16;
    for yy in 0..h { for xx in 0..w {
        let dx = (r - xx).max(xx - (w - 1 - r)).max(0); let dy = (r - yy).max(yy - (h - 1 - r)).max(0);
        if dx * dx + dy * dy <= r * r { let a = alpha as u32; let c = a * 0x30 / 255; px[(yy * w + xx) as usize] = a << 24 | c << 16 | c << 8 | c; }
    }}
    let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
    let _ = UpdateLayeredWindow(hwnd, None, None, Some(&SIZE { cx: w, cy: h }), Some(dc), Some(&POINT::default()), COLORREF(0), Some(&blend), ULW_ALPHA);
    ReleaseDC(None, screen);
    hwnd
}

unsafe fn accent(hwnd: HWND, state: u32) -> bool {
    let user = GetModuleHandleW(w!("user32.dll")).unwrap();
    let Some(f) = GetProcAddress(user, s!("SetWindowCompositionAttribute")) else { return false };
    let f: unsafe extern "system" fn(HWND, *mut CompositionData) -> BOOL = std::mem::transmute(f);
    let mut policy = AccentPolicy { state, flags: 2, gradient: 0x30202020, animation: 0 };
    let mut data = CompositionData { attribute: 19, data: &mut policy as *mut _ as *mut c_void, size: std::mem::size_of::<AccentPolicy>() };
    f(hwnd, &mut data).as_bool()
}

fn main() {
    unsafe {
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc_), lpszClassName: w!("BlurSpike"), ..Default::default() });
        let (x, y) = (std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(1300), 300);
        let a = layered(x, y, 180, 140, 60);
        println!("accent blur (3): {}", accent(a, 3));
        let b = layered(x, y + 160, 180, 140, 60);
        println!("accent acrylic (4): {}", accent(b, 4));
        let c = layered(x, y + 320, 180, 140, 60);
        let kind: i32 = 3; // DWMSBT_TRANSIENTWINDOW (acrylic)
        println!("system backdrop: {:?}", DwmSetWindowAttribute(c, DWMWA_SYSTEMBACKDROP_TYPE, &kind as *const _ as *const c_void, 4));
        let start = std::time::Instant::now();
        let mut msg = MSG::default();
        while start.elapsed().as_secs() < 5 {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() { DispatchMessageW(&msg); }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }
}
