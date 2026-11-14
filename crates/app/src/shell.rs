//! Everything that talks to the Windows shell: the desktop's item list, icons,
//! context menus, launching, change notifications and the native icon layer.
use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::mpsc::{channel, Sender};
use tessera_core::Rect;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::{IDropSource, IDropTarget, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::Common::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const SFGAO_FOLDER: u32 = 0x2000_0000;
const SFGAO_STREAM: u32 = 0x0040_0000;

/// Owned absolute item ID list.
pub struct Pidl(*mut ITEMIDLIST);
unsafe impl Send for Pidl {}

impl Pidl {
    pub fn as_ptr(&self) -> *const ITEMIDLIST {
        self.0
    }
}

impl Clone for Pidl {
    fn clone(&self) -> Self {
        Pidl(unsafe { ILClone(self.0) })
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0 as *const c_void)) }
    }
}

pub struct Item {
    /// Shell parsing name: full path, or `::{CLSID}` for virtual items.
    pub key: String,
    pub name: String,
    pub is_folder: bool,
    pub pidl: Pidl,
}

/// The shell view Explorer uses for the desktop. This is the list the user actually sees.
unsafe fn desktop_view() -> Result<IFolderView2> {
    let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
    let loc = VARIANT::from(0i32); // CSIDL_DESKTOP
    let mut hwnd = 0i32;
    let disp = windows.FindWindowSW(&loc, &VARIANT::default(), SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
    let browser: IShellBrowser = disp.cast::<IServiceProvider>()?.QueryService(&SID_STopLevelBrowser)?;
    browser.QueryActiveShellView()?.cast()
}

unsafe fn display_name(folder: &IShellFolder, pidl: *const ITEMIDLIST, flags: SHGDNF) -> String {
    let mut sr = STRRET::default();
    let mut buf = [0u16; 1024];
    if folder.GetDisplayNameOf(pidl, flags, &mut sr).is_err() || StrRetToBufW(&mut sr, Some(pidl), &mut buf).is_err() {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..buf.iter().position(|c| *c == 0).unwrap_or(0)])
}

/// Every item currently on the desktop, as Explorer's own desktop view reports it.
pub fn enumerate() -> Result<Vec<Item>> {
    unsafe {
        let view = desktop_view()?;
        let folder: IShellFolder = view.GetFolder()?;
        let list: IEnumIDList = view.Items(SVGIO_ALLVIEW)?;
        let mut out = Vec::new();
        loop {
            let mut pidl = [std::ptr::null_mut(); 1];
            let mut fetched = 0u32;
            if list.Next(&mut pidl, Some(&mut fetched)) != S_OK || fetched == 0 {
                break;
            }
            let pidl = Pidl(pidl[0]);
            let key = display_name(&folder, pidl.as_ptr(), SHGDN_FORPARSING);
            if key.is_empty() {
                continue;
            }
            let mut attrs = SFGAO_FOLDER | SFGAO_STREAM;
            let is_folder = folder.GetAttributesOf(&[pidl.as_ptr()], &mut attrs).is_ok() && attrs & SFGAO_FOLDER != 0 && attrs & SFGAO_STREAM == 0;
            out.push(Item { name: display_name(&folder, pidl.as_ptr(), SHGDN_NORMAL), key, is_folder, pidl });
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------- folder portals

/// The contents of one folder, for the portal group `id`.
pub struct PortalListing {
    pub id: u32,
    pub path: String,
    pub items: Vec<Item>,
}

/// A portal shows at most this many items; beyond that a folder belongs in Explorer.
const PORTAL_LIMIT: usize = 1000;

unsafe fn list_folder(path: &str) -> Result<Vec<Item>> {
    let mut raw = std::ptr::null_mut();
    SHParseDisplayName(&HSTRING::from(path), None, &mut raw, 0, None)?;
    let folder_pidl = Pidl(raw);
    let folder: IShellFolder = SHBindToObject(None, folder_pidl.as_ptr(), None)?;
    let mut list = None;
    // Hidden items are left out unless Explorer is set to show them, as in any folder window.
    folder.EnumObjects(HWND::default(), (SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0) as u32, &mut list).ok()?;
    let Some(list) = list else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    while out.len() < PORTAL_LIMIT {
        let mut child = [std::ptr::null_mut(); 1];
        let mut fetched = 0u32;
        if list.Next(&mut child, Some(&mut fetched)) != S_OK || fetched == 0 {
            break;
        }
        let child = Pidl(child[0]);
        let key = display_name(&folder, child.as_ptr(), SHGDN_FORPARSING);
        if key.is_empty() {
            continue;
        }
        let mut attrs = SFGAO_FOLDER | SFGAO_STREAM;
        let is_folder = folder.GetAttributesOf(&[child.as_ptr()], &mut attrs).is_ok() && attrs & SFGAO_FOLDER != 0 && attrs & SFGAO_STREAM == 0;
        let name = display_name(&folder, child.as_ptr(), SHGDN_NORMAL);
        out.push(Item { key, name, is_folder, pidl: Pidl(ILCombine(Some(folder_pidl.as_ptr()), Some(child.as_ptr()))) });
    }
    Ok(out)
}

/// Folders can be slow (network shares, sleeping disks), so portals are listed off the UI thread.
/// Send (group id, folder path); the listing comes back to `notify` as `msg` with a `Box<PortalListing>`.
pub fn spawn_portal_worker(notify: HWND, msg: u32) -> Sender<(u32, String)> {
    let (tx, rx) = channel::<(u32, String)>();
    let target = notify.0 as isize;
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        while let Ok(first) = rx.recv() {
            // A burst of change notifications asks for the same folder many times; list each once.
            let mut wanted = vec![first];
            while let Ok(more) = rx.try_recv() {
                wanted.retain(|w| w.0 != more.0);
                wanted.push(more);
            }
            for (id, path) in wanted {
                let items = list_folder(&path).unwrap_or_else(|e| {
                    crate::log(&format!("portal {path}: {e}"));
                    Vec::new()
                });
                let raw = Box::into_raw(Box::new(PortalListing { id, path, items }));
                if PostMessageW(Some(HWND(target as *mut c_void)), msg, WPARAM(0), LPARAM(raw as isize)).is_err() {
                    drop(Box::from_raw(raw));
                }
            }
        }
    });
    tx
}

/// Let the user choose a folder. None if they cancel.
pub fn pick_folder(owner: HWND) -> Option<String> {
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dialog.SetTitle(w!("Choose a folder to show on the desktop"));
        dialog.Show(Some(owner)).ok()?;
        let raw = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const c_void));
        path
    }
}

