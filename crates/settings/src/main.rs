//! Tessera's settings window. A separate program so the organiser itself never loads a web view:
//! this process, and its memory, exist only while the window is open.
//!
//! It never edits the layout file. Every change is a command sent to the running organiser
//! (see `tessera_core::ipc`), which applies it, saves, and is then read back.
//!
//! `tessera-settings --send <json>` sends one command and exits; handy for scripts.
#![windows_subsystem = "windows"]

mod assets;

use serde_json::{json, Value};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Icon, Theme, WindowBuilder};
use tessera_core::ipc::{COPYDATA_MAGIC, MAIN_WINDOW_CLASS};
use tessera_core::rules::Category;
use tao::platform::windows::{IconExtWindows, WindowExtWindows};
use tessera_core::{builtin_themes, store, Style, Tint};
use windows::core::HSTRING;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::*;
use wry::{WebContext, WebViewBuilder};

const TITLE: &str = "Tessera Settings";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn organiser() -> Option<HWND> {
    unsafe { FindWindowW(&HSTRING::from(MAIN_WINDOW_CLASS), None).ok() }
}

/// Deliver one JSON command to the organiser. True if it was accepted.
fn send(json: &str) -> bool {
    let Some(hwnd) = organiser() else { return false };
    let data = COPYDATASTRUCT { dwData: COPYDATA_MAGIC, cbData: json.len() as u32, lpData: json.as_ptr() as *mut _ };
    let mut result = 0usize;
    unsafe {
        // Some commands make the organiser open a dialog (the folder picker); let it come to the front.
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        // Timed, so a stuck organiser cannot hang this window.
        SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG, 3000, Some(&mut result));
    }
    result == 1
}

fn organiser_exe() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(|d| d.join("tessera.exe"))
}

fn reg(args: &[&str]) -> bool {
    Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output().map(|o| o.status.success()).unwrap_or(false)
}

fn autostart() -> bool {
    reg(&["query", RUN_KEY, "/v", "Tessera"])
}

fn set_autostart(on: bool) {
    match (on, organiser_exe()) {
        (true, Some(exe)) => reg(&["add", RUN_KEY, "/v", "Tessera", "/t", "REG_SZ", "/d", &format!("\"{}\"", exe.display()), "/f"]),
        _ => reg(&["delete", RUN_KEY, "/v", "Tessera", "/f"]),
    };
}

/// The program's own icon (resource 1, embedded from assets/tessera.ico).
fn window_icon() -> Option<Icon> {
    Icon::from_resource(1, None).ok()
}

/// Everything the page needs to draw itself. `error` is shown once as a toast.
fn state(error: Option<&str>) -> Value {
    let dir = store::data_dir();
    let config = store::load(&dir.join("layout.json")).and_then(|c| serde_json::to_value(c).ok());
    json!({
        "error": error,
        "running": organiser().is_some(),
        "autostart": autostart(),
        "version": env!("CARGO_PKG_VERSION"),
        "dataDir": dir.display().to_string(),
        "config": config,
        "tints": Tint::ALL.iter().map(|t| json!({ "name": t.name(), "rgb": t.rgb() })).collect::<Vec<_>>(),
        "categories": Category::ALL.iter().map(|c| c.title()).collect::<Vec<_>>(),
        "themes": builtin_themes(),
        "defaultLook": Style::default(),
        "fonts": fonts(),
        "icon": format!("data:image/png;base64,{}", assets::base64(include_bytes!("../../../assets/icon-64.png"))),
    })
}

/// Installed fonts, listed once per run (enumerating them takes a moment).
fn fonts() -> &'static [String] {
    static FONTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    FONTS.get_or_init(assets::fonts)
}

/// What to do after a message from the page.
#[derive(Default)]
struct Reply {
    /// Shown once as a toast.
    notice: Option<String>,
    /// Don't send fresh state back: a live preview on the desktop, sent many times a second
    /// while a slider moves; redrawing the page under the user's hands would fight them.
    quiet: bool,
    /// Script to run in the page.
    script: Option<String>,
}

impl Reply {
    fn notice(text: impl Into<String>) -> Reply {
        Reply { notice: Some(text.into()), ..Reply::default() }
    }
}

/// A theme file: `{"tessera_theme": 1, "name": …, "style": {…}}`.
fn export_theme(owner: HWND, msg: &Value) -> Reply {
    let name = msg["name"].as_str().unwrap_or("Theme");
    let Ok(style) = serde_json::from_value::<Style>(msg["style"].clone()) else { return Reply::notice("That theme couldn't be read") };
    let Some(path) = assets::file_dialog(owner, true, &format!("{name}.json")) else { return Reply { quiet: true, ..Reply::default() } };
    let file = json!({ "tessera_theme": 1, "name": name, "style": style });
    match std::fs::write(&path, serde_json::to_string_pretty(&file).unwrap_or_default()) {
        Ok(()) => Reply::notice(format!("Exported \u{201c}{name}\u{201d}")),
        Err(e) => Reply::notice(format!("Couldn't save the file: {e}")),
    }
}

