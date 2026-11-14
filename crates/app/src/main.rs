//! Tessera: desktop icon groups for Windows.
//!
//! One UI thread owns everything. Group windows are unowned, bottom-pinned layered windows
//! (see docs/architecture.md, ADR-2). The process is idle unless the user or the shell does something.
#![windows_subsystem = "windows"]

mod backdrop;
mod dnd;
mod render;
mod shell;

use render::{FenceView, ItemView, Renderer};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use tessera_core::ipc::{Command, Effect, COPYDATA_MAGIC};
use tessera_core::layout::{self, Metrics};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use tessera_core::{store, Config, HoverMotion, Material, Present, Rect, SelectMotion, Shadow, Sort, Style, Theme, Tint};
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, RevokeDragDrop};
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const MAIN_CLASS: PCWSTR = w!("TesseraMain");
const FENCE_CLASS: PCWSTR = w!("TesseraFence");

const WM_TRAY: u32 = WM_APP + 1;
const WM_ICON_READY: u32 = WM_APP + 2;
const WM_SHELL_CHANGE: u32 = WM_APP + 3;
const WM_DROP_ARRIVED: u32 = WM_APP + 4;
const WM_PORTAL_READY: u32 = WM_APP + 5;
const WM_PICK_PORTAL: u32 = WM_APP + 6;
/// Sent by a second launch of tessera.exe: the user opened Tessera while it was already running.
const WM_OPEN_SETTINGS: u32 = WM_APP + 7;
/// A click landed outside every group while an item was selected.
const WM_CLICK_OUTSIDE: u32 = WM_APP + 8;
/// Delete or Enter pressed on the desktop while an item is selected (wParam: key, lParam: Shift).
const WM_ITEM_KEY: u32 = WM_APP + 9;
/// Hotkey ids HOTKEY_PROFILE+1 ..= +9 switch to the first nine profiles (Ctrl+Alt+Shift+digit).
const HOTKEY_PROFILE: i32 = 10;

const TIMER_SAVE: usize = 1;
const TIMER_REFRESH: usize = 2;
const TIMER_REFRESH_LATE: usize = 3;
const TIMER_PAINT: usize = 4;
const TIMER_ZCHECK: usize = 5;
const TIMER_ZCHECK_LATE: usize = 6;
/// Re-capture the wallpaper a moment after it changed (Explorer fades the new one in).
const TIMER_BACKDROP: usize = 7;
/// One animation frame. Armed only while a hover or selection animation is running.
const TIMER_ANIM: usize = 8;

const WM_MOUSELEAVE: u32 = 0x02A3;
const EM_SETSEL: u32 = 0x00B1;

const HOTKEY_HIDE: i32 = 1;
const HOTKEY_PEEK: i32 = 2;

const EDGE_L: u8 = 1;
const EDGE_R: u8 = 2;
const EDGE_T: u8 = 4;
const EDGE_B: u8 = 8;

/// While set, groups float above application windows instead of being pinned to the desktop.
static PEEK: AtomicBool = AtomicBool::new(false);
static mut APP: *mut App = std::ptr::null_mut();
static mut EDIT_PROC: WNDPROC = None;

/// The single App lives on the UI thread for the life of the process.
fn app() -> Option<&'static mut App> {
    unsafe { APP.as_mut() }
}

#[derive(Default)]
struct Ui {
    scroll: i32,
    hover: Option<usize>,
    selected: Option<usize>,
    insert: Option<usize>,
    hot: bool,
    tracking: bool,
    /// For a portal browsed into a subfolder: the folder currently shown.
    nav: Option<String>,
    /// Where each tab sits in the title bar, as last painted: (x, width).
    tabs: Vec<(i32, i32)>,
    tab_hover: Option<usize>,
    /// Shadow room around the group inside its window, as last placed.
    margin: i32,
    /// Hover and selection animations in progress, by item position.
    anim: HashMap<usize, Anim>,
    /// The selection as of the last paint, to notice a new one.
    last_selected: Option<usize>,
}

/// Progress of one item's hover and selection animations, each 0..1, and of the one-off
/// ripple or bounce when it was selected (1 = finished).
#[derive(Clone, Copy)]
struct Anim {
    hover: f32,
    hover_to: f32,
    select: f32,
    select_to: f32,
    burst: f32,
}

/// Point each item's animation at what the pointer and selection now say, and start a burst for
/// a new selection. Without motion (or with animations off) everything snaps to its target.
/// Returns whether anything still has to move.
fn step_targets(ui: &mut Ui, hover: HoverMotion, select: SelectMotion, allowed: bool) -> bool {
    let (hovered, selected) = (ui.hover, ui.selected);
    let fresh = selected.filter(|s| ui.last_selected != Some(*s));
    ui.last_selected = selected;
    let mut keys: Vec<usize> = ui.anim.keys().copied().collect();
    keys.extend(hovered);
    keys.extend(selected);
    let mut moving = false;
    for i in keys {
        let a = ui.anim.entry(i).or_insert(Anim { hover: 0.0, hover_to: 0.0, select: 0.0, select_to: 0.0, burst: 1.0 });
        a.hover_to = if hovered == Some(i) { 1.0 } else { 0.0 };
        a.select_to = if selected == Some(i) { 1.0 } else { 0.0 };
        if !allowed || hover == HoverMotion::None {
            a.hover = a.hover_to;
        }
        if !allowed || select == SelectMotion::None {
            a.select = a.select_to;
        }
        if fresh == Some(i) && allowed && select.burst_seconds() > 0.0 {
            a.burst = 0.0;
        }
        if !allowed {
            a.burst = 1.0;
        }
        moving |= a.hover != a.hover_to || a.select != a.select_to || a.burst < 1.0;
    }
    ui.anim.retain(|_, a| a.hover > 0.0 || a.select > 0.0 || a.hover_to > 0.0 || a.select_to > 0.0 || a.burst < 1.0);
    moving
}

/// Move `value` towards `target` by `step`.
fn approach(value: f32, target: f32, step: f32) -> f32 {
    if value < target {
        (value + step).min(target)
    } else {
        (value - step).max(target)
    }
}

enum Zone {
    Edge(u8),
    /// The back chevron of a portal that has been browsed into a subfolder.
    Back,
    /// A tab in the title bar of a tabbed group, by position.
    Tab(usize),
    Title,
    Item(usize),
    Body,
}

enum Drag {
    None,
    Move { id: u32, dx: i32, dy: i32 },
    Resize { id: u32, edges: u8, start: Rect, px: i32, py: i32 },
    /// Button is down on an item; becomes `Item` once the pointer travels far enough.
    Pending { id: u32, index: usize, px: i32, py: i32 },
}

struct App {
    cfg: Config,
    path: PathBuf,
    items: HashMap<String, shell::Item>,
    renderer: Renderer,
    wins: Vec<(u32, HWND)>,
    ui: HashMap<u32, Ui>,
    main: HWND,
    hinst: HINSTANCE,
    icon_tx: Sender<shell::IconReq>,
    drag: Drag,
    hidden: bool,
    /// The last wallpaper capture failed; wait for a wallpaper or display change before retrying.
    backdrop_failed: bool,
    /// Explorer's icons are hidden and have had time to disappear: the wallpaper may be captured.
    backdrop_ready: bool,
    /// Windows with an animation running, and when the last frame was drawn.
    animating: HashSet<u32>,
    anim_clock: Option<std::time::Instant>,
    display: String,
    notify_id: u32,
    tray_icon: HICON,
    repaint: HashSet<u32>,
    rename: Option<(u32, HWND, HFONT)>,
    /// The item being renamed in place (F2), when `rename` is over an icon rather than a title.
    rename_item: Option<String>,
    /// A new folder to select and rename as soon as it appears (Ctrl+Shift+N).
    pending_rename: Option<String>,
    /// Letters typed to jump to an icon, and when the last one came.
    typeahead: (String, Option<std::time::Instant>),
    /// (source group, item key) while one of our own icons is in an OLE drag.
    own_drag: Option<(u32, String)>,
    /// Files being brought onto the desktop by a drop: lower-cased destination path -> group to file it in.
    pending_drops: HashMap<String, u32>,
    desktop_dirs: Vec<String>,
    portal_tx: Sender<(u32, String)>,
    /// Keys in `items` that belong to portal listings rather than to the desktop.
    portal_keys: HashSet<String>,
}

/// The desktop's items as the model sees them; portal listings are not desktop items.
fn desktop_present<'a>(items: &'a HashMap<String, shell::Item>, portal_keys: &HashSet<String>) -> Vec<Present<'a>> {
    items.values().filter(|i| !portal_keys.contains(&i.key)).map(|i| Present { key: &i.key, name: &i.name, is_folder: i.is_folder }).collect()
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('\\').map(|(dir, _)| dir).unwrap_or("")
}

fn is_ours(hwnd: HWND) -> bool {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid == std::process::id()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn lo(lp: LPARAM) -> i32 {
    (lp.0 & 0xFFFF) as i16 as i32
}

fn hi(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xFFFF) as i16 as i32
}

fn cursor() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

fn data_dir() -> PathBuf {
    store::data_dir()
}

fn log(msg: &str) {
    use std::io::Write;
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("tessera.log")) {
        let _ = writeln!(f, "{msg}");
    }
}

impl App {
    // Windows belong to tab hosts. Any group id may be passed to these helpers: a tab is mapped
    // to the window it lives in.

    fn hwnd(&self, id: u32) -> Option<HWND> {
        let id = self.cfg.tab_host(id);
        self.wins.iter().find(|w| w.0 == id).map(|w| w.1)
    }

    fn metrics(&self, id: u32) -> Metrics {
        let dpi = self.hwnd(id).map(|h| unsafe { GetDpiForWindow(h) }).filter(|d| *d > 0).unwrap_or(96);
        Metrics::new(&self.style_of(id), dpi)
    }

    /// The look a window is drawn with: its host's style, minus the effects low-effects mode
    /// turns off (wallpaper blur becomes plain translucency; no grain, no shadow).
    fn style_of(&self, id: u32) -> Style {
        let mut style = self.cfg.fence(self.cfg.tab_host(id)).map(|f| f.style.clone()).unwrap_or_default();
        if self.cfg.low_effects {
            if style.material.uses_backdrop() {
                style.material = Material::Transparent;
                style.opacity = style.opacity.max(75);
            }
            style.noise = 0;
            style.shadow = Shadow::default();
        }
        style
    }

    /// The window's rectangle: the group's, grown on every side by its shadow room.
    fn window_rect(&self, id: u32) -> Rect {
        let r = self.shown_rect(id);
        let m = self.metrics(id).margin;
        Rect { x: r.x - m, y: r.y - m, w: r.w + 2 * m, h: r.h + 2 * m }
    }

    fn uses_backdrop(&self, id: u32) -> bool {
        self.style_of(id).material.uses_backdrop()
    }

    /// Capture the wallpaper the first time a group needs it.
    fn ensure_backdrop(&mut self) {
        // Not before Explorer's icons are hidden and it has repainted the wallpaper under them,
        // or the capture would contain the old icon layout.
        if self.renderer.backdrop.is_some() || self.backdrop_failed || !self.backdrop_ready {
            return;
        }
        if shell::native_icons_visible() {
            self.backdrop_ready = false;
            unsafe {
                SetTimer(Some(self.main), TIMER_BACKDROP, 600, None);
            }
            return;
        }
        if !self.cfg.hosts().iter().any(|id| self.uses_backdrop(*id)) {
            return;
        }
        self.renderer.backdrop = backdrop::Backdrop::capture();
        match &self.renderer.backdrop {
            Some(b) => log(&format!("wallpaper captured: {}x{} ({} KB)", b.w, b.h, b.bytes() / 1024)),
            None => {
                // Don't retry on every paint; the next wallpaper or display change tries again.
                self.backdrop_failed = true;
                log("wallpaper capture failed; wallpaper materials fall back to plain translucency");
            }
        }
    }