/// Open a folder in an Explorer window.
pub fn open_path(path: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(path), None, None, SW_SHOWNORMAL);
    }
}

// ---------------------------------------------------------------- icons

pub struct IconReq {
    pub key: String,
    pub pidl: Pidl,
    pub size: i32,
}

/// Premultiplied BGRA pixels, top-down.
pub struct IconData {
    pub key: String,
    pub size: i32,
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

/// Icon extraction can hit slow disks and network shortcuts, so it never runs on the UI thread.
/// Results are posted to `notify` as `msg` with a `Box<IconData>` in LPARAM.
pub fn spawn_icon_worker(notify: HWND, msg: u32) -> Sender<IconReq> {
    let (tx, rx) = channel::<IconReq>();
    let target = notify.0 as isize;
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        for req in rx {
            if let Some(data) = load_icon(&req) {
                let raw = Box::into_raw(Box::new(data));
                if PostMessageW(Some(HWND(target as *mut c_void)), msg, WPARAM(0), LPARAM(raw as isize)).is_err() {
                    drop(Box::from_raw(raw));
                }
            }
        }
    });
    tx
}

unsafe fn load_icon(req: &IconReq) -> Option<IconData> {
    let factory: IShellItemImageFactory = SHCreateItemFromIDList(req.pidl.as_ptr()).ok()?;
    let hbm = factory.GetImage(SIZE { cx: req.size, cy: req.size }, SIIGBF_ICONONLY).ok()?;
    let mut bm = BITMAP::default();
    GetObjectW(hbm.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void));
    let (w, h) = (bm.bmWidth, bm.bmHeight);
    let mut out = None;
    if w > 0 && h > 0 {
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32, biWidth: w, biHeight: -h, biPlanes: 1, biBitCount: 32, ..Default::default() },
            ..Default::default()
        };
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        let dc = GetDC(None);
        let lines = GetDIBits(dc, hbm, 0, h as u32, Some(bgra.as_mut_ptr() as *mut c_void), &mut bmi, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        if lines == h {
            // Icons without an alpha channel come back with alpha 0 everywhere.
            if bgra.chunks_exact(4).all(|p| p[3] == 0) {
                bgra.chunks_exact_mut(4).for_each(|p| p[3] = 255);
            }
            // The shell returns straight alpha; Direct2D wants premultiplied. Drawing straight
            // alpha as premultiplied lights up every soft edge pixel, which shows as a bright,
            // jagged outline. A colour above its alpha can only occur in straight alpha, so an
            // image without one is taken as already premultiplied and left alone.
            if bgra.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]) {
                for p in bgra.chunks_exact_mut(4) {
                    let a = p[3] as u32;
                    for c in &mut p[..3] {
                        *c = ((*c as u32 * a + 127) / 255) as u8;
                    }
                }
            }
            out = Some(IconData { key: req.key.clone(), size: req.size, w: w as u32, h: h as u32, bgra });
        }
    }
    let _ = DeleteObject(hbm.into());
    out
}

