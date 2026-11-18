//! Tessera's installer and uninstaller, one small native executable.
//!
//! Installs per user (no administrator rights) into `%LOCALAPPDATA%\Programs\Tessera`:
//! the two programs, a copy of itself as `uninstall.exe`, Start menu shortcuts, an entry in
//! Settings → Apps, and optionally the same "start with Windows" value the settings window
//! toggles. Layouts in `%APPDATA%\Tessera` are left alone unless the user asks on uninstall.
//!
//! Command line: (none) install · `--silent [--autostart]` install without dialogs ·
//! `--uninstall [--silent]` remove.

#![windows_subsystem = "windows"]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use windows::core::{w, Interface, BOOL, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HINSTANCE, HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Shell::{FOLDERID_Programs, FOLDERID_UserProgramFiles, IShellLinkW, SHGetKnownFolderPath, ShellLink, KF_FLAG_CREATE};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId, PostMessageW, IDCANCEL, WM_CLOSE};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const AUTHOR: &str = env!("CARGO_PKG_AUTHORS");
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const TITLE: &str = "Tessera Setup";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Tessera";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const WEBVIEW2: &str = r"Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const PAYLOAD: [(&str, &[u8]); 2] = [
    ("tessera.exe", include_bytes!(env!("PAYLOAD_TESSERA"))),
    ("tessera-settings.exe", include_bytes!(env!("PAYLOAD_TESSERA_SETTINGS"))),
];

type R<T> = Result<T, String>;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).map(|a| a.to_lowercase()).collect();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let silent = has("--silent") || has("/s");
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let code = if has("--uninstall") { uninstall(silent) } else { install(silent, has("--autostart")) };
    std::process::exit(code);
}

// ───────────────────────────── install ─────────────────────────────

fn install(silent: bool, autostart_flag: bool) -> i32 {
    let dir = match install_dir() {
        Ok(d) => d,
        Err(e) => return fail(silent, &e),
    };
    let installed = reg_string(HKEY_CURRENT_USER, UNINSTALL_KEY, "DisplayVersion");
    let autostart_now = reg_string(HKEY_CURRENT_USER, RUN_KEY, "Tessera").is_some();

    let autostart = if silent {
        autostart_flag || autostart_now
    } else {
        let heading = match installed.as_deref() {
            Some(v) if v == VERSION => format!("Reinstall Tessera {VERSION}"),
            Some(v) => format!("Update Tessera {v} to {VERSION}"),
            None => format!("Install Tessera {VERSION}"),
        };
        let content = format!(
            "Tessera sorts your desktop icons into groups. It installs for your account only, so it doesn't need administrator rights.\n\nInstall folder: {}\n\nBy {AUTHOR}. Free and open source under the MIT License.",
            dir.display()
        );
        let footer = (!webview2_present()).then_some(
            "The settings window needs the Microsoft Edge WebView2 Runtime, which isn't on this PC. Tessera itself works without it.",
        );
        let check = if installed.is_some() { autostart_now } else { true };
        match dialog(Dialog {
            heading: &heading,
            content: &content,
            buttons: &[(100, "Install")],
            cancel: true,
            verify: Some(("Start Tessera when I sign in", check)),
            footer,
            ..Default::default()
        }) {
            (100, checked) => checked,
            _ => return 1,
        }
    };

    if let Err(e) = install_files(&dir, autostart) {
        return fail(silent, &e);
    }
    if !silent {
        let done = dialog(Dialog {
            heading: "Tessera is installed",
            content: "Find it in the Start menu. While it runs, Explorer's own desktop icons are hidden and Tessera shows them in groups; exit from its tray icon to get them back.",
            buttons: &[(101, "Start Tessera")],
            close: true,
            ..Default::default()
        });
        if done.0 == 101 {
            let _ = Command::new(dir.join("tessera.exe")).current_dir(&dir).spawn();
        }
    }
    0
}