/// Read a theme file and save it as a custom theme. Any missing setting takes its default and
/// the organiser clamps every value, so a hand-edited or older file is safe to import.
fn import_theme(owner: HWND) -> Reply {
    let Some(path) = assets::file_dialog(owner, false, "") else { return Reply { quiet: true, ..Reply::default() } };
    let text = match std::fs::metadata(&path) {
        Ok(m) if m.len() > 256 * 1024 => return Reply::notice("That file is too large to be a theme"),
        _ => std::fs::read_to_string(&path).unwrap_or_default(),
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else { return Reply::notice("That file isn't a Tessera theme") };
    let style_value = if value.get("style").is_some() { value["style"].clone() } else { value.clone() };
    let style = match serde_json::from_value::<Style>(style_value) {
        Ok(s) => s,
        Err(e) => return Reply::notice(format!("That theme couldn't be read: {e}")),
    };
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Imported".into());
    let mut name: String = value["name"].as_str().map(str::to_string).unwrap_or(stem).trim().chars().take(30).collect();
    if name.is_empty() || builtin_themes().iter().any(|t| t.name.eq_ignore_ascii_case(&name)) {
        name = format!("{name} (imported)").trim().to_string();
    }
    if organiser().is_none() {
        return Reply::notice("Tessera isn't running, so the theme couldn't be saved");
    }
    let cmd = json!({ "cmd": "theme_save", "name": name, "style": style });
    if send(&cmd.to_string()) {
        Reply::notice(format!("Imported \u{201c}{name}\u{201d}"))
    } else {
        Reply::notice("Tessera didn't accept that theme")
    }
}

/// Handle a message from the page. Local actions are done here; everything else is forwarded.
fn handle(body: &str, owner: HWND) -> Reply {
    let Ok(msg) = serde_json::from_str::<Value>(body) else { return Reply::default() };
    let quiet = msg["quiet"].as_bool().unwrap_or(false);
    match msg["cmd"].as_str() {
        Some("wallpaper") => {
            let url = assets::wallpaper_data_url();
            return Reply { quiet: true, script: Some(format!("window.setWallpaper({})", json!(url))), ..Reply::default() };
        }
        Some("export_theme") => return export_theme(owner, &msg),
        Some("import_theme") => return import_theme(owner),
        _ => {}
    }
    let notice = handle_command(&msg, body);
    Reply { notice: notice.map(str::to_string), quiet: quiet && notice.is_none(), script: None }
}

/// Returns a message for the user if the change could not be made.
fn handle_command(msg: &Value, body: &str) -> Option<&'static str> {
    match msg["cmd"].as_str() {
        Some("load") => {}
        Some("autostart") => set_autostart(msg["value"].as_bool().unwrap_or(false)),
        Some("start") => {
            if let Some(exe) = organiser_exe() {
                let _ = Command::new(exe).spawn();
                // Give it a moment to create its window so the refreshed state shows it running.
                for _ in 0..40 {
                    if organiser().is_some() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        Some("open_data") => {
            let _ = Command::new("explorer").arg(store::data_dir()).spawn();
        }
        Some(_) if organiser().is_none() => return Some("Tessera isn't running, so nothing was changed"),
        Some(_) => {
            if !send(body) {
                return Some("Tessera didn't accept that change");
            }
        }
        None => {}
    }
    None
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--send") {
        let ok = args.get(2).is_some_and(|json| send(json));
        std::process::exit(if ok { 0 } else { 1 });
    }
    unsafe {
        if let Ok(existing) = FindWindowW(None, &HSTRING::from(TITLE)) {
            let _ = ShowWindow(existing, SW_RESTORE);
            let _ = SetForegroundWindow(existing);
            return;
        }
    }

    let event_loop = EventLoopBuilder::<String>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title(TITLE)
        .with_inner_size(LogicalSize::new(960.0, 680.0))
        .with_min_inner_size(LogicalSize::new(640.0, 460.0))
        .with_window_icon(window_icon())
        .build(&event_loop)
        .expect("window");
    // WebView2 needs a writable profile folder; next to the exe is not writable once installed.
    let mut context = WebContext::new(Some(store::data_dir().join("webview")));
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        // Painted before the page loads, so a dark-mode window never flashes white.
        .with_background_color(if window.theme() == Theme::Dark { (0x15, 0x17, 0x1B, 255) } else { (0xF4, 0xF5, 0xF7, 255) })
        .with_html(include_str!("../ui/index.html"))
        .with_ipc_handler(move |request| {
            let _ = proxy.send_event(request.body().clone());
        })
        .build(&window)
        .expect("webview");

    let owner = HWND(window.hwnd() as *mut _);
    let run = move |script: &str| {
        let _ = webview.evaluate_script(script);
    };
    let push = move |error: Option<&str>| format!("window.render({})", state(error));
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *control_flow = ControlFlow::Exit,
            // The organiser may have changed things while we were in the background.
            Event::WindowEvent { event: WindowEvent::Focused(true), .. } => run(&push(None)),
            Event::UserEvent(body) => {
                let reply = handle(&body, owner);
                if let Some(script) = &reply.script {
                    run(script);
                }
                if !reply.quiet {
                    run(&push(reply.notice.as_deref()));
                }
            }
            _ => {}
        }
    });
}
