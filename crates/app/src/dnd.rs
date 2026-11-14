//! OLE drop target for a group window. One of these is registered per group.
//!
//! What a drop does depends on what is dragged and where it lands:
//! * onto a folder, the Recycle Bin or another system item in the group: the drag is handed to
//!   that item's own shell drop target, so it behaves exactly as on the native desktop;
//! * one of our own icons, between items: regroup or reorder; no file is touched;
//! * files from Explorer or another program: moved or copied to the Desktop folder by the shell's
//!   file-operation engine, then filed in this group;
//! * a link from a browser: an Internet shortcut is created on the Desktop and filed in this group.
use std::cell::RefCell;
use tessera_core::rules::is_web_url;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::{MK_CONTROL, MK_SHIFT, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::*;

const CF_UNICODETEXT: u16 = 13;
const CF_HDROP: u16 = 15;

#[derive(Default)]
enum Payload {
    #[default]
    Nothing,
    Files(Vec<String>),
    Link { url: String, title: String },
}

#[derive(Default)]
struct DragState {
    payload: Payload,
    data: Option<IDataObject>,
    /// The item under the pointer whose own drop target currently has the drag: (key, target).
    onto: Option<(String, IDropTarget)>,
}

#[implement(IDropTarget)]
pub struct GroupDropTarget {
    id: u32,
    hwnd: HWND,
    /// Draws the shell's drag image while the pointer is over this window.
    helper: Option<IDropTargetHelper>,
    state: RefCell<DragState>,
}

/// Copy the bytes of a global-memory clipboard format out of a data object.
unsafe fn global_bytes(data: &IDataObject, format: u16) -> Option<Vec<u8>> {
    let request = FORMATETC { cfFormat: format, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    let mut medium = data.GetData(&request).ok()?;
    let handle = medium.u.hGlobal;
    let ptr = GlobalLock(handle) as *const u8;
    let bytes = (!ptr.is_null()).then(|| std::slice::from_raw_parts(ptr, GlobalSize(handle)).to_vec());
    let _ = GlobalUnlock(handle);
    ReleaseStgMedium(&mut medium);
    bytes
}

fn utf16_until_nul(bytes: &[u8]) -> String {
    let wide: Vec<u16> = bytes.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).take_while(|c| *c != 0).collect();
    String::from_utf16_lossy(&wide)
}

/// What the drag carries that we can use: real files first, otherwise a web link.
unsafe fn read_payload(data: &IDataObject) -> Payload {
    let request = FORMATETC { cfFormat: CF_HDROP, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    if let Ok(mut medium) = data.GetData(&request) {
        let hdrop = HDROP(medium.u.hGlobal.0);
        let files: Vec<String> = (0..DragQueryFileW(hdrop, u32::MAX, None))
            .map(|i| {
                let mut buf = vec![0u16; DragQueryFileW(hdrop, i, None) as usize + 1];
                let len = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
                String::from_utf16_lossy(&buf[..len])
            })
            .collect();
        ReleaseStgMedium(&mut medium);
        if !files.is_empty() {
            return Payload::Files(files);
        }
    }
    let url = global_bytes(data, RegisterClipboardFormatW(CFSTR_INETURLW) as u16)
        .or_else(|| global_bytes(data, CF_UNICODETEXT))
        .map(|b| utf16_until_nul(&b).trim().to_string())
        .filter(|u| is_web_url(u));
    let Some(url) = url else { return Payload::Nothing };
    // Browsers describe the link as a virtual "<page title>.url" file; its name is the best title we get.
    let title = global_bytes(data, RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW) as u16)
        .filter(|b| b.len() >= std::mem::size_of::<FILEGROUPDESCRIPTORW>())
        .map(|b| {
            let offset = std::mem::offset_of!(FILEGROUPDESCRIPTORW, fgd) + std::mem::offset_of!(FILEDESCRIPTORW, cFileName);
            utf16_until_nul(&b[offset..])
        })
        .unwrap_or_default();
    Payload::Link { url, title }
}

impl GroupDropTarget {
    pub fn register(id: u32, hwnd: HWND) {
        unsafe {
            let helper = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok();
            let target: IDropTarget = GroupDropTarget { id, hwnd, helper, state: RefCell::default() }.into();
            if let Err(e) = RegisterDragDrop(hwnd, &target) {
                crate::log(&format!("RegisterDragDrop failed: {e}"));
            }
        }
    }

    /// What dropping between items here would do, within what the source allows. `to` is the
    /// group (tab) the drop would go to.
    fn choose(&self, to: u32, keys: MODIFIERKEYS_FLAGS, allowed: DROPEFFECT) -> DROPEFFECT {
        let Some(app) = crate::app() else { return DROPEFFECT_NONE };
        if app.own_regroup(to) {
            return DROPEFFECT_MOVE;
        }
        let first_allowed = |order: [DROPEFFECT; 3]| order.into_iter().find(|e| allowed.contains(*e)).unwrap_or(DROPEFFECT_NONE);
        match &self.state.borrow().payload {
            Payload::Nothing => DROPEFFECT_NONE,
            Payload::Link { .. } => first_allowed([DROPEFFECT_LINK, DROPEFFECT_COPY, DROPEFFECT_NONE]),
            // Same rules as dropping on the real desktop: move within a drive, copy across drives,
            // Ctrl forces copy, Shift forces move.
            Payload::Files(files) => {
                let want = if keys.contains(MK_CONTROL) {
                    DROPEFFECT_COPY
                } else if keys.contains(MK_SHIFT) || files.iter().all(|f| app.moves_by_default(to, f)) {
                    DROPEFFECT_MOVE
                } else {
                    DROPEFFECT_COPY
                };
                first_allowed([want, DROPEFFECT_COPY, DROPEFFECT_MOVE])
            }
        }
    }

    /// Stop forwarding to an item's drop target, if we were.
    fn leave_item(&self) {
        if let Some((_, target)) = self.state.borrow_mut().onto.take() {
            unsafe {
                let _ = target.DragLeave();
            }
        }
    }

    /// Work out what is under the pointer and what a drop would do; update the on-screen feedback.
    fn track(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> DROPEFFECT {
        let Some(app) = crate::app() else { return DROPEFFECT_NONE };
        let point = POINT { x: pt.x, y: pt.y };
        let allowed = unsafe { *effect };
        if let Some((index, key, pidl)) = app.drop_item_at(self.id, point) {
            let same = self.state.borrow().onto.as_ref().is_some_and(|(k, _)| *k == key);
            let mut inner = allowed;
            let accepted = unsafe {
                if same {
                    let target = self.state.borrow().onto.as_ref().map(|(_, t)| t.clone());
                    target.is_some_and(|t| t.DragOver(keys, *pt, &mut inner).is_ok())
                } else {
                    self.leave_item();
                    let data = self.state.borrow().data.clone();
                    match (crate::shell::item_drop_target(self.hwnd, &pidl), data) {
                        (Some(target), Some(data)) if target.DragEnter(&data, keys, *pt, &mut inner).is_ok() => {
                            self.state.borrow_mut().onto = Some((key, target));
                            true
                        }
                        _ => false,
                    }
                }
            };
            if accepted && inner != DROPEFFECT_NONE {
                app.drag_onto(self.id, Some(index));
                unsafe { *effect = inner };
                return inner;
            }
        }
        self.leave_item();
        let chosen = self.choose(app.drop_fence_at(self.id, point), keys, allowed);
        unsafe { *effect = chosen };
        app.drag_onto(self.id, None);
        app.drag_over((chosen != DROPEFFECT_NONE).then_some(self.id), point);
        chosen
    }

    fn clear_feedback(&self) {
        if let Some(app) = crate::app() {
            app.drag_onto(self.id, None);
            app.drag_over(None, POINT::default());
        }
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for GroupDropTarget_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let data = data.ok()?;
        *self.state.borrow_mut() = DragState { payload: unsafe { read_payload(data) }, data: Some(data.clone()), onto: None };
        let chosen = self.track(keys, pt, effect);
        if let Some(helper) = &self.helper {
            unsafe {
                let _ = helper.DragEnter(self.hwnd, data, &POINT { x: pt.x, y: pt.y }, chosen);
            }
        }
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let chosen = self.track(keys, pt, effect);
        if let Some(helper) = &self.helper {
            unsafe {
                let _ = helper.DragOver(&POINT { x: pt.x, y: pt.y }, chosen);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        self.leave_item();
        *self.state.borrow_mut() = DragState::default();
        self.clear_feedback();
        if let Some(helper) = &self.helper {
            unsafe {
                let _ = helper.DragLeave();
            }
        }
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let allowed = unsafe { *effect };
        let chosen = self.track(keys, pt, effect);
        let point = POINT { x: pt.x, y: pt.y };
        let state = std::mem::take(&mut *self.state.borrow_mut());
        if let (Some(helper), Ok(data)) = (&self.helper, data.ok()) {
            unsafe {
                let _ = helper.Drop(data, &point, chosen);
            }
        }
        let mut report = DROPEFFECT_NONE;
        if let (Some((_, target)), Ok(data)) = (&state.onto, data.ok()) {
            // Dropped onto a folder, the Recycle Bin, ...: that item's own handler does the work and reports the effect.
            report = allowed;
            unsafe {
                if let Err(e) = target.Drop(data, keys, *pt, &mut report) {
                    crate::log(&format!("drop onto item failed: {e}"));
                    report = DROPEFFECT_NONE;
                }
            }
        } else if let Some(app) = crate::app().filter(|_| chosen != DROPEFFECT_NONE) {
            let to = app.drop_fence_at(self.id, point);
            if app.own_regroup(to) {
                app.drop_own_item(to, point);
            } else {
                match state.payload {
                    Payload::Files(files) => {
                        app.drop_files(to, point, files, chosen == DROPEFFECT_MOVE);
                        // A copy is reported as such. A move is done entirely by us, so the source is told
                        // "nothing for you to do" and must not delete the originals itself.
                        if chosen == DROPEFFECT_COPY {
                            report = DROPEFFECT_COPY;
                        }
                    }
                    Payload::Link { url, title } => {
                        app.drop_link(to, &url, &title);
                        report = chosen;
                    }
                    Payload::Nothing => {}
                }
            }
        }
        self.clear_feedback();
        unsafe { *effect = report };
        Ok(())
    }
}