fn install_files(dir: &Path, autostart: bool) -> R<()> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
    clear_leftovers(dir);
    // A running organiser would keep using the old build; it restores Explorer's icons as it exits.
    quit_organiser();
    stop_settings(dir);

    let mut bytes = 0u64;
    for (name, data) in PAYLOAD {
        write_exe(&dir.join(name), data)?;
        bytes += data.len() as u64;
    }
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let uninstaller = dir.join("uninstall.exe");
    if !same_file(&me, &uninstaller) {
        let data = std::fs::read(&me).map_err(|e| format!("Couldn't read the installer: {e}"))?;
        bytes += data.len() as u64;
        write_exe(&uninstaller, &data)?;
    }

    let programs = known_folder(&FOLDERID_Programs)?;
    shortcut(&programs.join("Tessera.lnk"), &dir.join("tessera.exe"), "Desktop icon groups")?;
    shortcut(&programs.join("Tessera Settings.lnk"), &dir.join("tessera-settings.exe"), "Tessera settings")?;

    let exe = dir.join("tessera.exe");
    let uninstall = format!("\"{}\" --uninstall", uninstaller.display());
    let values: [(&str, Value); 12] = [
        ("DisplayName", Value::Str("Tessera".into())),
        ("DisplayVersion", Value::Str(VERSION.into())),
        ("Publisher", Value::Str(AUTHOR.into())),
        ("URLInfoAbout", Value::Str(REPOSITORY.into())),
        ("HelpLink", Value::Str(REPOSITORY.into())),
        ("DisplayIcon", Value::Str(format!("{},0", exe.display()))),
        ("InstallLocation", Value::Str(dir.display().to_string())),
        ("UninstallString", Value::Str(uninstall.clone())),
        ("QuietUninstallString", Value::Str(format!("{uninstall} --silent"))),
        ("EstimatedSize", Value::Dword((bytes / 1024) as u32)),
        ("NoModify", Value::Dword(1)),
        ("NoRepair", Value::Dword(1)),
    ];
    reg_write(UNINSTALL_KEY, &values)?;
    // Same value and format the settings window writes, so its toggle reflects this choice.
    if autostart {
        reg_write(RUN_KEY, &[("Tessera", Value::Str(format!("\"{}\"", exe.display())))])?;
    } else {
        reg_delete_value(RUN_KEY, "Tessera");
    }
    Ok(())
}

/// Replace an executable even while it is running: a running image can't be overwritten or
/// deleted, but it can be renamed out of the way. The leftover goes on the next install.
fn write_exe(path: &Path, data: &[u8]) -> R<()> {
    if path.exists() && std::fs::remove_file(path).is_err() {
        let aside = path.with_extension(format!("old-{}", std::process::id()));
        std::fs::rename(path, &aside).map_err(|e| format!("{} is in use and couldn't be replaced: {e}", path.display()))?;
    }
    let tmp = path.with_extension("new");
    std::fs::write(&tmp, data).map_err(|e| format!("Couldn't write {}: {e}", path.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("Couldn't write {}: {e}", path.display()))
}

fn clear_leftovers(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if name.contains(".old-") || name.ends_with(".new") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

// ──────────────────────────── uninstall ────────────────────────────

fn uninstall(silent: bool) -> i32 {
    let dir = match reg_string(HKEY_CURRENT_USER, UNINSTALL_KEY, "InstallLocation") {
        Some(d) => PathBuf::from(d),
        None => match std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
            Some(d) => d,
            None => return fail(silent, "Couldn't find where Tessera is installed."),
        },
    };
    let delete_data = if silent {
        false
    } else {
        match dialog(Dialog {
            heading: "Uninstall Tessera?",
            content: "Tessera will close and Explorer's own desktop icons come back as they were. Your files are not touched.",
            buttons: &[(102, "Uninstall")],
            cancel: true,
            verify: Some(("Also delete my groups, profiles and settings", false)),
            ..Default::default()
        }) {
            (102, checked) => checked,
            _ => return 1,
        }
    };

    quit_organiser();
    stop_settings(&dir);
    reg_delete_value(RUN_KEY, "Tessera");
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(UNINSTALL_KEY));
    }
    if let Ok(programs) = known_folder(&FOLDERID_Programs) {
        let _ = std::fs::remove_file(programs.join("Tessera.lnk"));
        let _ = std::fs::remove_file(programs.join("Tessera Settings.lnk"));
    }
    for (name, _) in PAYLOAD {
        let _ = std::fs::remove_file(dir.join(name));
    }
    clear_leftovers(&dir);
    if delete_data {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let _ = std::fs::remove_dir_all(PathBuf::from(appdata).join("Tessera"));
        }
    }
    if !silent {
        dialog(Dialog { heading: "Tessera has been removed", close: true, ..Default::default() });
    }

    // This program can't delete itself while it runs. A hidden shell removes it, and then the
    // (now empty) folder, a moment after it exits.
    let me = std::env::current_exe().unwrap_or_default();
    if me.starts_with(&dir) {
        let script = format!("/c ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"", me.display(), dir.display());
        let _ = Command::new("cmd.exe").raw_arg(script).current_dir(std::env::temp_dir()).creation_flags(CREATE_NO_WINDOW).spawn();
    } else {
        let _ = std::fs::remove_dir(&dir);
    }
    0
}