// ---------------------------------------------------------------- verbs

/// Run the item's default verb, exactly as a double-click on the desktop would.
pub fn open(hwnd: HWND, pidl: &Pidl) {
    unsafe {
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_INVOKEIDLIST,
            hwnd,
            lpIDList: pidl.as_ptr() as *mut c_void,
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let _ = ShellExecuteExW(&mut info);
    }
}

/// Start an OLE drag of one item, carrying the shell's own data object, so every drop target
/// (Explorer folders, the Recycle Bin, editors, upload boxes, our own groups) sees a real file.
/// Blocks, pumping messages, until the drag ends.
pub fn drag_out(hwnd: HWND, pidl: &Pidl) -> Result<()> {
    unsafe {
        let mut child = std::ptr::null_mut();
        let parent: IShellFolder = SHBindToParent(pidl.as_ptr(), Some(&mut child))?;
        let data: IDataObject = parent.GetUIObjectOf(hwnd, &[child as *const ITEMIDLIST], None)?;
        // No custom drop source: the shell supplies the cursor feedback and the drag image.
        SHDoDragDrop(Some(hwnd), &data, None::<&IDropSource>, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK)?;
        Ok(())
    }
}

/// The user's and the shared Desktop directories, without trailing separators.
pub fn desktop_dirs() -> Vec<String> {
    unsafe {
        [FOLDERID_Desktop, FOLDERID_PublicDesktop]
            .iter()
            .filter_map(|id| {
                let raw = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok()?;
                let path = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0 as *const c_void));
                path
            })
            .collect()
    }
}

/// The shell drop target of one item: what makes dropping onto a folder move files into it,
/// and dropping onto the Recycle Bin delete them. None if the item does not accept drops.
pub fn item_drop_target(hwnd: HWND, pidl: &Pidl) -> Option<IDropTarget> {
    unsafe {
        let mut child = std::ptr::null_mut();
        let parent: IShellFolder = SHBindToParent(pidl.as_ptr(), Some(&mut child)).ok()?;
        parent.GetUIObjectOf(hwnd, &[child as *const ITEMIDLIST], None).ok()
    }
}

/// Write an Internet shortcut into `dir`, picking a free name. Returns the path created.
pub fn create_url_shortcut(dir: &str, file_name: &str, url: &str) -> Option<String> {
    let (stem, ext) = file_name.rsplit_once('.').unwrap_or((file_name, "url"));
    let path = (1..100).map(|n| if n == 1 { format!("{dir}\\{stem}.{ext}") } else { format!("{dir}\\{stem} ({n}).{ext}") }).find(|p| !std::path::Path::new(p).exists())?;
    std::fs::write(&path, format!("[InternetShortcut]\r\nURL={url}\r\n")).ok()?;
    Some(path)
}

/// Reports where each transferred file actually ended up (the user may have chosen
/// "keep both" in a conflict dialog), so it can be filed in the group it was dropped on.
#[implement(IFileOperationProgressSink)]
struct ArrivalSink {
    notify: isize,
    msg: u32,
    group: u32,
}

impl ArrivalSink {
    fn arrived(&self, hr: HRESULT, created: Ref<IShellItem>, folder: Ref<IShellItem>, new_name: &PCWSTR, original: Ref<IShellItem>) {
        if hr.is_err() {
            return;
        }
        unsafe {
            let text = |item: &IShellItem, form: SIGDN| -> Option<String> {
                let raw = item.GetDisplayName(form).ok()?;
                let s = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0 as *const c_void));
                s
            };
            let path = created.as_ref().and_then(|c| text(c, SIGDN_FILESYSPATH)).or_else(|| {
                let dir = text(folder.as_ref()?, SIGDN_FILESYSPATH)?;
                let name = if new_name.is_null() { text(original.as_ref()?, SIGDN_PARENTRELATIVEPARSING)? } else { new_name.to_string().ok()? };
                Some(format!("{dir}\\{name}"))
            });
            if let Some(path) = path {
                let raw = Box::into_raw(Box::new((path, self.group)));
                if PostMessageW(Some(HWND(self.notify as *mut c_void)), self.msg, WPARAM(0), LPARAM(raw as isize)).is_err() {
                    drop(Box::from_raw(raw));
                }
            }
        }
    }
}

