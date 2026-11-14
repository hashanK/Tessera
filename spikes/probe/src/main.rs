//! Feasibility probe. Not shipped. Answers three questions on the running machine:
//!   probe inspect        -> can we read desktop items + positions through IFolderView, and how fast?
//!   probe windows <secs> -> which window attachment strategy sits on the desktop layer and gets input?
//!   probe hide <secs>    -> can the native icon layer be hidden and restored from outside Explorer?
use std::io::Write;
use std::time::Instant;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::Common::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let secs = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(8u32);
    match args.get(1).map(String::as_str) {
        Some("inspect") => inspect(),
        Some("windows") => windows_test(secs),
        Some("hide") => hide_test(secs),
        _ => {
            println!("usage: probe inspect | windows <secs> | hide <secs>");
            Ok(())
        }
    }
}

fn inspect() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let t0 = Instant::now();
        let sw: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
        let loc = VARIANT::from(0i32); // CSIDL_DESKTOP
        let empty = VARIANT::default();
        let mut hwnd = 0i32;
        let disp = sw.FindWindowSW(&loc, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
        let sp: IServiceProvider = disp.cast()?;
        let browser: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser)?;
        let view = browser.QueryActiveShellView()?;
        let fv: IFolderView2 = view.cast()?;
        let folder: IShellFolder = fv.GetFolder()?;
        let t_bind = t0.elapsed();

        let mut mode = 0u32;
        let mut icon = 0i32;
        fv.GetViewModeAndIconSize(std::mem::transmute(&mut mode), &mut icon)?;
        let mut spacing = POINT::default();
        let _ = fv.GetSpacing(&mut spacing);
        println!(
            "view mode={mode} icon_size={icon} spacing={}x{} auto_arrange={:?}",
            spacing.x,
            spacing.y,
            fv.GetAutoArrange()
        );

        let t1 = Instant::now();
        let items: IEnumIDList = fv.Items(SVGIO_ALLVIEW)?;
        let mut n = 0;
        loop {
            let mut pidl = [std::ptr::null_mut(); 1];
            let mut fetched = 0u32;
            if items.Next(&mut pidl, Some(&mut fetched)) != S_OK || fetched == 0 {
                break;
            }
            let pt = fv.GetItemPosition(pidl[0])?;
            let mut sr = STRRET::default();
            folder.GetDisplayNameOf(pidl[0], SHGDN_NORMAL, &mut sr)?;
            let mut buf = [0u16; 260];
            StrRetToBufW(&mut sr, Some(pidl[0]), &mut buf)?;
            let name = String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)]);
            if n < 6 {
                println!("  ({:>5},{:>5}) {name}", pt.x, pt.y);
            }
            CoTaskMemFree(Some(pidl[0] as _));
            n += 1;
        }
        println!("items={n} bind={t_bind:?} enumerate+positions={:?}", t1.elapsed());
    }
    Ok(())
}

static mut LOG: Option<std::fs::File> = None;

fn log(s: &str) {
    println!("{s}");
    #[allow(static_mut_refs)]
    unsafe {
        if let Some(f) = LOG.as_mut() {
            let _ = writeln!(f, "{s}");
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MOUSEWHEEL => {
            let mut t = [0u16; 32];
            let n = GetWindowTextW(hwnd, &mut t);
            log(&format!("input msg=0x{msg:X} on {}", String::from_utf16_lossy(&t[..n as usize])));
            LRESULT(0)
        }
        WM_TIMER => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Paint a solid premultiplied colour into a layered window.
unsafe fn paint(hwnd: HWND, w: i32, h: i32, argb: u32) -> Result<()> {
    let screen = GetDC(None);
    let mem = CreateCompatibleDC(Some(screen));
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let dib = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
    std::slice::from_raw_parts_mut(bits as *mut u32, (w * h) as usize).fill(argb);
    let old = SelectObject(mem, dib.into());
    let size = SIZE { cx: w, cy: h };
    let src = POINT::default();
    let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 255, AlphaFormat: 1 };
    let r = UpdateLayeredWindow(hwnd, Some(screen), None, Some(&size), Some(mem), Some(&src), COLORREF(0), Some(&blend), ULW_ALPHA);
    SelectObject(mem, old);
    let _ = DeleteObject(dib.into());
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
    r
}

fn windows_test(secs: u32) -> Result<()> {
    unsafe {
        #[allow(static_mut_refs)]
        {
            LOG = std::fs::File::create(std::env::temp_dir().join("probe-windows.log")).ok();
        }
        let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
        let cls = w!("ProbeFence");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: cls,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        let progman = FindWindowW(w!("Progman"), None)?;
        let (w, h) = (300, 200);

        // A: cross-process child of Progman, placed above SHELLDLL_DefView.
        match CreateWindowExW(WS_EX_LAYERED | WS_EX_NOACTIVATE, cls, w!("child"), WS_CHILD | WS_VISIBLE, 200, 200, w, h, Some(progman), None, Some(hinst), None) {
            Ok(c) => {
                log(&format!("child created: paint={:?}", paint(c, w, h, 0xC8C800C8)));
                log(&format!("child z-top: {:?}", SetWindowPos(c, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)));
            }
            Err(e) => log(&format!("child FAILED: {e}")),
        }
        // B: top-level popup owned by Progman.
        match CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, cls, w!("owned"), WS_POPUP | WS_VISIBLE, 600, 200, w, h, Some(progman), None, Some(hinst), None) {
            Ok(o) => log(&format!("owned created: paint={:?}", paint(o, w, h, 0xC800C8C8))),
            Err(e) => log(&format!("owned FAILED: {e}")),
        }
        // C: unowned top-level popup pinned to the bottom of the z-order.
        match CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, cls, w!("bottom"), WS_POPUP | WS_VISIBLE, 1000, 200, w, h, None, None, Some(hinst), None) {
            Ok(b) => {
                log(&format!("bottom created: paint={:?}", paint(b, w, h, 0xC8C8C800)));
                let _ = SetWindowPos(b, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            Err(e) => log(&format!("bottom FAILED: {e}")),
        }

        let timer_host = CreateWindowExW(Default::default(), cls, w!("timer"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None)?;
        SetTimer(Some(timer_host), 1, secs * 1000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        log("done");
    }
    Ok(())
}

fn hide_test(secs: u32) -> Result<()> {
    unsafe {
        let progman = FindWindowW(w!("Progman"), None)?;
        let defview = FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), None)?;
        let list = FindWindowExW(Some(defview), None, w!("SysListView32"), None)?;
        println!("before: visible={}", IsWindowVisible(list).as_bool());
        let _ = ShowWindow(list, SW_HIDE);
        println!("hidden: visible={}", IsWindowVisible(list).as_bool());
        std::thread::sleep(std::time::Duration::from_secs(secs as u64));
        println!("after {secs}s: visible={} (false = Explorer did not undo it)", IsWindowVisible(list).as_bool());
        let _ = ShowWindow(list, SW_SHOWNA);
        println!("restored: visible={}", IsWindowVisible(list).as_bool());
    }
    Ok(())
}