// ───────────────────────────── processes ─────────────────────────────

/// Ask a running organiser to exit (as `tessera --quit` does) and wait for it, so it puts
/// Explorer's icons back and releases its files.
fn quit_organiser() {
    unsafe {
        let Ok(hwnd) = FindWindowW(w!("TesseraMain"), None) else { return };
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process = OpenProcess(PROCESS_SYNCHRONIZE, false, pid).ok();
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        match process {
            Some(h) => {
                WaitForSingleObject(h, 8000);
                let _ = CloseHandle(h);
            }
            None => std::thread::sleep(Duration::from_millis(1500)),
        }
    }
}

/// Close a settings window started from the install folder. It holds no unsaved state —
/// every change is applied by the organiser as it is made — so ending it is safe.
fn stop_settings(dir: &Path) {
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return };
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snapshot, &mut entry).is_ok();
        while ok {
            let name = String::from_utf16_lossy(&entry.szExeFile[..entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(0)]);
            if name.eq_ignore_ascii_case("tessera-settings.exe") {
                if let Ok(h) = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, false, entry.th32ProcessID) {
                    if image_path(h).is_some_and(|p| p.starts_with(dir)) {
                        let _ = TerminateProcess(h, 0);
                        WaitForSingleObject(h, 3000);
                    }
                    let _ = CloseHandle(h);
                }
            }
            ok = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
}

unsafe fn image_path(process: HANDLE) -> Option<PathBuf> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).ok()?;
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
}

// ──────────────────────────── shell, registry ────────────────────────────

fn install_dir() -> R<PathBuf> {
    Ok(known_folder(&FOLDERID_UserProgramFiles)?.join("Tessera"))
}

fn known_folder(id: &windows::core::GUID) -> R<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_CREATE, None).map_err(|e| format!("Couldn't find a system folder: {e}"))?;
        let path = PathBuf::from(p.to_string().unwrap_or_default());
        CoTaskMemFree(Some(p.0 as _));
        Ok(path)
    }
}

fn shortcut(link: &Path, target: &Path, description: &str) -> R<()> {
    let err = |e: windows::core::Error| format!("Couldn't create the Start menu shortcut: {e}");
    unsafe {
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(err)?;
        shell.SetPath(&HSTRING::from(target.as_os_str())).map_err(err)?;
        if let Some(dir) = target.parent() {
            shell.SetWorkingDirectory(&HSTRING::from(dir.as_os_str())).map_err(err)?;
        }
        shell.SetDescription(&HSTRING::from(description)).map_err(err)?;
        let file: IPersistFile = shell.cast().map_err(err)?;
        file.Save(&HSTRING::from(link.as_os_str()), true).map_err(err)
    }
}

enum Value {
    Str(String),
    Dword(u32),
}

fn reg_write(key: &str, values: &[(&str, Value)]) -> R<()> {
    unsafe {
        let mut hkey = HKEY::default();
        let status = RegCreateKeyExW(HKEY_CURRENT_USER, &HSTRING::from(key), None, PCWSTR::null(), REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut hkey, None);
        if status.is_err() {
            return Err(format!("Couldn't write to the registry ({key}): error {}", status.0));
        }
        let mut result = Ok(());
        for (name, value) in values {
            let status = match value {
                Value::Str(s) => {
                    let data: Vec<u8> = s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
                    RegSetValueExW(hkey, &HSTRING::from(*name), None, REG_SZ, Some(&data))
                }
                Value::Dword(d) => RegSetValueExW(hkey, &HSTRING::from(*name), None, REG_DWORD, Some(&d.to_le_bytes())),
            };
            if status.is_err() {
                result = Err(format!("Couldn't write {name} to the registry: error {}", status.0));
            }
        }
        let _ = RegCloseKey(hkey);
        result
    }
}