#[allow(non_snake_case)]
impl IFileOperationProgressSink_Impl for ArrivalSink_Impl {
    fn PostMoveItem(&self, _: u32, item: Ref<IShellItem>, folder: Ref<IShellItem>, name: &PCWSTR, hr: HRESULT, created: Ref<IShellItem>) -> Result<()> {
        self.arrived(hr, created, folder, name, item);
        Ok(())
    }
    fn PostCopyItem(&self, _: u32, item: Ref<IShellItem>, folder: Ref<IShellItem>, name: &PCWSTR, hr: HRESULT, created: Ref<IShellItem>) -> Result<()> {
        self.arrived(hr, created, folder, name, item);
        Ok(())
    }
    fn StartOperations(&self) -> Result<()> { Ok(()) }
    fn FinishOperations(&self, _: HRESULT) -> Result<()> { Ok(()) }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> { Ok(()) }
    fn PostRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR, _: HRESULT, _: Ref<IShellItem>) -> Result<()> { Ok(()) }
    fn PreMoveItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> { Ok(()) }
    fn PreCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> { Ok(()) }
    fn PreDeleteItem(&self, _: u32, _: Ref<IShellItem>) -> Result<()> { Ok(()) }
    fn PostDeleteItem(&self, _: u32, _: Ref<IShellItem>, _: HRESULT, _: Ref<IShellItem>) -> Result<()> { Ok(()) }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> Result<()> { Ok(()) }
    fn PostNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR, _: &PCWSTR, _: u32, _: HRESULT, _: Ref<IShellItem>) -> Result<()> { Ok(()) }
    fn UpdateProgress(&self, _: u32, _: u32) -> Result<()> { Ok(()) }
    fn ResetTimer(&self) -> Result<()> { Ok(()) }
    fn PauseTimer(&self) -> Result<()> { Ok(()) }
    fn ResumeTimer(&self) -> Result<()> { Ok(()) }
}

/// Move or copy files into the user's Desktop folder with the shell's file-operation engine:
/// the same progress and conflict dialogs as Explorer, and Ctrl+Z in Explorer undoes it.
/// Runs on its own thread so a large copy never stalls the groups or the program dragged from.
/// Each file that arrives is reported to `notify` as `msg` with a `Box<(String, u32)>` (path, group).
/// `dest` is the folder to put them in; None means the Desktop.
pub fn transfer(files: Vec<String>, move_them: bool, dest: Option<String>, notify: HWND, msg: u32, group: u32) {
    let notify = notify.0 as isize;
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let run = || -> Result<()> {
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            op.SetOperationFlags(FOF_ALLOWUNDO)?;
            let sink: IFileOperationProgressSink = ArrivalSink { notify, msg, group }.into();
            op.Advise(&sink)?;
            let desktop: IShellItem = match &dest {
                Some(folder) => SHCreateItemFromParsingName(&HSTRING::from(folder), None)?,
                None => SHGetKnownFolderItem(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None)?,
            };
            for file in &files {
                let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(file), None)?;
                if move_them {
                    op.MoveItem(&item, &desktop, PCWSTR::null(), None)?;
                } else {
                    op.CopyItem(&item, &desktop, PCWSTR::null(), None)?;
                }
            }
            op.PerformOperations()
        };
        if let Err(e) = run() {
            crate::log(&format!("drop: file operation failed: {e}"));
        }
        CoUninitialize();
    });
}

thread_local! {
    /// The shell menu currently on screen; owner-drawn and submenu items need its message handler.
    static ACTIVE_MENU: RefCell<Option<IContextMenu2>> = const { RefCell::new(None) };
}

/// Forward menu messages to the shell menu being tracked. Returns true if it was handled.
pub fn menu_message(msg: u32, wp: WPARAM, lp: LPARAM) -> bool {
    let menu = ACTIVE_MENU.with(|m| m.borrow().clone());
    match menu {
        Some(menu) => unsafe { menu.HandleMenuMsg(msg, wp, lp).is_ok() },
        None => false,
    }
}