    /// Animations follow the Windows "Animation effects" setting, and low-effects mode.
    fn motion_allowed(&self) -> bool {
        if self.cfg.low_effects {
            return false;
        }
        let mut on = BOOL(1);
        unsafe {
            let _ = SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some(&mut on as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
        }
        on.as_bool()
    }

    /// Something in window `id` started moving: run the frame timer until it stops.
    fn start_animation(&mut self, id: u32) {
        self.animating.insert(id);
        if self.anim_clock.is_none() {
            self.anim_clock = Some(std::time::Instant::now());
            unsafe {
                SetTimer(Some(self.main), TIMER_ANIM, 15, None);
            }
        }
    }

    /// One animation frame: advance every running animation by the time since the last frame,
    /// redraw those windows, and keep the timer only while something is still moving. Idle
    /// costs nothing; a hover lasts about a fifth of a second.
    fn animation_frame(&mut self) {
        let now = std::time::Instant::now();
        let dt = self.anim_clock.map(|t| (now - t).as_secs_f32()).unwrap_or(0.0).min(0.05);
        self.anim_clock = None;
        for id in std::mem::take(&mut self.animating) {
            let icons = self.style_of(id).icons;
            let (hover_in, select) = (icons.hover_motion.seconds().max(0.01), icons.select_motion.seconds().max(0.01));
            let burst = icons.select_motion.burst_seconds().max(0.01);
            if let Some(ui) = self.ui.get_mut(&id) {
                for a in ui.anim.values_mut() {
                    // Leaving is a little slower than arriving, which reads as calmer.
                    let hover_step = dt / if a.hover_to > a.hover { hover_in } else { hover_in * 1.4 };
                    a.hover = approach(a.hover, a.hover_to, hover_step);
                    a.select = approach(a.select, a.select_to, dt / select);
                    a.burst = (a.burst + dt / burst).min(1.0);
                }
            }
            // Painting re-checks the targets and re-arms the timer while anything still moves.
            self.paint(id);
        }
    }

    /// The wallpaper (or the screen layout) changed: capture it again and repaint the groups
    /// that show it.
    fn refresh_backdrop(&mut self) {
        self.backdrop_ready = true;
        self.renderer.backdrop = None;
        self.backdrop_failed = false;
        for id in self.cfg.hosts() {
            if self.uses_backdrop(id) {
                self.paint(id);
            }
        }
    }

    /// On-screen rectangle: the stored one, or just the title bar when rolled up.
    fn shown_rect(&self, id: u32) -> Rect {
        let Some(f) = self.cfg.fence(self.cfg.tab_host(id)) else { return Rect::default() };
        let mut r = f.rect;
        if f.collapsed {
            r.h = self.metrics(id).title_h;
        }
        r
    }

    /// The group whose icons the window `id` belongs to is showing right now.
    fn shown(&self, id: u32) -> Option<&tessera_core::Fence> {
        self.cfg.fence(self.cfg.shown_tab(id))
    }

    fn paint(&mut self, id: u32) {
        let id = self.cfg.tab_host(id);
        self.ensure_backdrop();
        let (Some(hwnd), Some(host), Some(fence)) = (self.hwnd(id), self.cfg.fence(id), self.cfg.fence(self.cfg.shown_tab(id))) else { return };
        let style = self.style_of(id);
        let order = self.cfg.tab_order(id);
        let m = self.metrics(id);
        let shown = self.shown_rect(id);
        let motion = (style.icons.hover_motion, style.icons.select_motion, self.motion_allowed());
        let ui = self.ui.entry(id).or_default();
        ui.scroll = ui.scroll.clamp(0, layout::max_scroll(&m, &layout::grid(&m, shown.w, fence.items.len()), shown.h));
        let animating = step_targets(ui, motion.0, motion.1, motion.2);
        // A portal browsed into a subfolder is titled with that folder's name.
        let nav = ui.nav.as_deref().filter(|_| fence.is_portal());
        let title = nav.map(|p| p.rsplit('\\').next().unwrap_or(p)).unwrap_or(&fence.title);
        let view = FenceView {
            w: shown.w,
            h: shown.h,
            m,
            back: nav.is_some(),
            title,
            style: &style,
            theme: self.cfg.theme,
            collapsed: host.collapsed,
            items: fence.items.iter().map(|k| ItemView { key: k, name: self.items.get(k).map(|i| i.name.as_str()).unwrap_or("") }).collect(),
            scroll: ui.scroll,
            hover: ui.hover,
            selected: ui.selected,
            insert: ui.insert,
            hot: ui.hot,
            tabs: if order.len() > 1 { order.iter().filter_map(|t| self.cfg.fence(*t).map(|f| f.title.as_str())).collect() } else { Vec::new() },
            active_tab: order.iter().position(|t| *t == fence.id).unwrap_or(0),
            tab_hover: ui.tab_hover,
            tab_style: host.tab_style,
            origin: (shown.x, shown.y),
            anim: ui.anim.iter().map(|(i, a)| (*i, (a.hover, a.select, a.burst))).collect(),
        };
        let painted = self.renderer.paint_fence(hwnd, &view);
        for index in painted.missing {
            let key = &fence.items[index];
            if let Some(item) = self.items.get(key) {
                if self.renderer.pending.insert(key.clone()) {
                    let _ = self.icon_tx.send(shell::IconReq { key: key.clone(), pidl: item.pidl.clone(), size: m.icon });
                }
            }
        }
        self.ui.entry(id).or_default().tabs = painted.tabs;
        if animating {
            self.start_animation(id);
        }
    }

    /// Bring the tab at position `index` of window `id` to the front.
    fn switch_tab(&mut self, id: u32, index: usize) {
        let Some(tab) = self.cfg.tab_order(id).get(index).copied() else { return };
        let before = self.cfg.shown_tab(id);
        if !self.cfg.select_tab(tab) {
            return;
        }
        let host = self.cfg.tab_host(id);
        let ui = self.ui.entry(host).or_default();
        let was_browsing = ui.nav.take().is_some();
        (ui.scroll, ui.hover, ui.selected, ui.insert) = (0, None, None, None);
        if was_browsing {
            // The tab we left was browsed into a subfolder; it starts from its own folder next time.
            self.refresh_portals(Some(before));
            self.watch_shell();
        }
        self.paint(host);
        self.dirty();
    }

    /// Make the set of group windows match the groups that have windows (after tabs were combined,
    /// split, added or reordered), and redraw them all.
    fn sync_windows(&mut self) {
        let hosts = self.cfg.hosts();
        for id in self.wins.iter().map(|w| w.0).filter(|id| !hosts.contains(id)).collect::<Vec<_>>() {
            self.destroy_window(id);
        }
        for id in hosts {
            if self.wins.iter().any(|w| w.0 == id) {
                self.place(id);
            } else {
                self.create_window(id);
            }
        }
    }

    fn paint_all(&mut self) {
        for id in self.wins.iter().map(|w| w.0).collect::<Vec<_>>() {
            self.paint(id);
        }
    }

    /// Coalesce repaints caused by bursts of background results.
    fn paint_soon(&mut self, id: u32) {
        self.repaint.insert(id);
        unsafe {
            SetTimer(Some(self.main), TIMER_PAINT, 30, None);
        }
    }

    /// Move/resize the window to match the model, then redraw it.
    fn place(&mut self, id: u32) {
        let (Some(hwnd), r) = (self.hwnd(id), self.window_rect(id)) else { return };
        unsafe {
            let _ = SetWindowPos(hwnd, None, r.x, r.y, r.w, r.h, SWP_NOACTIVATE | SWP_NOZORDER);
        }
        let margin = self.metrics(id).margin;
        self.ui.entry(self.cfg.tab_host(id)).or_default().margin = margin;
        self.paint(id);
    }

    fn create_window(&mut self, id: u32) {
        let r = self.window_rect(id);
        unsafe {
            let ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
            let Ok(hwnd) = CreateWindowExW(ex, FENCE_CLASS, w!("Tessera group"), WS_POPUP, r.x, r.y, r.w, r.h, None, None, Some(self.hinst), None) else {
                log("could not create a group window");
                return;
            };
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, id as isize);
            dnd::GroupDropTarget::register(id, hwnd);
            self.wins.push((id, hwnd));
            self.ui.insert(id, Ui { margin: self.metrics(id).margin, ..Ui::default() });
            self.paint(id);
            if !self.hidden {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }

    fn destroy_window(&mut self, id: u32) {
        if let Some(pos) = self.wins.iter().position(|w| w.0 == id) {
            let (_, hwnd) = self.wins.remove(pos);
            self.ui.remove(&id);
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = RevokeDragDrop(hwnd);
                let _ = DestroyWindow(hwnd);
            }
        }
    }

    /// Something changed that must survive a restart; write it shortly, once.
    fn dirty(&mut self) {
        self.cfg.store_placement(&self.display);
        unsafe {
            SetTimer(Some(self.main), TIMER_SAVE, 600, None);
        }
    }

    fn save(&mut self) {
        if let Err(e) = store::save(&self.path, &self.cfg) {
            log(&format!("save failed: {e}"));
        }
    }

    /// Re-read the desktop and bring groups in line with it.
    fn refresh(&mut self) {
        let items = match shell::enumerate() {
            Ok(items) => items,
            Err(e) => {
                log(&format!("desktop enumeration failed: {e}"));
                return;
            }
        };
        let renamed: HashSet<String> = items.iter().filter(|i| self.items.get(&i.key).is_some_and(|old| old.name != i.name)).map(|i| i.key.clone()).collect();
        for key in self.items.keys().filter(|k| !self.portal_keys.contains(*k) && !items.iter().any(|i| &i.key == *k)).cloned().collect::<Vec<_>>() {
            self.renderer.forget_icon(&key);
        }
        // Replace the desktop's items; portal listings are refreshed separately and carried over.
        let mut old = std::mem::take(&mut self.items);
        self.items = items.into_iter().map(|i| (i.key.clone(), i)).collect();
        for key in &self.portal_keys {
            if let Some(item) = old.remove(key) {
                self.items.entry(key.clone()).or_insert(item);
            }
        }
        let present = desktop_present(&self.items, &self.portal_keys);
        let mut changed = self.cfg.reconcile(&present, shell::primary_work_area());
        changed.extend(self.cfg.fences.iter().filter(|f| f.items.iter().any(|k| renamed.contains(k))).map(|f| f.id));
        drop(present);
        // Files that arrived because they were dropped on a group go to that group, not where the rules would put them.
        let arrived: Vec<(String, u32)> = self.items.keys().filter_map(|k| self.pending_drops.remove(&k.to_lowercase()).map(|to| (k.clone(), to))).collect();
        for (key, to) in arrived {
            let from = self.cfg.fences.iter().find(|f| f.items.contains(&key)).map(|f| f.id);
            if from != Some(to) && self.cfg.move_item(&key, to, usize::MAX) {
                changed.extend(from);
                changed.push(to);
            }
        }
        if changed.is_empty() {
            self.rename_if_arrived();
            return;
        }
        for id in changed {
            if self.hwnd(id).is_none() {
                self.create_window(self.cfg.tab_host(id));
            } else {
                self.paint(id);
            }
        }
        self.dirty();
        self.rename_if_arrived();
    }

    fn zone(&self, id: u32, x: i32, y: i32) -> Zone {
        let (Some(host), Some(f), m, r) = (self.cfg.fence(self.cfg.tab_host(id)), self.shown(id), self.metrics(id), self.shown_rect(id)) else { return Zone::Body };
        let mut edges = 0;
        if x < m.grip {
            edges |= EDGE_L;
        } else if x >= r.w - m.grip {
            edges |= EDGE_R;
        }
        if !host.collapsed {
            if y < m.grip {
                edges |= EDGE_T;
            } else if y >= r.h - m.grip {
                edges |= EDGE_B;
            }
        }
        if edges != 0 {
            return Zone::Edge(edges);
        }
        if y < m.title_h {
            let ui = self.ui.get(&host.id);
            if f.is_portal() && ui.is_some_and(|u| u.nav.is_some()) && x < m.px(30.0) {
                return Zone::Back;
            }
            // The gaps between tabs (and a lone title) move the group.
            return match ui.and_then(|u| u.tabs.iter().position(|(tx, tw)| x >= *tx && x < tx + tw)) {
                Some(i) => Zone::Tab(i),
                None => Zone::Title,
            };
        }
        let scroll = self.ui.get(&id).map(|u| u.scroll).unwrap_or(0);
        match layout::hit(&m, &layout::grid(&m, r.w, f.items.len()), x, y, scroll, f.items.len()) {
            Some(i) => Zone::Item(i),
            None => Zone::Body,
        }
    }

    /// Snap a moving group to the work-area margin and to its neighbours, then keep it on screen.
    fn snap(&self, id: u32, mut r: Rect) -> Rect {
        let area = shell::work_area_for(r);
        let m = self.metrics(id);
        let (gap, reach) = (m.px(8.0), m.px(10.0));
        let mut xs = vec![area.x + gap, area.x + area.w - gap - r.w];
        let mut ys = vec![area.y + gap, area.y + area.h - gap - r.h];
        for other in self.wins.iter().filter(|w| w.0 != id).map(|w| self.shown_rect(w.0)) {
            xs.extend([other.x, other.x + other.w + gap, other.x - gap - r.w, other.x + other.w - r.w]);
            ys.extend([other.y, other.y + other.h + gap, other.y - gap - r.h, other.y + other.h - r.h]);
        }
        let nearest = |v: i32, candidates: &[i32]| candidates.iter().copied().filter(|c| (c - v).abs() <= reach).min_by_key(|c| (c - v).abs()).unwrap_or(v);
        r.x = nearest(r.x, &xs);
        r.y = nearest(r.y, &ys);
        r.clamp_into(area)
    }

    /// Group (a tab, for tabbed windows) and insertion index under a screen point, for an item
    /// being dragged. Over a tab's label it is that tab, at the end.
    fn drop_target(&self, pt: POINT) -> Option<(u32, usize)> {
        self.wins.iter().find_map(|(id, _)| {
            let r = self.shown_rect(*id);
            let host = self.cfg.fence(*id)?;
            if !r.contains(pt.x, pt.y) {
                return None;
            }
            let m = self.metrics(*id);
            if pt.y - r.y < m.title_h {
                let (x, ui) = (pt.x - r.x, self.ui.get(id));
                if let Some(i) = ui.and_then(|u| u.tabs.iter().position(|(tx, tw)| x >= *tx && x < tx + tw)) {
                    let tab = *self.cfg.tab_order(*id).get(i)?;
                    return Some((tab, self.cfg.fence(tab)?.items.len()));
                }
            }
            let f = self.shown(*id)?;
            if host.collapsed {
                return Some((f.id, f.items.len()));
            }
            let scroll = self.ui.get(id).map(|u| u.scroll).unwrap_or(0);
            Some((f.id, layout::insert_index(&m, &layout::grid(&m, r.w, f.items.len()), pt.x - r.x, pt.y - r.y, scroll, f.items.len())))
        })
    }

    /// The group a drop at `pt` on window `id` goes to: a tab whose label is under the pointer,
    /// otherwise the tab on screen.
    pub fn drop_fence_at(&self, id: u32, pt: POINT) -> u32 {
        let host = self.cfg.tab_host(id);
        self.drop_target(pt).map(|t| t.0).filter(|t| self.cfg.tab_host(*t) == host).unwrap_or_else(|| self.cfg.shown_tab(host))
    }

    fn set_insert(&mut self, target: Option<(u32, usize)>) {
        for id in self.wins.iter().map(|w| w.0).collect::<Vec<_>>() {
            let want = target.filter(|t| t.0 == id).map(|t| t.1);
            let ui = self.ui.entry(id).or_default();
            if ui.insert != want {
                ui.insert = want;
                self.paint(id);
            }
        }
    }

    /// Select one item (or none). Only one item is selected across all groups.
    fn select(&mut self, id: u32, index: Option<usize>) {
        let id = self.cfg.tab_host(id);
        for other in self.wins.iter().map(|w| w.0).collect::<Vec<_>>() {
            let want = if other == id { index } else { None };
            let ui = self.ui.entry(other).or_default();
            if ui.selected != want {
                ui.selected = want;
                self.paint(other);
            }
        }
        watch_outside_clicks(self.main, index.is_some());
    }

    /// Delete / Shift+Delete / Enter pressed on the desktop while an item is selected.
    /// The selected item: (window, position in the tab on screen, item key).
    fn selected_item(&self) -> Option<(u32, usize, String)> {
        let (host, index) = self.ui.iter().find_map(|(id, ui)| ui.selected.map(|i| (*id, i)))?;
        let key = self.cfg.fence(self.cfg.shown_tab(host))?.items.get(index)?.clone();
        Some((host, index, key))
    }

    /// Items of the tab on screen in window `host`.
    fn shown_items(&self, host: u32) -> Vec<String> {
        self.cfg.fence(self.cfg.shown_tab(host)).map(|f| f.items.clone()).unwrap_or_default()
    }

    /// A key pressed while the desktop is the active window (see `desktop_key` hook).
    fn desktop_key(&mut self, key: VIRTUAL_KEY, mods: isize) {
        self.desktop_key_inner(key, mods);
        // Diagnostics for testing keyboard handling: set TESSERA_DEBUG_KEYS=1.
        if std::env::var_os("TESSERA_DEBUG_KEYS").is_some() {
            let sel = self.selected_item().map(|(h, i, k)| format!("{h}:{i} {}", k.rsplit(std::path::MAIN_SEPARATOR).next().unwrap_or(&k)));
            log(&format!("key {:#x} mods {mods} -> {}", key.0, sel.unwrap_or_else(|| "nothing selected".into())));
        }
    }

    fn desktop_key_inner(&mut self, key: VIRTUAL_KEY, mods: isize) {
        let (shift, alt) = (mods & MOD_SHIFT_KEY != 0, mods & MOD_ALT_KEY != 0);
        let ctrl = mods & MOD_CTRL_KEY != 0;
        match key {
            VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_HOME | VK_END | VK_PRIOR | VK_NEXT => return self.navigate(key),
            VK_ESCAPE => return self.clear_selection(),
            VK_F5 => {
                unsafe {
                    SetTimer(Some(self.main), TIMER_REFRESH, 10, None);
                }
                return;
            }
            _ if ctrl && shift && key.0 == 0x4E => return self.new_folder(),
            _ if ctrl && key.0 == 0x56 => return self.paste(),
            _ if !ctrl && !alt && !matches!(key, VK_DELETE | VK_RETURN | VK_F2 | VK_APPS | VK_F10) => {
                let ch = char::from_u32(key.0 as u32).unwrap_or(' ').to_ascii_lowercase();
                return self.type_ahead(ch);
            }
            _ => {}
        }
        let Some((host, index, item_key)) = self.selected_item() else { return };
        let (Some(item), Some(hwnd)) = (self.items.get(&item_key), self.hwnd(host)) else { return };
        let pidl = item.pidl.clone();
        // System items (This PC, Recycle Bin, Network…) aren't files: no delete, cut or copy.
        let is_file = !item_key.starts_with("::");
        let verb = |verb: &str, shift: bool| {
            if let Err(e) = shell::invoke_verb(hwnd, &pidl, verb, shift) {
                log(&format!("{verb} failed: {e}"));
            }
        };
        match key {
            VK_RETURN if alt => verb("properties", false),
            VK_RETURN => shell::open(hwnd, &pidl),
            VK_DELETE if is_file => {
                self.clear_selection();
                verb("delete", shift);
            }
            VK_F2 => self.start_item_rename(host, index),
            VK_APPS | VK_F10 => {
                let c = self.item_screen_rect(host, index);
                shell::take_foreground(hwnd);
                if let Err(e) = shell::context_menu(hwnd, &pidl, POINT { x: c.x + c.w / 2, y: c.y + c.h / 2 }) {
                    log(&format!("context menu failed: {e}"));
                }
            }
            _ if ctrl && key.0 == 0x43 && is_file => verb("copy", false),
            _ if ctrl && key.0 == 0x58 && is_file => verb("cut", false),
            _ => {}
        }
    }

    /// Screen rectangle of the icon cell at `index` in window `host`, as currently scrolled.
    fn item_screen_rect(&self, host: u32, index: usize) -> Rect {
        let (m, r) = (self.metrics(host), self.shown_rect(host));
        let g = layout::grid(&m, r.w, self.shown_items(host).len());
        let scroll = self.ui.get(&host).map(|u| u.scroll).unwrap_or(0);
        let c = layout::cell_rect(&m, &g, index, scroll);
        Rect { x: r.x + c.x, y: r.y + c.y, w: c.w, h: c.h }
    }

    /// Select `index` in window `host` and scroll it into view.
    fn select_visible(&mut self, host: u32, index: usize) {
        let (m, r) = (self.metrics(host), self.shown_rect(host));
        let g = layout::grid(&m, r.w, self.shown_items(host).len());
        let c = layout::cell_rect(&m, &g, index, 0);
        let ui = self.ui.entry(host).or_default();
        let (top, bottom) = (m.title_h, r.h - m.pad);
        if c.y - ui.scroll < top {
            ui.scroll = c.y - top;
        } else if c.y + c.h - ui.scroll > bottom {
            ui.scroll = c.y + c.h - bottom;
        }
        ui.scroll = ui.scroll.clamp(0, layout::max_scroll(&m, &g, r.h));
        self.select(host, Some(index));
        self.paint(host);
    }

    /// Windows that show icons, in reading order (top to bottom, then left to right).
    fn icon_windows(&self) -> Vec<u32> {
        let mut hosts: Vec<u32> = self.cfg.hosts().into_iter().filter(|h| self.cfg.fence(*h).is_some_and(|f| !f.collapsed) && !self.shown_items(*h).is_empty()).collect();
        hosts.sort_by_key(|h| {
            let r = self.shown_rect(*h);
            (r.y / 40, r.x)
        });
        hosts
    }

    /// Arrow keys, Home, End, Page Up and Page Down: move through a group's grid, and at its edge
    /// on to the nearest group in that direction, landing on the icon closest to where we were.
    fn navigate(&mut self, key: VIRTUAL_KEY) {
        let Some((host, index, _)) = self.selected_item() else {
            if let Some(first) = self.icon_windows().first().copied() {
                self.select_visible(first, 0);
            }
            return;
        };
        let n = self.shown_items(host).len();
        let (m, r) = (self.metrics(host), self.shown_rect(host));
        let g = layout::grid(&m, r.w, n);
        let cols = g.cols.max(1) as usize;
        let page = (((r.h - m.title_h - m.pad) / m.cell_h.max(1)).max(1) as usize) * cols;
        let (col, row, rows) = (index % cols, index / cols, n.div_ceil(cols));
        let within = match key {
            VK_HOME => Some(0),
            VK_END => Some(n - 1),
            VK_PRIOR => Some(index.saturating_sub(page)),
            VK_NEXT => Some((index + page).min(n - 1)),
            VK_LEFT if col > 0 => Some(index - 1),
            VK_RIGHT if col + 1 < cols && index + 1 < n => Some(index + 1),
            VK_UP if row > 0 => Some(index - cols),
            VK_DOWN if index + cols < n => Some(index + cols),
            // The row below exists but is shorter: go to its last icon.
            VK_DOWN if row + 1 < rows => Some(n - 1),
            _ => None,
        };
        if let Some(i) = within {
            return self.select_visible(host, i);
        }
        // At the edge: the nearest group in that direction.
        let here = self.item_screen_rect(host, index);
        let (hx, hy) = (here.x + here.w / 2, here.y + here.h / 2);
        let best = self
            .icon_windows()
            .into_iter()
            .filter(|h| *h != host)
            .filter_map(|h| {
                let o = self.shown_rect(h);
                let (ahead, side) = match key {
                    VK_RIGHT => (o.x - (r.x + r.w), (o.y + o.h / 2 - hy).abs()),
                    VK_LEFT => (r.x - (o.x + o.w), (o.y + o.h / 2 - hy).abs()),
                    VK_DOWN => (o.y - (r.y + r.h), (o.x + o.w / 2 - hx).abs()),
                    VK_UP => (r.y - (o.y + o.h), (o.x + o.w / 2 - hx).abs()),
                    _ => return None,
                };
                // Only groups that lie that way (allowing a little overlap).
                (ahead > -40).then_some((h, ahead.max(0) + side * 2))
            })
            .min_by_key(|(_, score)| *score);
        let Some((to, _)) = best else { return };
        let count = self.shown_items(to).len();
        let nearest = (0..count)
            .min_by_key(|i| {
                let c = self.item_screen_rect(to, *i);
                let (dx, dy) = ((c.x + c.w / 2 - hx) as i64, (c.y + c.h / 2 - hy) as i64);
                dx * dx + dy * dy
            })
            .unwrap_or(0);
        self.select_visible(to, nearest);
    }

    /// Typing jumps to the next icon whose name starts with what was typed in the last second,
    /// in this group first and then in the others, as on the desktop.
    fn type_ahead(&mut self, ch: char) {
        if !ch.is_alphanumeric() {
            return;
        }
        let now = std::time::Instant::now();
        let fresh = self.typeahead.1.is_none_or(|t| now.duration_since(t).as_millis() > 1000);
        if fresh {
            self.typeahead.0.clear();
        }
        self.typeahead.0.push(ch);
        self.typeahead.1 = Some(now);
        let prefix = self.typeahead.0.clone();
        let name_of = |app: &App, key: &str| app.items.get(key).map(|i| i.name.to_lowercase()).unwrap_or_default();
        let current = self.selected_item();
        // Candidates in order: after the current icon in its group, then the other groups, then wrap.
        let mut order: Vec<(u32, usize)> = Vec::new();
        let windows = self.icon_windows();
        let start = current.as_ref().and_then(|(h, _, _)| windows.iter().position(|w| w == h)).unwrap_or(0);
        for k in 0..=windows.len() {
            let host = windows[(start + k) % windows.len().max(1)];
            let n = self.shown_items(host).len();
            for i in 0..n {
                order.push((host, i));
            }
        }
        // A repeated single letter cycles; a longer prefix may stay on the current icon.
        let skip_current = prefix.chars().count() == 1;
        let begin = current.as_ref().and_then(|(h, i, _)| order.iter().position(|(oh, oi)| oh == h && oi == i)).map(|p| if skip_current { p + 1 } else { p }).unwrap_or(0);
        let found = order.iter().cycle().skip(begin).take(order.len()).find(|(h, i)| {
            let items = self.shown_items(*h);
            items.get(*i).is_some_and(|k| name_of(self, k).starts_with(&prefix))
        });
        if let Some(&(h, i)) = found {
            self.select_visible(h, i);
        }
    }

    /// The group pasted or created files go to: the selected icon's group, else the catch-all.
    fn target_group(&self) -> Option<u32> {
        match self.selected_item() {
            Some((host, _, _)) => Some(self.cfg.shown_tab(host)),
            None => self.cfg.fences.iter().find(|f| f.inbox).map(|f| f.id),
        }
    }

    /// Ctrl+V: files copied or cut in Explorer (or here) are pasted into the group, like a drop.
    fn paste(&mut self) {
        let (Some(to), Some((files, cut))) = (self.target_group(), shell::clipboard_files(self.main)) else { return };
        if cut || self.portal_path(to).is_some() {
            // Cut: a move, which for something already on the Desktop is just filing it here.
            return self.drop_files(to, POINT { x: i32::MIN, y: i32::MIN }, files, cut);
        }
        // Copy: always a real copy, even of a Desktop item, which the shell names "… - Copy".
        shell::transfer(files, false, None, self.main, WM_DROP_ARRIVED, to, true);
    }

    /// Ctrl+Shift+N: a new folder in the group (on the Desktop, or in a portal's folder), selected
    /// and ready to be named as soon as it appears.
    fn new_folder(&mut self) {
        let Some(to) = self.target_group() else { return };
        let dir = match self.portal_path(to) {
            Some(p) => p,
            None => match shell::desktop_dirs().into_iter().next() {
                Some(d) => d,
                None => return,
            },
        };
        let base = std::path::Path::new(&dir);
        let mut path = base.join("New folder");
        let mut n = 2;
        while path.exists() {
            path = base.join(format!("New folder ({n})"));
            n += 1;
        }
        if let Err(e) = std::fs::create_dir(&path) {
            log(&format!("new folder failed: {e}"));
            return;
        }
        let path = path.display().to_string();
        if self.portal_path(to).is_none() {
            self.expect_arrival(&path, to);
        }
        self.pending_rename = Some(path);
    }

    /// The new folder from Ctrl+Shift+N has appeared: select it and start renaming it.
    fn rename_if_arrived(&mut self) {
        let Some(path) = self.pending_rename.clone() else { return };
        for host in self.cfg.hosts() {
            if let Some(i) = self.shown_items(host).iter().position(|k| k.eq_ignore_ascii_case(&path)) {
                self.pending_rename = None;
                self.select_visible(host, i);
                self.start_item_rename(host, i);
                return;
            }
        }
    }

    /// F2: an edit box over the icon's label. Enter renames the file through the shell (hidden
    /// extensions stay hidden, conflicts get Explorer's own dialog); Esc cancels.
    fn start_item_rename(&mut self, host: u32, index: usize) {
        self.finish_rename(true);
        let Some(key) = self.shown_items(host).get(index).cloned() else { return };
        let Some(item) = self.items.get(&key) else { return };
        let text = shell::edit_name(&item.pidl).unwrap_or_else(|| item.name.clone());
        let is_folder = item.is_folder;
        let (m, cell) = (self.metrics(host), self.item_screen_rect(host, index));
        unsafe {
            let wide_text = wide(&text);
            let style = WS_POPUP | WS_BORDER | WINDOW_STYLE((ES_AUTOHSCROLL | ES_CENTER) as u32);
            let (w, h) = (cell.w + m.px(24.0), m.px(24.0));
            let y = cell.y + m.icon + m.px(10.0);
            let Ok(edit) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("EDIT"), PCWSTR(wide_text.as_ptr()), style, cell.x - m.px(12.0), y, w, h, None, None, Some(self.hinst), None) else { return };
            let font = CreateFontW(-m.px(13.0), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            // Select the name without its extension, as Explorer does.
            let end = match text.rfind('.') {
                Some(dot) if !is_folder && dot > 0 => text[..dot].encode_utf16().count() as isize,
                _ => -1,
            };
            SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(end)));
            EDIT_PROC = std::mem::transmute::<isize, WNDPROC>(SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize));
            self.rename = Some((host, edit, font));
            self.rename_item = Some(key);
            let _ = ShowWindow(edit, SW_SHOW);
            shell::take_foreground(edit);
        }
    }

    /// The user clicked somewhere that isn't a group (the bare desktop, another program): like
    /// on the native desktop, nothing stays selected.
    fn clear_selection(&mut self) {
        self.select(0, None);
    }

    // ------------------------------------------------------------ pointer input

    fn mouse_down(&mut self, id: u32, hwnd: HWND, x: i32, y: i32) {
        self.finish_rename(true);
        let pt = cursor();
        let zone = self.zone(id, x, y);
        if !matches!(zone, Zone::Item(_)) {
            self.clear_selection();
        }
        self.drag = match zone {
            Zone::Edge(edges) => Drag::Resize { id, edges, start: self.cfg.fence(id).map(|f| f.rect).unwrap_or_default(), px: pt.x, py: pt.y },
            Zone::Title => Drag::Move { id, dx: x, dy: y },
            // A tab comes forward on press; dragging from it still moves the group.
            Zone::Tab(i) => {
                self.switch_tab(id, i);
                Drag::Move { id, dx: x, dy: y }
            }
            Zone::Back => {
                self.portal_back(self.cfg.shown_tab(id));
                Drag::None
            }
            Zone::Item(index) => {
                self.select(id, Some(index));
                // As on the native desktop, clicking an icon makes the desktop the active window,
                // so keys (Delete, Enter) go to it rather than to the last program used.
                if let Some(desktop) = shell::desktop_host() {
                    unsafe {
                        let _ = SetForegroundWindow(desktop);
                    }
                }
                Drag::Pending { id, index, px: pt.x, py: pt.y }
            }
            Zone::Body => Drag::None,
        };
        if !matches!(self.drag, Drag::None) {
            unsafe {
                SetCapture(hwnd);
            }
        }
    }

    fn mouse_move(&mut self, id: u32, hwnd: HWND, x: i32, y: i32) {
        let pt = cursor();
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Move { id, dx, dy } => {
                let mut r = self.shown_rect(id);
                r.x = pt.x - dx;
                r.y = pt.y - dy;
                let r = self.snap(id, r);
                if let Some(f) = self.cfg.fence_mut(id) {
                    f.rect.x = r.x;
                    f.rect.y = r.y;
                }
                let margin = self.metrics(id).margin;
                unsafe {
                    let _ = SetWindowPos(hwnd, None, r.x - margin, r.y - margin, 0, 0, SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOZORDER);
                }
                // A wallpaper material shows what is behind the group, which changes as it moves.
                if self.uses_backdrop(id) {
                    self.paint(id);
                }
                self.drag = Drag::Move { id, dx, dy };
            }
            Drag::Resize { id, edges, start, px, py } => {
                let (min_w, min_h) = self.metrics(id).min_size();
                let (dx, dy) = (pt.x - px, pt.y - py);
                let mut r = start;
                if edges & EDGE_L != 0 {
                    r.w = (start.w - dx).max(min_w);
                    r.x = start.x + start.w - r.w;
                }
                if edges & EDGE_R != 0 {
                    r.w = (start.w + dx).max(min_w);
                }
                if edges & EDGE_T != 0 {
                    r.h = (start.h - dy).max(min_h);
                    r.y = start.y + start.h - r.h;
                }
                if edges & EDGE_B != 0 {
                    r.h = (start.h + dy).max(min_h);
                }
                if let Some(f) = self.cfg.fence_mut(id) {
                    f.rect = r;
                }
                self.place(id);
                self.drag = Drag::Resize { id, edges, start, px, py };
            }
            Drag::Pending { id, index, px, py } => {
                let threshold = self.metrics(id).px(5.0);
                let key = self.shown(id).and_then(|f| f.items.get(index)).cloned();
                match key {
                    Some(key) if (pt.x - px).abs() > threshold || (pt.y - py).abs() > threshold => self.drag_item_out(self.cfg.shown_tab(id), hwnd, key),
                    _ => self.drag = Drag::Pending { id, index, px, py },
                }
            }
            Drag::None => {
                let zone = self.zone(id, x, y);
                let hover = match zone {
                    Zone::Item(i) => Some(i),
                    _ => None,
                };
                let tab_hover = match zone {
                    Zone::Tab(i) => Some(i),
                    _ => None,
                };
                let ui = self.ui.entry(id).or_default();
                let changed = ui.hover != hover || ui.tab_hover != tab_hover || !ui.hot;
                ui.hover = hover;
                ui.tab_hover = tab_hover;
                ui.hot = true;
                if !ui.tracking {
                    ui.tracking = true;
                    let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    unsafe {
                        let _ = TrackMouseEvent(&mut track);
                    }
                }
                if changed {
                    self.paint(id);
                }
            }
        }
    }

    /// Hand an icon to OLE drag and drop. Our own groups are drop targets too, so the same drag
    /// reorders within a group, moves between groups, or leaves for another program.
    fn drag_item_out(&mut self, from: u32, hwnd: HWND, key: String) {
        let Some(pidl) = self.items.get(&key).map(|i| i.pidl.clone()) else { return };
        unsafe {
            let _ = ReleaseCapture();
        }
        self.own_drag = Some((from, key));
        // Modal: returns when the item is dropped or the drag is cancelled. Our drop targets run inside it.
        if let Err(e) = shell::drag_out(hwnd, &pidl) {
            log(&format!("drag failed: {e}"));
        }
        self.own_drag = None;
        self.set_insert(None);
    }

    fn is_portal(&self, id: u32) -> bool {
        self.cfg.fence(id).is_some_and(|f| f.is_portal())
    }

    /// True when the drag in progress is one of our desktop icons and dropping it on group `to`
    /// only regroups it. Anything involving a portal is a real file transfer instead.
    pub fn own_regroup(&self, to: u32) -> bool {
        self.own_drag.as_ref().is_some_and(|(from, _)| !self.is_portal(*from)) && !self.is_portal(to)
    }

    pub fn is_on_desktop(&self, path: &str) -> bool {
        self.desktop_dirs.iter().any(|d| d.eq_ignore_ascii_case(parent_dir(path)))
    }

    /// Where files dropped on group `to` end up: the portal's folder, or the Desktop.
    fn drop_folder(&self, to: u32) -> Option<String> {
        self.portal_path(to).or_else(|| self.desktop_dirs.first().cloned())
    }

    /// Whether dropping `path` on group `to` moves it by default (same drive) rather than copies it.
    pub fn moves_by_default(&self, to: u32, path: &str) -> bool {
        if !self.is_portal(to) && self.is_on_desktop(path) {
            return true;
        }
        let drive = |p: &str| p.get(..2).map(str::to_ascii_lowercase);
        self.drop_folder(to).is_some_and(|d| drive(&d) == drive(path) && path.as_bytes().get(1) == Some(&b':'))
    }

    /// A drag is over group `target` (or over none of ours): move the insertion marker.
    /// Portals have no manual order, so they show none.
    /// Dragging over another tab's label brings that tab forward, so the drop lands where the user can see.
    pub fn drag_over(&mut self, target: Option<u32>, pt: POINT) {
        let hit = target.and_then(|host| self.drop_target(pt).filter(|t| self.cfg.tab_host(t.0) == self.cfg.tab_host(host)));
        if let Some((tab, _)) = hit.filter(|t| t.0 != self.cfg.shown_tab(t.0)) {
            let order = self.cfg.tab_order(tab);
            if let Some(i) = order.iter().position(|t| *t == tab) {
                self.switch_tab(tab, i);
            }
        }
        let at = hit.filter(|t| !self.is_portal(t.0)).map(|(tab, index)| (self.cfg.tab_host(tab), index));
        self.set_insert(at);
    }

    /// One of our own icons was dropped on group `to`: regroup or reorder it. No file is touched.
    pub fn drop_own_item(&mut self, to: u32, pt: POINT) {
        let (Some((from, key)), Some((_, index))) = (self.own_drag.clone(), self.drop_target(pt).filter(|t| t.0 == to)) else { return };
        if self.cfg.move_item(&key, to, index) {
            let landed = self.cfg.fence(to).and_then(|f| f.items.iter().position(|k| *k == key));
            self.paint(from);
            self.select(to, landed);
            self.paint(to);
            self.dirty();
        }
    }

    /// The item under a screen point, if dropping *onto* it means something: folders and system
    /// items such as the Recycle Bin. Only the middle of a cell counts, so the gaps between icons
    /// stay available for reordering. Never the item being dragged itself.
    pub fn drop_item_at(&self, id: u32, pt: POINT) -> Option<(usize, String, shell::Pidl)> {
        let (f, r, m) = (self.shown(id)?, self.shown_rect(id), self.metrics(id));
        if self.cfg.fence(self.cfg.tab_host(id))?.collapsed || pt.y - r.y < m.title_h {
            return None;
        }
        let scroll = self.ui.get(&self.cfg.tab_host(id)).map(|u| u.scroll).unwrap_or(0);
        let g = layout::grid(&m, r.w, f.items.len());
        let index = layout::hit(&m, &g, pt.x - r.x, pt.y - r.y, scroll, f.items.len())?;
        let cell = layout::cell_rect(&m, &g, index, scroll);
        if (pt.x - r.x - (cell.x + cell.w / 2)).abs() > cell.w * 3 / 10 {
            return None;
        }
        let item = self.items.get(&f.items[index])?;
        let dragged_itself = self.own_drag.as_ref().is_some_and(|(_, key)| *key == item.key);
        ((item.is_folder || item.key.starts_with("::")) && !dragged_itself).then(|| (index, item.key.clone(), item.pidl.clone()))
    }

    /// Highlight the item a drag would be dropped onto (or clear the highlight).
    pub fn drag_onto(&mut self, id: u32, index: Option<usize>) {
        if index.is_some() {
            self.set_insert(None);
        }
        let ui = self.ui.entry(id).or_default();
        if ui.hover != index {
            ui.hover = index;
            self.paint(id);
        }
    }

    /// File something that is about to appear on the desktop into group `to` when it does.
    fn expect_arrival(&mut self, path: &str, to: u32) {
        if self.pending_drops.len() > 512 {
            self.pending_drops.clear();
        }
        self.pending_drops.insert(path.to_lowercase(), to);
        unsafe {
            SetTimer(Some(self.main), TIMER_REFRESH, 200, None);
        }
    }

    /// Files from another program were dropped on group `to`.
    pub fn drop_files(&mut self, to: u32, pt: POINT, files: Vec<String>, move_them: bool) {
        if let Some(folder) = self.portal_path(to) {
            // Into a portal: a plain transfer into its folder; the portal's own watcher shows the result.
            let incoming: Vec<String> = files.into_iter().filter(|f| !parent_dir(f).eq_ignore_ascii_case(&folder)).collect();
            if !incoming.is_empty() {
                shell::transfer(incoming, move_them, Some(folder), self.main, WM_DROP_ARRIVED, 0, false);
            }
            return;
        }
        let mut index = self.drop_target(pt).filter(|t| t.0 == to).map(|t| t.1).unwrap_or(usize::MAX);
        let mut incoming = Vec::new();
        for file in files {
            if self.is_on_desktop(&file) {
                // Already a desktop item (dragged from an Explorer window showing the Desktop): just file it here.
                let key = self.items.keys().find(|k| k.eq_ignore_ascii_case(&file)).cloned();
                if key.is_some_and(|key| self.cfg.move_item(&key, to, index)) {
                    index = index.saturating_add(1);
                }
            } else {
                incoming.push(file);
            }
        }
        if !incoming.is_empty() {
            // Each file's real destination comes back as WM_DROP_ARRIVED, then the change notification delivers the item.
            shell::transfer(incoming, move_them, None, self.main, WM_DROP_ARRIVED, to, false);
        }
        self.paint_all();
        self.dirty();
    }

    /// A web link was dropped on group `to`: make an Internet shortcut on the Desktop and file it there.
    pub fn drop_link(&mut self, to: u32, url: &str, title: &str) {
        let Some(folder) = self.drop_folder(to) else { return };
        match shell::create_url_shortcut(&folder, &tessera_core::rules::shortcut_name(title, url), url) {
            Some(_) if self.is_portal(to) => {}
            Some(path) => self.expect_arrival(&path, to),
            None => log("could not create a shortcut for the dropped link"),
        }
    }

    fn mouse_up(&mut self) {
        // Commit first: releasing capture sends WM_CAPTURECHANGED, which cancels whatever is still in progress.
        self.end_drag(true);
        unsafe {
            let _ = ReleaseCapture();
        }
    }

    /// Finish whatever move or resize is in progress.
    fn end_drag(&mut self, _commit: bool) {
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::Move { .. } | Drag::Resize { .. } => self.dirty(),
            Drag::Pending { .. } | Drag::None => {}
        }
    }

    fn double_click(&mut self, id: u32, hwnd: HWND, x: i32, y: i32) {
        match self.zone(id, x, y) {
            Zone::Title | Zone::Tab(_) => self.toggle_collapsed(id),
            Zone::Item(index) => {
                let Some(fence) = self.cfg.fence(self.cfg.shown_tab(id)) else { return };
                let id = fence.id;
                let Some(item) = fence.items.get(index).and_then(|k| self.items.get(k)) else { return };
                self.drag = Drag::None;
                // Inside a portal, opening a folder browses into it instead of opening Explorer.
                if fence.is_portal() && item.is_folder && item.key.contains(":\\") {
                    let folder = item.key.clone();
                    self.portal_show(id, Some(folder));
                } else {
                    let pidl = item.pidl.clone();
                    shell::open(hwnd, &pidl);
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ folder portals

    /// The folder a portal is showing right now: where it was browsed to, or its own folder.
    /// Only the tab on screen can be browsed; a portal in a background tab shows its own folder.
    fn portal_path(&self, id: u32) -> Option<String> {
        let root = self.cfg.fence(id)?.portal.clone()?;
        let host = self.cfg.tab_host(id);
        let nav = (self.cfg.shown_tab(host) == id).then(|| self.ui.get(&host).and_then(|u| u.nav.clone())).flatten();
        Some(nav.unwrap_or(root))
    }

    fn portal_folders(&self) -> Vec<String> {
        self.cfg.fences.iter().filter_map(|f| self.portal_path(f.id)).collect()
    }

    /// Ask the worker for a fresh listing of every portal (or just one).
    fn refresh_portals(&mut self, only: Option<u32>) {
        for id in self.cfg.fences.iter().filter(|f| f.is_portal() && only.is_none_or(|o| o == f.id)).map(|f| f.id).collect::<Vec<_>>() {
            if let Some(path) = self.portal_path(id) {
                let _ = self.portal_tx.send((id, path));
            }
        }
    }

    /// A listing arrived from the worker: show it, unless the portal has moved on since it was requested.
    fn portal_listed(&mut self, listing: shell::PortalListing) {
        if self.portal_path(listing.id).is_none_or(|p| !p.eq_ignore_ascii_case(&listing.path)) {
            return;
        }
        let Some(fence) = self.cfg.fence_mut(listing.id) else { return };
        let new_keys: HashSet<&str> = listing.items.iter().map(|i| i.key.as_str()).collect();
        for gone in fence.items.iter().filter(|k| !new_keys.contains(k.as_str())) {
            self.items.remove(gone);
            self.portal_keys.remove(gone);
            self.renderer.forget_icon(gone);
        }
        let unchanged = fence.items.len() == listing.items.len() && listing.items.iter().all(|i| self.items.get(&i.key).is_some_and(|old| old.name == i.name));
        fence.items = listing.items.iter().map(|i| i.key.clone()).collect();
        let info = listing.items.iter().map(|i| (i.key.as_str(), Present { key: &i.key, name: &i.name, is_folder: i.is_folder })).collect::<HashMap<_, _>>();
        self.cfg.sort_fence(listing.id, &info.iter().map(|(k, p)| (*k, p)).collect());
        drop(info);
        for item in listing.items {
            self.portal_keys.insert(item.key.clone());
            self.items.insert(item.key.clone(), item);
        }
        if !unchanged {
            self.paint(listing.id);
        }
        self.rename_if_arrived();
    }

    /// Point a portal at a subfolder (`Some`) or back at its own folder (`None`).
    fn portal_show(&mut self, id: u32, folder: Option<String>) {
        let Some(fence) = self.cfg.fence_mut(id) else { return };
        for key in std::mem::take(&mut fence.items) {
            self.items.remove(&key);
            self.portal_keys.remove(&key);
        }
        let ui = self.ui.entry(self.cfg.tab_host(id)).or_default();
        ui.nav = folder;
        ui.scroll = 0;
        ui.hover = None;
        ui.selected = None;
        self.paint(id);
        self.refresh_portals(Some(id));
        self.watch_shell();
    }

    /// Back chevron: up one folder, never above the portal's own folder.
    fn portal_back(&mut self, id: u32) {
        let host = self.cfg.tab_host(id);
        let (Some(root), Some(current)) = (self.cfg.fence(id).and_then(|f| f.portal.clone()), self.ui.get(&host).and_then(|u| u.nav.clone())) else { return };
        let parent = parent_dir(&current);
        let inside = parent.len() > root.len() && parent.to_lowercase().starts_with(&root.to_lowercase());
        self.portal_show(id, inside.then(|| parent.to_string()));
    }

    /// Ask for a folder and add a portal for it.
    fn pick_portal(&mut self) {
        let Some(path) = shell::pick_folder(self.main) else { return };
        let id = self.cfg.add_portal(&path, shell::primary_work_area());
        self.create_window(id);
        self.refresh_portals(Some(id));
        self.watch_shell();
        self.dirty();
    }

    /// (Re)register for change notifications on the desktop and on every folder a portal shows.
    fn watch_shell(&mut self) {
        shell::unregister_notify(self.notify_id);
        self.notify_id = shell::register_notify(self.main, WM_SHELL_CHANGE, &self.portal_folders());
    }

    // ------------------------------------------------------------ profiles

    /// A different profile became the on-screen layout: rebuild every group window from it.
    fn reload_layout(&mut self) {
        for id in self.wins.iter().map(|w| w.0).collect::<Vec<_>>() {
            self.destroy_window(id);
        }
        for key in std::mem::take(&mut self.portal_keys) {
            self.items.remove(&key);
        }
        for f in self.cfg.fences.iter_mut().filter(|f| f.is_portal()) {
            f.items.clear();
        }
        // The desktop may have changed while this profile was away.
        let present = desktop_present(&self.items, &self.portal_keys);
        self.cfg.reconcile(&present, shell::primary_work_area());
        drop(present);
        self.cfg.apply_placement(&self.display, shell::virtual_screen());
        for f in &mut self.cfg.fences {
            f.rect = f.rect.clamp_into(shell::work_area_for(f.rect));
        }
        for id in self.cfg.hosts() {
            self.create_window(id);
        }
        self.refresh_portals(None);
        self.watch_shell();
    }

    fn switch_profile(&mut self, id: u32) {
        self.finish_rename(true);
        self.cfg.store_placement(&self.display);
        if self.cfg.profile_switch(id) {
            self.reload_layout();
            self.dirty();
        }
    }

    fn toggle_collapsed(&mut self, id: u32) {
        if let Some(f) = self.cfg.fence_mut(id) {
            f.collapsed = !f.collapsed;
            self.place(id);
            self.dirty();
        }
    }

    fn wheel(&mut self, id: u32, delta: i32) {
        let step = self.metrics(id).cell_h * delta / 120;
        let ui = self.ui.entry(id).or_default();
        let before = ui.scroll;
        ui.scroll = (ui.scroll - step).max(0);
        ui.hover = None;
        self.paint(id); // clamps the offset
        let _ = before;
    }

    fn mouse_leave(&mut self, id: u32) {
        let ui = self.ui.entry(id).or_default();
        ui.tracking = false;
        if ui.hot || ui.hover.is_some() || ui.tab_hover.is_some() {
            ui.hot = false;
            ui.hover = None;
            ui.tab_hover = None;
            self.paint(id);
        }
    }

    fn right_click(&mut self, id: u32, hwnd: HWND, x: i32, y: i32) {
        self.finish_rename(true);
        let pt = cursor();
        match self.zone(id, x, y) {
            Zone::Item(index) => {
                self.select(id, Some(index));
                let pidl = self.shown(id).and_then(|f| f.items.get(index)).and_then(|k| self.items.get(k)).map(|i| i.pidl.clone());
                if let Some(pidl) = pidl {
                    if let Err(e) = shell::context_menu(hwnd, &pidl, pt) {
                        log(&format!("context menu: {e}"));
                    }
                }
            }
            // Right-clicking a tab brings it forward first, so the menu is about that tab.
            Zone::Tab(i) => {
                self.switch_tab(id, i);
                self.fence_menu(id, hwnd, pt);
            }
            _ => self.fence_menu(id, hwnd, pt),
        }
    }

    // ------------------------------------------------------------ menus and commands

    /// The group menu. Look (colour, size, opacity, roll-up) belongs to the window; name, sort and
    /// deletion belong to the tab on screen.
    fn fence_menu(&mut self, id: u32, hwnd: HWND, pt: POINT) {
        let id = self.cfg.tab_host(id);
        let tab = self.cfg.shown_tab(id);
        let order = self.cfg.tab_order(id);
        let tabbed = order.len() > 1;
        // Other windows this one could be combined into.
        let others: Vec<(u32, String)> = self.cfg.hosts().into_iter().filter(|h| *h != id).filter_map(|h| self.cfg.fence(h).map(|f| (h, f.title.clone()))).collect();
        let (Some(f), Some(tf)) = (self.cfg.fence(id), self.cfg.fence(tab)) else { return };
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let position = order.iter().position(|t| *t == tab).unwrap_or(0);
        let cmd = unsafe {
            let (Ok(menu), Ok(sort), Ok(size), Ok(tint), Ok(opacity), Ok(tabs), Ok(combine)) =
                (CreatePopupMenu(), CreatePopupMenu(), CreatePopupMenu(), CreatePopupMenu(), CreatePopupMenu(), CreatePopupMenu(), CreatePopupMenu())
            else {
                return;
            };
            let _ = AppendMenuW(sort, check(tf.sort == Sort::Manual), 10, w!("Manual"));
            let _ = AppendMenuW(sort, check(tf.sort == Sort::Name), 11, w!("Name"));
            let _ = AppendMenuW(sort, check(tf.sort == Sort::Kind), 12, w!("Kind"));
            let _ = AppendMenuW(tabs, MF_STRING, 60, w!("Add tab"));
            if tabbed {
                let _ = AppendMenuW(tabs, if position > 0 { MF_STRING } else { MF_GRAYED }, 62, w!("Move tab left"));
                let _ = AppendMenuW(tabs, if position + 1 < order.len() { MF_STRING } else { MF_GRAYED }, 63, w!("Move tab right"));
                let _ = AppendMenuW(tabs, MF_STRING, 61, w!("Make this tab its own group"));
            }
            for (i, (_, title)) in others.iter().enumerate().take(30) {
                let _ = AppendMenuW(combine, MF_STRING, 70 + i, PCWSTR(wide(title).as_ptr()));
            }
            let _ = AppendMenuW(tabs, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(tabs, if others.is_empty() { MF_POPUP | MF_GRAYED } else { MF_POPUP }, combine.0 as usize, w!("Combine into"));
            for (i, (px, label)) in [(32, w!("Small")), (48, w!("Medium")), (64, w!("Large"))].into_iter().enumerate() {
                let _ = AppendMenuW(size, check(f.style.icon == px), 20 + i, label);
            }
            for (i, t) in Tint::ALL.into_iter().enumerate() {
                let _ = AppendMenuW(tint, check(f.style.tint == t), 30 + i, PCWSTR(wide(t.name()).as_ptr()));
            }
            for (i, (pct, label)) in [(35, w!("Light")), (62, w!("Balanced")), (80, w!("Strong")), (95, w!("Solid"))].into_iter().enumerate() {
                let _ = AppendMenuW(opacity, check(f.style.opacity == pct), 40 + i, label);
            }
            let _ = AppendMenuW(menu, MF_STRING, 1, if tabbed { w!("Rename tab") } else { w!("Rename") });
            let _ = AppendMenuW(menu, MF_STRING, 2, if f.collapsed { w!("Expand") } else { w!("Roll up") });
            let _ = AppendMenuW(menu, MF_STRING, 3, w!("Fit to contents"));
            if tf.is_portal() {
                let _ = AppendMenuW(menu, MF_STRING, 4, w!("Open in Explorer"));
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_POPUP, tabs.0 as usize, w!("Tabs"));
            let _ = AppendMenuW(menu, MF_POPUP, sort.0 as usize, w!("Sort by"));
            let _ = AppendMenuW(menu, MF_POPUP, size.0 as usize, w!("Icon size"));
            let _ = AppendMenuW(menu, MF_POPUP, tint.0 as usize, w!("Colour"));
            let _ = AppendMenuW(menu, MF_POPUP, opacity.0 as usize, w!("Opacity"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, 50, w!("New group"));
            let _ = AppendMenuW(menu, MF_STRING, 52, w!("New folder portal…"));
            let _ = AppendMenuW(menu, MF_STRING, 53, w!("Settings…"));
            let delete_label = match (tabbed, tf.is_portal()) {
                (true, _) => w!("Close tab"),
                (false, true) => w!("Remove portal"),
                (false, false) => w!("Delete group"),
            };
            let _ = AppendMenuW(menu, if tf.inbox { MF_GRAYED } else { MF_STRING }, 51, delete_label);
            let _ = SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.x, pt.y, hwnd, None).0 as usize;
            let _ = DestroyMenu(menu);
            cmd
        };
        match cmd {
            1 => return self.start_rename(tab),
            2 => return self.toggle_collapsed(id),
            3 => self.fit_to_contents(id),
            4 => return self.portal_path(tab).iter().for_each(|p| shell::open_path(p)),
            52 => return self.pick_portal(),
            53 => return self.open_settings(),
            60 => {
                self.command(Command::TabAdd { id, title: None });
                return self.start_rename(self.cfg.shown_tab(id));
            }
            61 => return self.command(Command::TabDetach { id: tab }),
            62 | 63 => return self.command(Command::TabMove { id: tab, delta: if cmd == 62 { -1 } else { 1 } }),
            70..=99 => {
                if let Some((into, _)) = others.get(cmd - 70) {
                    self.command(Command::TabMerge { id, into: *into });
                }
                return;
            }
            10..=12 => {
                if let Some(f) = self.cfg.fence_mut(tab) {
                    f.sort = [Sort::Manual, Sort::Name, Sort::Kind][cmd - 10];
                }
                let present: Vec<Present> = self.items.values().map(|i| Present { key: &i.key, name: &i.name, is_folder: i.is_folder }).collect();
                let info = present.iter().map(|p| (p.key, p)).collect();
                self.cfg.sort_fence(tab, &info);
            }
            20..=22 => {
                if let Some(f) = self.cfg.fence_mut(id) {
                    f.style.icon = [32, 48, 64][cmd - 20];
                }
            }
            30..=36 => {
                if let Some(f) = self.cfg.fence_mut(id) {
                    f.style.tint = Tint::ALL[cmd - 30];
                }
            }
            40..=43 => {
                if let Some(f) = self.cfg.fence_mut(id) {
                    f.style.opacity = [35, 62, 80, 95][cmd - 40];
                }
            }
            50 => return self.new_group(Some(pt)),
            51 => return self.delete_group(tab),
            _ => return,
        }
        self.place(id);
        self.dirty();
    }

    fn delete_group(&mut self, id: u32) {
        let listing = self.cfg.fence(id).filter(|f| f.is_portal()).map(|f| f.items.clone()).unwrap_or_default();
        if self.cfg.remove_fence(id, shell::primary_work_area()) {
            for key in listing {
                self.items.remove(&key);
                self.portal_keys.remove(&key);
            }
            self.sync_windows();
            self.watch_shell();
            self.dirty();
        }
    }

    fn fit_to_contents(&mut self, id: u32) {
        let m = self.metrics(id);
        let count = self.shown(id).map(|f| f.items.len()).unwrap_or(0);
        let Some(f) = self.cfg.fence_mut(self.cfg.tab_host(id)) else { return };
        let g = layout::grid(&m, f.rect.w, count);
        let (_, h) = m.size_for(g.cols, g.rows);
        f.rect.h = h;
        f.rect = f.rect.clamp_into(shell::work_area_for(f.rect));
    }

    fn new_group(&mut self, at: Option<POINT>) {
        let area = shell::primary_work_area();
        let (w, h) = Metrics::new(&self.cfg.default_style, 96).size_for(3, 2);
        let pos = at.unwrap_or(POINT { x: area.x + (area.w - w) / 2, y: area.y + (area.h - h) / 2 });
        let id = self.cfg.add_fence("New group", Rect { x: pos.x, y: pos.y, w, h }.clamp_into(area));
        self.create_window(id);
        self.dirty();
        self.start_rename(id);
    }

    /// Put an edit box over the title. Group windows never take focus themselves.
    fn start_rename(&mut self, id: u32) {
        self.finish_rename(true);
        let (Some(f), r, m) = (self.cfg.fence(id), self.shown_rect(id), self.metrics(id)) else { return };
        unsafe {
            let title = wide(&f.title);
            let style = WS_POPUP | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
            let Ok(edit) = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, w!("EDIT"), PCWSTR(title.as_ptr()), style, r.x + m.px(6.0), r.y + m.px(3.0), r.w - m.px(12.0), m.title_h - m.px(6.0), None, None, Some(self.hinst), None) else { return };
            let font = CreateFontW(-m.px(13.0), 0, 0, 0, 400, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, 0, w!("Segoe UI"));
            SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
            EDIT_PROC = std::mem::transmute::<isize, WNDPROC>(SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize));
            self.rename = Some((id, edit, font));
            let _ = ShowWindow(edit, SW_SHOW);
            let _ = SetForegroundWindow(edit);
            let _ = SetFocus(Some(edit));
        }
    }

    fn finish_rename(&mut self, commit: bool) {
        let Some((id, edit, font)) = self.rename.take() else { return };
        unsafe {
            let mut buf = [0u16; 128];
            let len = GetWindowTextW(edit, &mut buf) as usize;
            let _ = DestroyWindow(edit);
            let _ = DeleteObject(font.into());
            let text = String::from_utf16_lossy(&buf[..len]).trim().to_string();
            // An icon's label: rename the file itself through the shell.
            if let Some(key) = self.rename_item.take() {
                let item = self.items.get(&key).map(|i| (i.pidl.clone(), shell::edit_name(&i.pidl).unwrap_or_else(|| i.name.clone())));
                if let (true, Some((pidl, before)), Some(hwnd)) = (commit && !text.is_empty(), item, self.hwnd(id)) {
                    if text != before {
                        if let Err(e) = shell::rename(hwnd, &pidl, &text) {
                            log(&format!("rename failed: {e}"));
                        }
                    }
                }
                // Back to the desktop, so the arrow keys carry on working, as after a rename in Explorer.
                if let Some(desktop) = shell::desktop_host() {
                    let _ = SetForegroundWindow(desktop);
                }
                return;
            }
            if commit && !text.is_empty() {
                if let Some(f) = self.cfg.fence_mut(id) {
                    f.title = text;
                }
                self.paint(id);
                self.dirty();
            }
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
        GROUPS_HIDDEN.store(hidden, Ordering::Relaxed);
        for (_, hwnd) in &self.wins {
            unsafe {
                let _ = ShowWindow(*hwnd, if hidden { SW_HIDE } else { SW_SHOWNOACTIVATE });
            }
        }
    }

    /// Peek: lift the groups above every application window, or drop them back onto the desktop.
    fn set_peek(&mut self, on: bool) {
        PEEK.store(on, Ordering::Relaxed);
        if on && self.hidden {
            self.set_hidden(false);
        }
        for (_, hwnd) in &self.wins {
            unsafe {
                let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
                if on {
                    let _ = SetWindowPos(*hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
                } else {
                    let _ = SetWindowPos(*hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
                    let _ = SetWindowPos(*hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, flags);
                }
            }
        }
    }

    /// Show Desktop (Win+D) lifts the desktop window over every normal window, bottom-pinned ones
    /// included, and drops it again later. Groups must stay directly above the desktop window
    /// wherever it goes. Re-checked on foreground changes, never polled.
    fn follow_desktop(&mut self, recheck: bool) {
        // The keyboard hook follows the desktop being the active window; the delayed re-checks
        // below also catch a desktop that was already active when Tessera started.
        watch_desktop_keys(self.main);
        if PEEK.load(Ordering::Relaxed) {
            return;
        }
        unsafe {
            let Some(host) = shell::desktop_host() else { return };
            for (_, hwnd) in &self.wins {
                // In place if only our own or invisible windows sit between this group and the desktop.
                let mut next = GetWindow(*hwnd, GW_HWNDNEXT);
                let in_place = loop {
                    match next {
                        Ok(h) if h == host => break true,
                        Ok(h) if is_ours(h) || !IsWindowVisible(h).as_bool() => next = GetWindow(h, GW_HWNDNEXT),
                        _ => break false,
                    }
                };
                if !in_place {
                    // fence_proc rewrites the destination to "directly above the desktop".
                    let _ = SetWindowPos(*hwnd, Some(HWND_BOTTOM), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
            // The shell reorders the desktop a moment after the foreground changes.
            if recheck {
                SetTimer(Some(self.main), TIMER_ZCHECK, 250, None);
                SetTimer(Some(self.main), TIMER_ZCHECK_LATE, 1500, None);
            }
        }
    }

    /// Run a command from the settings window or a script. The layout file is written before
    /// returning, so the sender can read the result back immediately.
    fn command(&mut self, cmd: Command) {
        self.finish_rename(true);
        match self.cfg.apply(cmd, shell::primary_work_area()) {
            Effect::None => return,
            Effect::Restyle(ids) => ids.into_iter().for_each(|id| self.place(id)),
            Effect::Resort(id) => {
                let present: Vec<Present> = self.items.values().map(|i| Present { key: &i.key, name: &i.name, is_folder: i.is_folder }).collect();
                let info = present.iter().map(|p| (p.key, p)).collect();
                self.cfg.sort_fence(id, &info);
                drop(present);
                self.place(id);
            }
            Effect::RepaintAll => self.paint_all(),
            Effect::SaveOnly => {}
            Effect::Windows => self.sync_windows(),
            Effect::Reload => self.reload_layout(),
            Effect::PickPortal => unsafe {
                // The folder dialog must not run inside the sender's SendMessage.
                let _ = PostMessageW(Some(self.main), WM_PICK_PORTAL, WPARAM(0), LPARAM(0));
                return;
            },
            Effect::Created(id) => {
                self.create_window(id);
                self.refresh_portals(Some(id));
                self.watch_shell();
            }
            Effect::Deleted(id) => {
                // The model has already dropped the group (a tab may have taken over its window).
                let _ = id;
                self.sync_windows();
                let live: HashSet<&String> = self.cfg.fences.iter().flat_map(|f| f.items.iter()).collect();
                let orphaned: Vec<String> = self.portal_keys.iter().filter(|k| !live.contains(k)).cloned().collect();
                for key in orphaned {
                    self.items.remove(&key);
                    self.portal_keys.remove(&key);
                }
                self.paint_all();
                self.watch_shell();
            }
            Effect::Rebuild => self.reset_groups(),
            Effect::Peek(on) => return self.set_peek(on),
            Effect::Hidden(on) => return self.set_hidden(on),
            Effect::Quit => unsafe {
                // Posted, not destroyed here: we are inside the sender's SendMessage.
                let _ = PostMessageW(Some(self.main), WM_CLOSE, WPARAM(0), LPARAM(0));
            },
        }
        self.cfg.store_placement(&self.display);
        self.save();
    }

    /// Open the settings window: a separate program that only exists while it is on screen.
    fn open_settings(&self) {
        unsafe {
            if let Ok(existing) = FindWindowW(None, w!("Tessera Settings")) {
                let _ = ShowWindow(existing, SW_RESTORE);
                let _ = SetForegroundWindow(existing);
                return;
            }
        }
        // Pass on our right to take the foreground so the new window opens in front, not
        // behind (or flashing on the taskbar).
        unsafe {
            let _ = AllowSetForegroundWindow(ASFW_ANY);
        }
        let exe = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("tessera-settings.exe")));
        if let Some(Err(e)) = exe.map(|p| std::process::Command::new(p).spawn()) {
            log(&format!("could not start the settings window: {e}"));
        }
    }

    /// Throw away every group and rebuild them from the item kinds.
    fn reset_groups(&mut self) {
        for id in self.wins.iter().map(|w| w.0).collect::<Vec<_>>() {
            self.destroy_window(id);
        }
        // Portals are not derived from the desktop, so they survive a rebuild.
        self.cfg.fences.retain(|f| f.is_portal());
        for f in &mut self.cfg.fences {
            (f.tabs, f.active_tab) = (Vec::new(), None);
        }
        self.cfg.placements.clear();
        let present = desktop_present(&self.items, &self.portal_keys);
        self.cfg.first_run(&present, shell::primary_work_area(), 96);
        drop(present);
        for id in self.cfg.hosts() {
            self.create_window(id);
        }
        self.dirty();
    }

    fn tray_menu(&mut self) {
        let pt = cursor();
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let profiles = self.cfg.profile_list();
        let cmd = unsafe {
            let (Ok(menu), Ok(sub)) = (CreatePopupMenu(), CreatePopupMenu()) else { return };
            for (i, (_, name, active)) in profiles.iter().enumerate() {
                let label = if i < 9 { format!("{name}\tCtrl+Alt+Shift+{}", i + 1) } else { name.clone() };
                let _ = AppendMenuW(sub, check(*active), 100 + i, PCWSTR(wide(&label).as_ptr()));
            }
            let _ = AppendMenuW(sub, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(sub, MF_STRING, 99, w!("New profile from this layout"));
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Profiles"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, 1, w!("New group"));
            let _ = AppendMenuW(menu, MF_STRING, 7, w!("New folder portal…"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, check(self.hidden), 2, w!("Hide groups\tCtrl+Alt+H"));
            let _ = AppendMenuW(menu, check(PEEK.load(Ordering::Relaxed)), 3, w!("Peek above windows\tCtrl+Alt+Space"));
            let _ = AppendMenuW(menu, check(self.cfg.theme == Theme::Light), 4, w!("Light theme"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, 5, w!("Rebuild groups by kind…"));
            let _ = AppendMenuW(menu, MF_STRING, 6, w!("Settings…"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, 9, w!("Exit Tessera"));
            let _ = SetForegroundWindow(self.main);
            let cmd = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.x, pt.y, self.main, None).0;
            let _ = DestroyMenu(menu);
            cmd
        };
        match cmd {
            1 => self.new_group(None),
            2 => self.set_hidden(!self.hidden),
            3 => self.set_peek(!PEEK.load(Ordering::Relaxed)),
            4 => {
                self.cfg.theme = if self.cfg.theme == Theme::Light { Theme::Dark } else { Theme::Light };
                self.paint_all();
                self.dirty();
            }
            5 => {
                let answer = unsafe { MessageBoxW(Some(self.main), w!("Replace all groups with a fresh set sorted by kind?\n\nYour files are not touched. Group names, colours and positions are discarded."), w!("Tessera"), MB_OKCANCEL | MB_ICONQUESTION) };
                if answer == IDOK {
                    self.reset_groups();
                }
            }
            6 => self.open_settings(),
            7 => self.pick_portal(),
            99 => self.command(Command::ProfileNew { name: None }),
            100.. => {
                if let Some((id, _, _)) = profiles.get(cmd as usize - 100) {
                    self.switch_profile(*id);
                }
            }
            9 => unsafe {
                let _ = DestroyWindow(self.main);
            },
            _ => {}
        }
    }

    fn add_tray_icon(&self) {
        let mut nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.main, uID: 1, uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP, uCallbackMessage: WM_TRAY, hIcon: self.tray_icon, ..Default::default() };
        let tip = wide("Tessera");
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        }
    }

    /// Explorer started (again): everything it owned on our behalf is gone and must be redone.
    fn attach_to_shell(&mut self) {
        shell::show_native_icons(false);
        // Capture the wallpaper only once Explorer has repainted it without its icons.
        self.backdrop_ready = false;
        self.renderer.backdrop = None;
        unsafe {
            SetTimer(Some(self.main), TIMER_BACKDROP, 600, None);
        }
        self.add_tray_icon();
        self.watch_shell();
    }

    /// The monitor arrangement or work area changed: restore that arrangement's layout, or fit the current one.
    fn display_changed(&mut self) {
        let key = shell::display_key();
        if key != self.display {
            self.display = key;
            self.cfg.apply_placement(&self.display, shell::virtual_screen());
        }
        for f in &mut self.cfg.fences {
            f.rect = f.rect.clamp_into(shell::work_area_for(f.rect));
        }
        for id in self.wins.iter().map(|w| w.0).collect::<Vec<_>>() {
            self.place(id);
        }
        self.dirty();
    }

    fn shutdown(&mut self) {
        self.finish_rename(true);
        shell::show_native_icons(true);
        shell::unregister_notify(self.notify_id);
        let nid = NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.main, uID: 1, ..Default::default() };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
        self.cfg.store_placement(&self.display);
        self.save();
    }
}

// ---------------------------------------------------------------- window procedures

unsafe extern "system" fn fence_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Keep groups glued to the desktop no matter what asks them to move in the z-order:
    // every position change is redirected to "just below the lowest foreign window above the desktop".
    if msg == WM_WINDOWPOSCHANGING && !PEEK.load(Ordering::Relaxed) {
        let pos = &mut *(lp.0 as *mut WINDOWPOS);
        let mut anchor = shell::desktop_host().and_then(|host| GetWindow(host, GW_HWNDPREV).ok());
        while let Some(h) = anchor.filter(|h| is_ours(*h)) {
            anchor = GetWindow(h, GW_HWNDPREV).ok();
        }
        pos.hwndInsertAfter = anchor.unwrap_or(HWND_BOTTOM);
        pos.flags &= !SWP_NOZORDER;
        return LRESULT(0);
    }
    let id = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as u32;
    let Some(app) = app().filter(|_| id != 0) else { return DefWindowProcW(hwnd, msg, wp, lp) };
    // Client coordinates include the shadow room; everything else works in group coordinates.
    let margin = app.ui.get(&id).map(|u| u.margin).unwrap_or(0);
    let (lo, hi) = (|lp: LPARAM| lo(lp) - margin, |lp: LPARAM| hi(lp) - margin);
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            let mut pt = cursor();
            let _ = ScreenToClient(hwnd, &mut pt);
            let shape = match app.zone(id, pt.x - margin, pt.y - margin) {
                Zone::Edge(e) if e == EDGE_L || e == EDGE_R => IDC_SIZEWE,
                Zone::Edge(e) if e == EDGE_T || e == EDGE_B => IDC_SIZENS,
                Zone::Edge(e) if e == EDGE_L | EDGE_T || e == EDGE_R | EDGE_B => IDC_SIZENWSE,
                Zone::Edge(_) => IDC_SIZENESW,
                _ => IDC_ARROW,
            };
            SetCursor(LoadCursorW(None, shape).ok());
            return LRESULT(1);
        }
        WM_LBUTTONDOWN => app.mouse_down(id, hwnd, lo(lp), hi(lp)),
        WM_MOUSEMOVE => app.mouse_move(id, hwnd, lo(lp), hi(lp)),
        WM_LBUTTONUP => app.mouse_up(),
        WM_CAPTURECHANGED => app.end_drag(false),
        WM_LBUTTONDBLCLK => app.double_click(id, hwnd, lo(lp), hi(lp)),
        WM_RBUTTONUP => app.right_click(id, hwnd, lo(lp), hi(lp)),
        WM_MOUSEWHEEL => app.wheel(id, ((wp.0 >> 16) & 0xFFFF) as i16 as i32),
        WM_MOUSELEAVE => app.mouse_leave(id),
        WM_DPICHANGED => app.paint(id),
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM => {
            if !shell::menu_message(msg, wp, lp) {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

/// The low-level mouse hook that notices clicks outside the groups. It exists only while an
/// item is selected (its only job is to end the selection), so a desktop with nothing selected
/// has no hook at all. The callback does nothing but post a message.
static OUTSIDE_HOOK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static OUTSIDE_NOTIFY: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// The keyboard hook: installed while the desktop is the active window, so the keys you would
/// press on the native desktop (arrows, Delete, F2, Ctrl+C...) reach the icons in the groups.
/// Keys typed into any other program never pass through it, because it doesn't exist then.
static KEY_HOOK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// Groups hidden with Ctrl+Alt+H: leave the keyboard to the desktop.
static GROUPS_HIDDEN: AtomicBool = AtomicBool::new(false);

/// Modifier bits sent with a key.
const MOD_SHIFT_KEY: isize = 1;
const MOD_CTRL_KEY: isize = 2;
const MOD_ALT_KEY: isize = 4;

fn watch_outside_clicks(main: HWND, on: bool) {
    OUTSIDE_NOTIFY.store(main.0 as isize, Ordering::Relaxed);
    set_hook(&OUTSIDE_HOOK, WH_MOUSE_LL, outside_click, on);
}

/// Follow the active window: keyboard hook on while it is the desktop, off otherwise.
fn watch_desktop_keys(main: HWND) {
    OUTSIDE_NOTIFY.store(main.0 as isize, Ordering::Relaxed);
    set_hook(&KEY_HOOK, WH_KEYBOARD_LL, desktop_key, unsafe { desktop_is_active() });
}

fn set_hook(slot: &std::sync::atomic::AtomicIsize, kind: WINDOWS_HOOK_ID, proc_: unsafe extern "system" fn(i32, WPARAM, LPARAM) -> LRESULT, on: bool) {
    let hook = slot.load(Ordering::Relaxed);
    unsafe {
        if on && hook == 0 {
            let module = GetModuleHandleW(None).ok().map(HINSTANCE::from);
            if let Ok(h) = SetWindowsHookExW(kind, Some(proc_), module, 0) {
                slot.store(h.0 as isize, Ordering::Relaxed);
            }
        } else if !on && hook != 0 {
            let _ = UnhookWindowsHookEx(HHOOK(hook as *mut _));
            slot.store(0, Ordering::Relaxed);
        }
    }
}

/// The desktop itself is the active window. (Not our own windows: a menu or the rename box
/// must get its keys.)
unsafe fn desktop_is_active() -> bool {
    let fg = GetForegroundWindow();
    let mut class = [0u16; 32];
    let n = GetClassNameW(fg, &mut class) as usize;
    matches!(String::from_utf16_lossy(&class[..n]).as_str(), "Progman" | "WorkerW")
}

/// The desktop shortcuts Tessera answers. Everything else (Ctrl+Z, Alt+F4, Win+...) is left to
/// the desktop window as before.
fn is_desktop_key(key: VIRTUAL_KEY, mods: isize) -> bool {
    let (shift, ctrl, alt) = (mods & MOD_SHIFT_KEY != 0, mods & MOD_CTRL_KEY != 0, mods & MOD_ALT_KEY != 0);
    let plain = !ctrl && !alt;
    let letter = (0x41..=0x5A).contains(&key.0) || (0x30..=0x39).contains(&key.0);
    match key {
        VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN | VK_HOME | VK_END | VK_PRIOR | VK_NEXT => plain,
        VK_DELETE | VK_F2 | VK_F5 | VK_ESCAPE | VK_APPS => plain,
        VK_RETURN => !ctrl,                 // Enter opens, Alt+Enter shows properties
        VK_F10 => shift && plain,           // Shift+F10: the context menu
        _ if ctrl && !alt => matches!(key.0, 0x43 | 0x58 | 0x56) && !shift || key.0 == 0x4E && shift, // Ctrl+C/X/V, Ctrl+Shift+N
        _ => letter && plain && !shift,     // type to jump to an icon
    }
}

unsafe extern "system" fn desktop_key(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) && !GROUPS_HIDDEN.load(Ordering::Relaxed) {
        let info = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let key = VIRTUAL_KEY(info.vkCode as u16);
        let down = |k: VIRTUAL_KEY| GetAsyncKeyState(k.0 as i32) < 0;
        let win = down(VK_LWIN) || down(VK_RWIN);
        let mods = (down(VK_SHIFT) as isize * MOD_SHIFT_KEY) | (down(VK_CONTROL) as isize * MOD_CTRL_KEY) | ((info.flags.0 & 0x20 != 0) as isize * MOD_ALT_KEY);
        if !win && is_desktop_key(key, mods) && desktop_is_active() {
            let main = HWND(OUTSIDE_NOTIFY.load(Ordering::Relaxed) as *mut _);
            let _ = PostMessageW(Some(main), WM_ITEM_KEY, WPARAM(key.0 as usize), LPARAM(mods));
            return LRESULT(1); // handled: the desktop window doesn't also get it
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

unsafe extern "system" fn outside_click(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && matches!(wp.0 as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN) {
        let info = &*(lp.0 as *const MSLLHOOKSTRUCT);
        let mut pid = 0;
        GetWindowThreadProcessId(WindowFromPoint(info.pt), Some(&mut pid));
        // Our own windows (groups, menus, the rename box) handle their clicks themselves.
        if pid != std::process::id() {
            let main = HWND(OUTSIDE_NOTIFY.load(Ordering::Relaxed) as *mut _);
            let _ = PostMessageW(Some(main), WM_CLICK_OUTSIDE, WPARAM(0), LPARAM(0));
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

unsafe extern "system" fn foreground_changed(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    if let Some(app) = app() {
        app.follow_desktop(true);
        watch_desktop_keys(app.main);
    }
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let key = wp.0 as u16;
    let done = match msg {
        WM_KEYDOWN | WM_CHAR if key == VK_RETURN.0 => Some(true),
        WM_KEYDOWN | WM_CHAR if key == VK_ESCAPE.0 => Some(false),
        WM_KILLFOCUS => Some(true),
        _ => None,
    };
    if let (Some(commit), Some(app)) = (done, app()) {
        app.finish_rename(commit);
        return LRESULT(0);
    }
    CallWindowProcW(EDIT_PROC, hwnd, msg, wp, lp)
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    static TASKBAR_CREATED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let taskbar_created = *TASKBAR_CREATED.get_or_init(|| RegisterWindowMessageW(w!("TaskbarCreated")));
    let Some(app) = app() else { return DefWindowProcW(hwnd, msg, wp, lp) };
    match msg {
        WM_TRAY => match lp.0 as u32 & 0xFFFF {
            WM_RBUTTONUP | WM_CONTEXTMENU => app.tray_menu(),
            WM_LBUTTONDBLCLK => app.open_settings(),
            _ => {}
        },
        WM_OPEN_SETTINGS => app.open_settings(),
        WM_CLICK_OUTSIDE => app.clear_selection(),
        WM_ITEM_KEY => app.desktop_key(VIRTUAL_KEY(wp.0 as u16), lp.0),
        WM_COPYDATA => {
            let data = &*(lp.0 as *const COPYDATASTRUCT);
            if data.dwData != COPYDATA_MAGIC || data.lpData.is_null() {
                return LRESULT(0);
            }
            let bytes = std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize);
            return match serde_json::from_slice::<Command>(bytes) {
                Ok(cmd) => {
                    app.command(cmd);
                    LRESULT(1)
                }
                Err(e) => {
                    log(&format!("rejected command: {e}"));
                    LRESULT(0)
                }
            };
        }
        WM_ICON_READY => {
            let data = Box::from_raw(lp.0 as *mut shell::IconData);
            let owner = app.cfg.fences.iter().find(|f| f.items.contains(&data.key)).map(|f| f.id);
            app.renderer.set_icon(*data);
            if let Some(id) = owner {
                app.paint_soon(id);
            }
        }
        WM_PORTAL_READY => app.portal_listed(*Box::from_raw(lp.0 as *mut shell::PortalListing)),
        WM_PICK_PORTAL => app.pick_portal(),
        WM_DROP_ARRIVED => {
            let arrival = Box::from_raw(lp.0 as *mut (String, u32));
            // Group 0 means "dropped into a portal's folder": nothing to file, the folder watcher picks it up.
            if arrival.1 != 0 {
                app.expect_arrival(&arrival.0, arrival.1);
            }
        }
        WM_SHELL_CHANGE => {
            if let Some((old, new)) = shell::notify_rename(wp, lp) {
                if app.cfg.rename_key(&old, &new) {
                    if let Some(mut item) = app.items.remove(&old) {
                        item.key = new.clone();
                        app.items.insert(new, item);
                    }
                    app.renderer.forget_icon(&old);
                }
            }
            SetTimer(Some(hwnd), TIMER_REFRESH, 200, None);
        }
        WM_TIMER => {
            let _ = KillTimer(Some(hwnd), wp.0);
            match wp.0 {
                TIMER_SAVE => app.save(),
                TIMER_REFRESH => {
                    app.refresh();
                    app.refresh_portals(None);
                    // Explorer's view may lag behind the notification; look once more.
                    SetTimer(Some(hwnd), TIMER_REFRESH_LATE, 1200, None);
                }
                TIMER_REFRESH_LATE => app.refresh(),
                TIMER_ZCHECK | TIMER_ZCHECK_LATE => app.follow_desktop(false),
                TIMER_BACKDROP => app.refresh_backdrop(),
                TIMER_ANIM => app.animation_frame(),
                TIMER_PAINT => {
                    for id in std::mem::take(&mut app.repaint) {
                        app.paint(id);
                    }
                }
                _ => {}
            }
        }
        WM_HOTKEY => match wp.0 as i32 {
            HOTKEY_HIDE => app.set_hidden(!app.hidden),
            HOTKEY_PEEK => app.set_peek(!PEEK.load(Ordering::Relaxed)),
            n if (HOTKEY_PROFILE + 1..=HOTKEY_PROFILE + 9).contains(&n) => {
                if let Some((id, _, _)) = app.cfg.profile_list().get((n - HOTKEY_PROFILE - 1) as usize) {
                    app.switch_profile(*id);
                }
            }
            _ => {}
        },
        WM_DISPLAYCHANGE => {
            app.display_changed();
            SetTimer(Some(hwnd), TIMER_BACKDROP, 1500, None);
        }
        WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETDESKWALLPAPER.0 => {
            SetTimer(Some(hwnd), TIMER_BACKDROP, 1500, None);
        }
        WM_SETTINGCHANGE if wp.0 as u32 == SPI_SETWORKAREA.0 => app.display_changed(),
        WM_ENDSESSION if wp.0 != 0 => app.shutdown(),
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM => {
            if !shell::menu_message(msg, wp, lp) {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
        }
        WM_DESTROY => {
            app.shutdown();
            PostQuitMessage(0);
        }
        _ if msg == taskbar_created => {
            app.attach_to_shell();
            SetTimer(Some(hwnd), TIMER_REFRESH, 1500, None);
            SetTimer(Some(hwnd), TIMER_BACKDROP, 2500, None);
        }
        _ => return DefWindowProcW(hwnd, msg, wp, lp),
    }
    LRESULT(0)
}

// ---------------------------------------------------------------- entry point

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    unsafe {
        let running = FindWindowW(MAIN_CLASS, None).ok();
        match arg.as_str() {
            // Recovery after a hard kill: bring Explorer's icons back and leave.
            "--restore" => return shell::show_native_icons(true),
            "--quit" => {
                if let Some(hwnd) = running {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                return;
            }
            // Already running: opening Tessera again (Start menu, desktop shortcut) shows its settings.
            _ if running.is_some() => {
                let hwnd = running.unwrap();
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                // We were just started by the user, so we may hand our right to take the foreground on.
                let _ = AllowSetForegroundWindow(pid);
                let _ = PostMessageW(Some(hwnd), WM_OPEN_SETTINGS, WPARAM(0), LPARAM(0));
                return;
            }
            _ => {}
        }
        if let Err(e) = run() {
            shell::show_native_icons(true);
            log(&format!("fatal: {e}"));
        }
    }
}

unsafe fn run() -> Result<()> {
    std::panic::set_hook(Box::new(|info| {
        shell::show_native_icons(true);
        log(&format!("panic: {info}"));
    }));
    // OLE, not just COM: drag and drop needs it. Implies a single-threaded apartment.
    OleInitialize(None)?;
    let hinst: HINSTANCE = GetModuleHandleW(None)?.into();
    let arrow = LoadCursorW(None, IDC_ARROW)?;
    RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(main_proc), hInstance: hinst, lpszClassName: MAIN_CLASS, ..Default::default() });
    RegisterClassW(&WNDCLASSW { style: CS_DBLCLKS, lpfnWndProc: Some(fence_proc), hInstance: hinst, lpszClassName: FENCE_CLASS, hCursor: arrow, ..Default::default() });
    // A real (hidden) top-level window, not message-only: it must receive broadcasts such as TaskbarCreated.
    let main = CreateWindowExW(WS_EX_TOOLWINDOW, MAIN_CLASS, w!("Tessera"), WS_POPUP, 0, 0, 0, 0, None, None, Some(hinst), None)?;

    let renderer = Renderer::new()?;
    // The program's own icon (resource 1, from assets/tessera.ico), at the tray's size.
    let small = GetSystemMetrics(SM_CXSMICON).max(16);
    let tray_icon = LoadImageW(Some(hinst), PCWSTR(1 as _), IMAGE_ICON, small, small, LR_DEFAULTCOLOR).map(|h| HICON(h.0)).unwrap_or_default();
    let path = data_dir().join("layout.json");
    let loaded = store::load(&path);
    let first_run = loaded.is_none();
    let app = Box::new(App {
        cfg: loaded.unwrap_or_default(),
        path,
        items: HashMap::new(),
        renderer,
        wins: Vec::new(),
        ui: HashMap::new(),
        main,
        hinst,
        icon_tx: shell::spawn_icon_worker(main, WM_ICON_READY),
        drag: Drag::None,
        hidden: false,
        backdrop_failed: false,
        backdrop_ready: false,
        animating: HashSet::new(),
        anim_clock: None,
        display: shell::display_key(),
        notify_id: 0,
        tray_icon,
        repaint: HashSet::new(),
        rename: None,
        rename_item: None,
        pending_rename: None,
        typeahead: (String::new(), None),
        own_drag: None,
        pending_drops: HashMap::new(),
        desktop_dirs: shell::desktop_dirs(),
        portal_tx: shell::spawn_portal_worker(main, WM_PORTAL_READY),
        portal_keys: HashSet::new(),
    });
    APP = Box::into_raw(app);
    let app = &mut *APP;

    app.items = shell::enumerate()?.into_iter().map(|i| (i.key.clone(), i)).collect();
    let area = shell::primary_work_area();
    let dpi = GetDpiForWindow(main).max(96);
    {
        let present: Vec<Present> = app.items.values().map(|i| Present { key: &i.key, name: &i.name, is_folder: i.is_folder }).collect();
        if first_run {
            app.cfg.first_run(&present, area, dpi);
        } else {
            app.cfg.reconcile(&present, area);
            app.cfg.apply_placement(&app.display, shell::virtual_screen());
        }
    }
    for f in &mut app.cfg.fences {
        f.rect = f.rect.clamp_into(shell::work_area_for(f.rect));
    }
    // Portal listings saved in the file are stale by definition; they are listed afresh below.
    for f in app.cfg.fences.iter_mut().filter(|f| f.is_portal()) {
        f.items.clear();
    }
    for id in app.cfg.hosts() {
        app.create_window(id);
    }
    app.attach_to_shell();
    app.refresh_portals(None);
    // If the desktop is already in front (Show Desktop), put the groups above it now: no
    // foreground change will arrive to do it.
    app.follow_desktop(true);
    app.dirty();
    for n in 1..=9 {
        let _ = RegisterHotKey(Some(main), HOTKEY_PROFILE + n, MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, '0' as u32 + n as u32);
    }
    // Out-of-context hook: delivered through this thread's message loop, nothing is injected anywhere.
    SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(foreground_changed), 0, 0, WINEVENT_OUTOFCONTEXT);
    let _ = RegisterHotKey(Some(main), HOTKEY_HIDE, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'H' as u32);
    let _ = RegisterHotKey(Some(main), HOTKEY_PEEK, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_SPACE.0 as u32);

    let mut msg = MSG::default();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
    Ok(())
}