fn reg_delete_value(key: &str, name: &str) {
    unsafe {
        let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name));
    }
}

fn reg_string(root: HKEY, key: &str, name: &str) -> Option<String> {
    unsafe {
        let mut buf = [0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let status = RegGetValueW(root, &HSTRING::from(key), &HSTRING::from(name), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut size));
        if status.is_err() {
            return None;
        }
        let len = (size as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// WebView2's evergreen runtime registers its version per machine or per user.
fn webview2_present() -> bool {
    let machine = reg_string(HKEY_LOCAL_MACHINE, &format!(r"SOFTWARE\WOW6432Node\{WEBVIEW2}"), "pv");
    let user = reg_string(HKEY_CURRENT_USER, &format!(r"Software\{WEBVIEW2}"), "pv");
    [machine, user].into_iter().flatten().any(|v| !v.is_empty() && v != "0.0.0.0")
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

// ───────────────────────────── dialogs ─────────────────────────────

#[derive(Default)]
struct Dialog<'a> {
    heading: &'a str,
    content: &'a str,
    buttons: &'a [(i32, &'a str)],
    cancel: bool,
    close: bool,
    verify: Option<(&'a str, bool)>,
    footer: Option<&'a str>,
    error: bool,
}

/// A standard Windows task dialog. Returns the button pressed and the checkbox state.
fn dialog(d: Dialog) -> (i32, bool) {
    let title = HSTRING::from(TITLE);
    let heading = HSTRING::from(d.heading);
    let content = HSTRING::from(d.content);
    let labels: Vec<HSTRING> = d.buttons.iter().map(|(_, l)| HSTRING::from(*l)).collect();
    let buttons: Vec<TASKDIALOG_BUTTON> = d.buttons.iter().zip(&labels).map(|((id, _), l)| TASKDIALOG_BUTTON { nButtonID: *id, pszButtonText: PCWSTR(l.as_ptr()) }).collect();
    let verify = d.verify.map(|(t, _)| HSTRING::from(t));
    let footer = d.footer.map(HSTRING::from);

    let mut config = TASKDIALOGCONFIG { cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32, ..Default::default() };
    config.hInstance = unsafe { GetModuleHandleW(None).map(HINSTANCE::from).unwrap_or_default() };
    config.hwndParent = HWND::default();
    config.pszWindowTitle = PCWSTR(title.as_ptr());
    config.pszMainInstruction = PCWSTR(heading.as_ptr());
    if !d.content.is_empty() {
        config.pszContent = PCWSTR(content.as_ptr());
    }
    // Our own icon (resource 1), or the system error icon.
    config.Anonymous1.pszMainIcon = if d.error { TD_ERROR_ICON } else { PCWSTR(1 as _) };
    let mut flags = TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW;
    if let Some((_, checked)) = d.verify {
        config.pszVerificationText = PCWSTR(verify.as_ref().unwrap().as_ptr());
        if checked {
            flags |= TDF_VERIFICATION_FLAG_CHECKED;
        }
    }
    if let Some(f) = &footer {
        config.pszFooter = PCWSTR(f.as_ptr());
        config.Anonymous2.pszFooterIcon = TD_WARNING_ICON;
    }
    config.dwFlags = flags;
    let mut common = TASKDIALOG_COMMON_BUTTON_FLAGS(0);
    if d.cancel {
        common |= TDCBF_CANCEL_BUTTON;
    }
    if d.close || buttons.is_empty() && !d.cancel {
        common |= TDCBF_CLOSE_BUTTON;
    }
    config.dwCommonButtons = common;
    config.cButtons = buttons.len() as u32;
    config.pButtons = buttons.as_ptr();
    config.nDefaultButton = buttons.first().map(|b| b.nButtonID).unwrap_or(0);

    let (mut pressed, mut checked) = (IDCANCEL.0, BOOL(0));
    unsafe {
        if TaskDialogIndirect(&config, Some(&mut pressed), None, Some(&mut checked)).is_err() {
            return (IDCANCEL.0, false);
        }
    }
    (pressed, checked.as_bool())
}

fn fail(silent: bool, message: &str) -> i32 {
    if !silent {
        dialog(Dialog { heading: "Setup couldn't finish", content: message, close: true, error: true, ..Default::default() });
    }
    2
}