/// Run an item's own "delete" verb, exactly as Explorer does for the Delete key: to the Recycle
/// Bin (Explorer's confirmation and undo apply), or permanently with `permanent` (Shift+Delete).
pub fn delete(hwnd: HWND, pidl: &Pidl, permanent: bool) -> Result<()> {
    unsafe {
        let mut child = std::ptr::null_mut();
        let parent: IShellFolder = SHBindToParent(pidl.as_ptr(), Some(&mut child))?;
        let menu: IContextMenu = parent.GetUIObjectOf(hwnd, &[child as *const ITEMIDLIST], None)?;
        // Some handlers only accept a verb after building their menu.
        let popup = CreatePopupMenu()?;
        let _ = menu.QueryContextMenu(popup, 0, 1, 0x7FFF, CMF_NORMAL);
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            // CMIC_MASK_UNICODE, plus CMIC_MASK_SHIFT_DOWN for a permanent delete.
            fMask: 0x4000 | if permanent { 0x1000_0000 } else { 0 },
            hwnd,
            lpVerb: windows::core::s!("delete"),
            lpVerbW: w!("delete"),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let result = menu.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO);
        let _ = DestroyMenu(popup);
        result
    }
}

/// Show the real shell context menu for an item and run whatever the user picks.
pub fn context_menu(hwnd: HWND, pidl: &Pidl, pt: POINT) -> Result<()> {
    unsafe {
        let mut child = std::ptr::null_mut();
        let parent: IShellFolder = SHBindToParent(pidl.as_ptr(), Some(&mut child))?;
        let menu: IContextMenu = parent.GetUIObjectOf(hwnd, &[child as *const ITEMIDLIST], None)?;
        let popup = CreatePopupMenu()?;
        let _ = menu.QueryContextMenu(popup, 0, 1, 0x7FFF, CMF_NORMAL);
        ACTIVE_MENU.with(|m| *m.borrow_mut() = menu.cast().ok());
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenuEx(popup, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.x, pt.y, hwnd, None).0;
        ACTIVE_MENU.with(|m| *m.borrow_mut() = None);
        let result = if cmd > 0 {
            let verb = (cmd - 1) as usize;
            let info = CMINVOKECOMMANDINFOEX {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                fMask: 0x4000 | 0x2000_0000, // CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE
                hwnd,
                lpVerb: PCSTR(verb as *const u8),
                lpVerbW: PCWSTR(verb as *const u16),
                nShow: SW_SHOWNORMAL.0,
                ptInvoke: pt,
                ..Default::default()
            };
            menu.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO)
        } else {
            Ok(())
        };
        let _ = DestroyMenu(popup);
        result
    }
}

// ---------------------------------------------------------------- change notifications

/// Ask the shell to post `msg` to `hwnd` whenever the desktop's contents change. Event-driven; nothing polls.
/// `folders` are the folders shown by portals, watched the same way.
pub fn register_notify(hwnd: HWND, msg: u32, folders: &[String]) -> u32 {
    unsafe {
        // The merged desktop namespace plus both directories that feed it.
        let mut pidls: Vec<Pidl> = [0x0000, 0x0010, 0x0019].iter().filter_map(|csidl| SHGetSpecialFolderLocation(None, *csidl).ok().map(Pidl)).collect();
        for folder in folders {
            let mut raw = std::ptr::null_mut();
            if SHParseDisplayName(&HSTRING::from(folder.as_str()), None, &mut raw, 0, None).is_ok() {
                pidls.push(Pidl(raw));
            }
        }
        let entries: Vec<SHChangeNotifyEntry> = pidls.iter().map(|p| SHChangeNotifyEntry { pidl: p.as_ptr() as *mut ITEMIDLIST, fRecursive: false.into() }).collect();
        let events = SHCNE_CREATE | SHCNE_DELETE | SHCNE_MKDIR | SHCNE_RMDIR | SHCNE_RENAMEITEM | SHCNE_RENAMEFOLDER | SHCNE_UPDATEITEM | SHCNE_UPDATEDIR | SHCNE_ATTRIBUTES;
        SHChangeNotifyRegister(hwnd, SHCNRF_ShellLevel | SHCNRF_InterruptLevel | SHCNRF_NewDelivery, events.0 as i32, msg, entries.len() as i32, entries.as_ptr())
    }
}

pub fn unregister_notify(id: u32) {
    if id != 0 {
        unsafe {
            let _ = SHChangeNotifyDeregister(id);
        }
    }
}

unsafe fn parsing_name(pidl: *const ITEMIDLIST) -> Option<String> {
    let raw = SHGetNameFromIDList(pidl, SIGDN_DESKTOPABSOLUTEPARSING).ok()?;
    let name = raw.to_string().ok();
    CoTaskMemFree(Some(raw.0 as *const c_void));
    name
}

/// If the notification is a rename, return (old key, new key).
pub fn notify_rename(wp: WPARAM, lp: LPARAM) -> Option<(String, String)> {
    unsafe {
        let mut pidls: *mut *mut ITEMIDLIST = std::ptr::null_mut();
        let mut event = 0i32;
        let lock = SHChangeNotification_Lock(HANDLE(wp.0 as *mut c_void), lp.0 as u32, Some(&mut pidls), Some(&mut event));
        if lock.is_invalid() {
            return None;
        }
        let mut out = None;
        if event as u32 & (SHCNE_RENAMEITEM.0 | SHCNE_RENAMEFOLDER.0) != 0 && !pidls.is_null() {
            let (old, new) = (*pidls, *pidls.add(1));
            if !old.is_null() && !new.is_null() {
                out = parsing_name(old).zip(parsing_name(new));
            }
        }
        let _ = SHChangeNotification_Unlock(lock);
        out
    }
}

// ---------------------------------------------------------------- native icon layer

/// The top-level desktop window and the shell view inside it. Normally `Progman`; some builds
/// move the view under a `WorkerW` once a wallpaper host has been spawned.
unsafe fn desktop_host_and_view() -> Option<(HWND, HWND)> {
    let find_view = |parent: HWND| FindWindowExW(Some(parent), None, w!("SHELLDLL_DefView"), None).ok().map(|v| (parent, v));
    if let Some(found) = FindWindowW(w!("Progman"), None).ok().and_then(find_view) {
        return Some(found);
    }
    let mut worker = None;
    loop {
        worker = Some(FindWindowExW(None, worker, w!("WorkerW"), None).ok()?);
        if let Some(found) = worker.and_then(find_view) {
            return Some(found);
        }
    }
}

pub fn desktop_host() -> Option<HWND> {
    unsafe { desktop_host_and_view().map(|(host, _)| host) }
}

unsafe fn native_list() -> Option<HWND> {
    FindWindowExW(Some(desktop_host_and_view()?.1), None, w!("SysListView32"), None).ok()
}

/// Show or hide Explorer's own desktop icons. Nothing about them is modified.
/// Explorer's own icon layer is on screen (it would end up in a wallpaper capture).
pub fn native_icons_visible() -> bool {
    unsafe { native_list().is_some_and(|l| IsWindowVisible(l).as_bool()) }
}

pub fn show_native_icons(show: bool) {
    unsafe {
        if let Some(list) = native_list() {
            let _ = ShowWindow(list, if show { SW_SHOWNA } else { SW_HIDE });
        }
    }
}

// ---------------------------------------------------------------- displays

fn to_rect(r: RECT) -> Rect {
    Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
}

unsafe extern "system" fn collect_monitor(_: HMONITOR, _: HDC, rect: *mut RECT, data: LPARAM) -> BOOL {
    (*(data.0 as *mut Vec<RECT>)).push(*rect);
    true.into()
}

/// Identifies the current monitor arrangement, e.g. `1920x1080@0,0|2560x1440@1920,0`.
pub fn display_key() -> String {
    let mut rects: Vec<RECT> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(&mut rects as *mut _ as isize));
    }
    rects.sort_by_key(|r| (r.left, r.top));
    rects.iter().map(|r| format!("{}x{}@{},{}", r.right - r.left, r.bottom - r.top, r.left, r.top)).collect::<Vec<_>>().join("|")
}

pub fn primary_work_area() -> Rect {
    let mut r = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut r as *mut _ as *mut c_void), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
    to_rect(r)
}

pub fn virtual_screen() -> Rect {
    unsafe { Rect { x: GetSystemMetrics(SM_XVIRTUALSCREEN), y: GetSystemMetrics(SM_YVIRTUALSCREEN), w: GetSystemMetrics(SM_CXVIRTUALSCREEN), h: GetSystemMetrics(SM_CYVIRTUALSCREEN) } }
}

/// Work area of the monitor a rectangle is mostly on.
pub fn work_area_for(r: Rect) -> Rect {
    unsafe {
        let rect = RECT { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h };
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() {
            to_rect(info.rcWork)
        } else {
            primary_work_area()
        }
    }
}
