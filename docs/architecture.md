# Architecture and decision log

## Shape

```
tessera.exe  (one process, one UI thread, one icon worker thread)
│
├─ tessera-core      pure Rust, no OS calls, unit-tested
│    model           Config, Fence, Style, Rect, per-display placements
│    layout          grid metrics, hit-testing, scroll limits
│    rules           item classification, matchers, first-run grouping
│    store           versioned JSON, atomic write, .bak fallback
│
└─ tessera (app)
     shell           desktop enumeration (IFolderView), change notifications,
                     context menus, launch, icon extraction worker
     render          Direct2D/DirectWrite → 32-bpp DIB → UpdateLayeredWindow
     fence           one window per group: input, drag, resize, scroll, roll-up
     app             state, tray, hotkeys, native-icon-layer guard, persistence
```

```
tessera-settings.exe  (on demand; exits when its window closes)
     WebView2 host + ui/index.html  ──WM_COPYDATA commands──▶  tessera.exe
                                    ◀──────reads layout.json──
```

The settings UI is a **separate, on-demand process**; the organiser itself never loads a web view.

## Decisions

### ADR-1 — Own the icon layer instead of corralling native icons
Hide `SysListView32` while running; render items ourselves from shell data.
*Why:* out-of-process we cannot draw between wallpaper and icons, hide single icons, or learn when the user moves one; the in-process alternative puts our bugs inside Explorer. Measured in `spikes/probe`.
*Cost:* no free-floating icons (the **Desktop** group replaces them); we must reproduce selection, drag and keyboard behaviour ourselves.
*Guard:* files and native icon positions are never written, so exiting is a complete undo.

### ADR-2 — Unowned layered windows anchored directly above the desktop window, one per group
*Why:* the only tested attachment that renders, takes input and survives Explorer restarts. Child-of-Progman does not compose; owned-by-Progman dies with Explorer.
*How:* every `WM_WINDOWPOSCHANGING` on a group is rewritten to "insert below the lowest foreign window above the desktop host". Show Desktop lifts the desktop host over all normal windows; an out-of-context `EVENT_SYSTEM_FOREGROUND` hook (plus two one-shot re-checks at 250 ms and 1.5 s, because the shell reorders late) puts the groups back on top of it. No polling, nothing injected.
*Rejected on the way:* plain `HWND_BOTTOM` (covered in Show Desktop) and "raise while the desktop has the foreground" (our own menus and stray focus changes dropped the groups behind the raised desktop).
*Risk:* depends on the desktop host being the window that contains `SHELLDLL_DefView`; both the `Progman` and `WorkerW` layouts are handled, only the `Progman` one is tested.

### ADR-3 — Static rendering through UpdateLayeredWindow
Direct2D DC render target draws into a DIB only when something changes. No swap chain, no composition timer, zero idle GPU/CPU. Moving a group is a `SetWindowPos` with no repaint.
*Trade-off:* no DWM backdrop blur on layered windows (confirmed by `spikes/blur`). Blur is drawn from a wallpaper capture instead: see ADR-16.

### ADR-4 — Rust core, native Win32 shell, web UI only for settings
Rust + `windows` crate for the organiser: direct COM access, no runtime, small binary. Tauri/WebView2 was evaluated for the whole app and rejected for the desktop layer (a web view per group, or one giant transparent web view, costs 100 MB+ and cannot be bottom-pinned cleanly). It stays the candidate for the settings window, launched on demand so its memory is only paid while open. .NET/WPF was rejected for idle footprint and start-up time.

### ADR-5 — Items are identified by shell parsing name
Full path for files, `::{CLSID}` for virtual items (This PC, Recycle Bin). Renames arrive as `SHCNE_RENAMEITEM` with both names and are remapped so an item keeps its group.

### ADR-6 — Event-driven, coalesced refresh
`SHChangeNotifyRegister` on the desktop; events arm a 200 ms one-shot timer; one re-enumeration and a diff. No polling anywhere. Config writes are debounced the same way and written atomically (`.tmp` → rename, previous file kept as `.bak`).

### ADR-7 — Layout per display configuration
Group rectangles are stored under a key derived from the monitor set (`1920x1080@0,0|…`). On `WM_DISPLAYCHANGE` the matching placement is restored, or the current one is clamped into the new work area and saved under the new key.

### ADR-8 — Single UI thread; blocking shell work goes to a worker
Icon extraction (`IShellItemImageFactory`) runs on a worker thread and posts results back. The UI thread never waits on the file system.

### ADR-9 — Settings window: wry + WebView2 in its own process, not Tauri
A second executable hosts one embedded HTML file in a WebView2 through `wry`/`tao`.
*Why not Tauri:* its value is the bundler, updater and plugin system; this window needs none of them, and Tauri would add an npm toolchain and a build step for a single page. `wry` is the web-view layer Tauri itself uses.
*Why a web view at all:* a settings page is forms and lists, which HTML does well and quickly; hand-building those controls in Direct2D would be weeks of work for no user benefit.
*Cost:* about 168 MB across 7 processes while the window is open (measured), zero when closed. The organiser's own footprint is unaffected.

### ADR-10 — One writer: settings sends commands, never edits the layout file
The organiser is the only process that writes `layout.json`. The settings window sends JSON commands over `WM_COPYDATA` (`tessera_core::ipc`); the organiser validates, applies, saves synchronously, and the window re-reads the file. It also re-reads whenever it regains focus, since the user may have dragged things meanwhile.
*Why:* two writers with a debounced save would lose changes. Commands are a serde-tagged enum, so unknown or malformed input is rejected; out-of-range values are clamped. `Config::apply` is pure and unit-tested.
*Bonus:* the same channel is a scripting interface (`tessera-settings --send`).
*Limit:* any process in the user's session can send commands. They can restyle, rename, regroup or quit — never touch files — so this is accepted.

### ADR-11 — One drag path: OLE for everything, the shell's data object and file engine
Dragging an icon calls `SHDoDragDrop` with the data object the shell itself builds for that item; every group window is a registered `IDropTarget`. The earlier private mouse-capture drag and its ghost window are gone.
*Why:* one mechanism covers reorder, regroup, drag out to any program and drop in from any program, with the shell's own cursors and drag image. Using the shell's data object means targets see exactly what they would see from Explorer.
*Own icon dropped on a group:* a pure regroup. `Drop` reports `DROPEFFECT_NONE` so nothing downstream can treat it as a file move.
*Foreign files dropped on a group:* desktop semantics (move within a drive, copy across, Ctrl/Shift override). The transfer runs through `IFileOperation` on a worker thread — Explorer's progress, conflict and undo behaviour, and neither our UI thread nor the source program waits on it. An `IFileOperationProgressSink` reports each file's real destination (so a "keep both" rename is still tracked); when the change notification delivers the item it is filed in the group it was dropped on rather than by rule. A move reports `DROPEFFECT_NONE` so the source does not also delete the originals.
*Links:* `UniformResourceLocatorW`, or plain text that is a single http(s) address, becomes a `.url` file on the Desktop, named from the browser's `FileGroupDescriptorW` title or the host name.
*Dropping onto an item:* over the middle of a folder or system item (Recycle Bin, This PC…), the drag is forwarded to that item's own shell `IDropTarget`, so the result is whatever Explorer would do. Only folders and system items qualify — app shortcuts are excluded so that reordering never launches a program by accident.
*Limits:* plain text and images are refused; dropping onto an app icon to "open with" is not offered.

### ADR-12 — A profile is a complete, independent layout
`Config` holds the on-screen layout in `fences`/`placements` plus a list of stored `Profile`s; switching swaps them. Group membership is per profile, not shared.
*Why:* "Work" and "Gaming" differ in which groups exist and what is in them, not only in where they sit; a shared membership table could not express that. The existing reconcile step already handles a layout that has been away while the desktop changed, so a stored profile needs no upkeep until it is switched to.
*New profile = copy of the current one* (with fresh group ids), which doubles as the snapshot feature: copy, experiment, switch back.
*Cost:* filing an icon in one profile does not file it in the others.

### ADR-13 — Folder portals are groups whose listing comes from a folder
A group with `portal: Some(path)` is excluded from all desktop bookkeeping (reconcile, regrouping, the catch-all). Its listing is produced by a dedicated worker thread (`IShellFolder::EnumObjects`), so a slow or dead network path cannot stall the UI thread, and the folder is added to the same `SHChangeNotifyRegister` watch as the desktop. Requests for the same portal are coalesced; a listing that arrives after the portal has been browsed elsewhere is discarded.
*Browsing:* opening a folder inside a portal re-points the portal (runtime state only); back never goes above the portal's own folder.
*Drag and drop:* anything involving a portal is a real file transfer through the same `IFileOperation` path, with the portal's current folder as destination. Only desktop-group to desktop-group is a pure regroup.
*Limits:* 1,000 items per listing; no manual order; listing cost for huge folders is not yet measured.

### ADR-14 — A tab is a whole group shown inside another group's window
A group lists other groups in `tabs` and remembers which one is in front (`active_tab`). Each tab stays a complete group — items, sort, rule, portal folder — so reconcile, rules, portals and drag and drop needed no new concepts. Only the host has a window; it owns the rectangle, look, roll-up and tab style.
*Why not tabs as a list inside one group:* every place that reads a group's items would have needed a second level, and a portal could not have been a tab.
*Moving the first tab* (or closing it) transfers the window to the new first tab: rectangle, look and remembered positions move with it.
*Input:* the renderer returns where each tab was drawn, so hit-testing matches the "fit to name" layout exactly. A tab comes forward on press; dragging from it moves the group. Dragging an icon over a tab's name brings that tab forward, and dropping there files the icon in it.

### ADR-15 — Our own small installer instead of Inno Setup, NSIS or MSI
`tessera-setup.exe` is a workspace crate that embeds the release builds with `include_bytes!` and uses standard Windows task dialogs. It installs per user into `%LOCALAPPDATA%\Programs\Tessera`, so there is no administrator prompt, which matches where the app already keeps its data (`%APPDATA%`) and its startup value (`HKCU\…\Run`).
*Why:* installing amounts to copying three files, creating two shortcuts and writing two registry keys. A third-party installer would add a toolchain to download and a script language to maintain for that. Building our own keeps `cargo` as the only build tool and lets the installer close a running Tessera cleanly, so Explorer's icons come back before files are replaced.
*How:* a running executable is renamed aside and replaced, never overwritten in place. The uninstaller is a copy of the setup program. It removes everything, then hands its own file and the empty folder to a hidden `cmd` that deletes them once it exits.
*Icons:* the build script turns `assets/tessera.ico` into a `.res` file (icon plus version block) that the MSVC linker embeds directly, so no resource compiler is needed. The tray icon, the settings window icon and the installer dialogs all load that same resource.
*Not done:* code signing, which is what removes the SmartScreen warning; automatic updates; a machine-wide install.

### ADR-16 — Appearance: one `Style` per group, themes as named styles, blur from the wallpaper
**Model.** Everything about a group's look lives in its `Style` (`tessera_core::look`): material, colours, gradient, border, shadow, title bar, typography, icon effects, radius and spacing. Every field has a serde default equal to the original look, so older layouts load unchanged and no migration step was needed (the version number is unchanged). `Style::sanitize` clamps every value; it runs on everything that arrives from outside (commands, imported themes), so the renderer can trust a style. A theme is just a named `Style`: the built-in ones are code (`builtin_themes`), custom ones are stored in `Config::custom_themes`. They therefore travel with backups and profile switches with no extra work. Applying a theme keeps the group's icon size.
**Blur.** Windows' own blur cannot be used. `spikes/blur` tried the accent policy (blur and acrylic) and `DWMWA_SYSTEMBACKDROP_TYPE` on a per-pixel-alpha layered window like a group. All three drew an opaque grey rectangle with square corners and showed none of the wallpaper. But groups only ever sit on the desktop, so what is behind them is the wallpaper. Tessera captures it once with `PrintWindow` from the window Explorer paints it in, with the icon view hidden so no icons are captured, and stores it at a quarter of its size (~0.5 MB at 1080p). Each paint blurs only the group's own piece: three box-blur passes approximate a Gaussian, and the result is used as a bitmap brush clipped to the rounded panel. Acrylic, Mica and Frosted are three recipes on top of that: blur radius, tint, veil and grain. There are no continuous captures. The wallpaper is captured again only on `SPI_SETDESKWALLPAPER`, a display change or an Explorer restart. The capture waits until Explorer's icons are hidden and Explorer has had about 0.6 s to repaint the wallpaper under them, and it is refused while those icons are visible. An early version captured during start-up, before the icons were hidden, and showed the old icon layout through the blur.
**Shadows** need room outside the group. The window grows by a margin on every side (`Metrics::margin`). The group's stored rectangle, snapping, hit-testing and drag and drop are unchanged; only window placement and the conversion of mouse coordinates know about the margin. Each shadow is drawn at half resolution, blurred once and cached per size and setting. It is cut away under the group, so translucent groups aren't darkened by their own shadow.
**Cost.** Repaints happen only when something changes, as before. Groups with a wallpaper material also repaint while being dragged, because what is behind them changes. Measured repaint times: 6.5 ms for a 624×528 group with 39 icons (Acrylic plus shadow), 1.6–5 ms for smaller ones. Idle CPU is still 0.0 ms over 15 s. Private memory rose from 18.1 to 23.9 MB (wallpaper capture, shadow cache, kept icon pixels for recolouring). *Low effects* turns off blur, grain and shadows globally.
**Settings.** The editor previews in HTML, using the same colour and size rules as the renderer and the real wallpaper for wallpaper materials. While "On the desktop" is on, the draft is also sent to the organiser as a `look` command at most every 60 ms. Those messages are marked quiet, so the page isn't redrawn under the slider. Cancel sends the original style back.
*Limits:* the backdrop is the wallpaper, not whatever is really behind a group, so a group over another group shows the wallpaper; animated wallpapers appear frozen; a wallpaper slideshow may not be noticed until the next display change. With multiple monitors the capture covers the window Explorer paints all wallpapers in (not yet tested). *Motion* (added later): hover and selection animate per item (0..1 progress with easing, plus a one-off ripple or bounce on selection). A 15 ms frame timer runs only while something is moving and re-arms itself frame by frame, so a hover costs about a fifth of a second of repaints and then nothing. Motion is off when Windows "Animation effects" is off and in low-effects mode.

### ADR-17 — A mouse hook only while something is selected
Clicking the bare desktop must clear the selection, as it does on the native desktop, but Windows tells no one else about a click on the desktop window. So while an item is selected, and only then, Tessera installs a low-level mouse hook (`WH_MOUSE_LL`). Its callback only posts a message when a button goes down over a window of another process. The selection then clears and the hook is removed. With nothing selected, there is no hook.
*Rejected:* a permanent hook (system-wide cost for a rare event), and the foreground-change event, which doesn't fire when the desktop was already in front.
*Keys:* a keyboard hook (`WH_KEYBOARD_LL`) exists only while the desktop itself is the active window. It is switched on and off by the foreground-change event and re-checked with the existing desktop z-order checks, which also covers a desktop that was already active at start-up. It answers only the native desktop's own shortcuts (arrows, Home, End, Page Up/Down, typing to find, Enter, Alt+Enter, F2, F5, Esc, Delete, Shift+Delete, Shift+F10, Menu, Ctrl+C/X/V, Ctrl+Shift+N), passes everything else (Ctrl+Z, Alt+F4, Win+…) through, and never acts while a menu, the rename box or another program is active. File actions use the item's own shell verbs (`delete`, `copy`, `cut`, `properties`), `IShellFolder::SetNameOf` for renames (hidden extensions stay hidden) and `IFileOperation` with rename-on-collision for paste, as Explorer does. Clicking an icon makes the desktop the active window, as clicking the native desktop does. Setting `TESSERA_DEBUG_KEYS=1` logs each key and the resulting selection.
*Limit:* Windows keeps undo per program, so Ctrl+Z on the desktop doesn't undo a delete, rename or paste made through Tessera.

### ADR-18 — Multi-select within one group
Each window keeps a set of selected positions, an anchor (where a Shift range starts) and a focus (what F2 and the arrows act on). Only one window has a selection at a time, as on the native desktop, where there is one view: selecting in another group starts afresh. Clicks follow Explorer. Ctrl toggles; Shift ranges from the anchor; Ctrl+Shift adds a range; a plain click on an icon that is already selected keeps the selection until the button comes up, so the whole selection can be dragged, and a following double-click opens them all.
*Acting on several:* items on the desktop often live in two folders, the user's Desktop and the Public Desktop, and shell context menus need one parent folder. So, for several items:
- deleting uses one `IFileOperation` (one confirmation and one progress window);
- Copy and Cut put a file list plus "Preferred DropEffect" on the clipboard, as Explorer does;
- dragging uses the parent folder's own data object when they share a folder, otherwise one built from absolute ids;
- the right-click menu and Properties use the combined shell menu when the items share a folder, otherwise they fall back to the focused item.
Dropping several of our own icons on a group regroups them in their on-screen order.
*Not built:* rubber-band selection; a selection spanning groups.

### ADR-19 — Colours from the wallpaper
The palette is k-means (8 centres from a deterministic start, 12 rounds) over a reduced copy of the wallpaper. Look-alike clusters are merged, and up to 6 are ranked by share. The settings window computes it in JavaScript from its preview copy. The organiser computes it in Rust (`look::palette`) from the wallpaper capture it already keeps for blur materials, using the same steps. *Follow my wallpaper* runs after every wallpaper re-read (start-up, and about 1.5 s after a wallpaper or display change). It sends one `recolor_all`, which changes only the background colour, the accent and the gradient stops of each group, and does nothing when the groups already have that colour. No timers, so the idle cost is still zero. A new wallpaper is noticed two ways: the `SPI_SETDESKWALLPAPER` broadcast, and a thread waiting in `RegNotifyChangeKeyValue` on `HKCU\Control Panel\Desktop`. The registry catches Windows 11's Settings app and the slideshow, which don't reliably broadcast. The settings window watches the same key, so its wallpaper preview and palette follow while it is open.

## Recovery behaviour

| Event | Behaviour |
|---|---|
| Normal exit | Native icon layer shown again, tray icon removed, config flushed. |
| Panic | Panic hook restores the native icon layer before aborting. |
| Hard kill | Native icons stay hidden until `tessera --restore`, the next launch, or an Explorer restart. |
| Explorer restart | `TaskbarCreated`: re-hide native layer, re-add tray icon, re-register notifications, re-enumerate. Group windows are untouched. |
| Corrupt config | Fall back to `.bak`; if that fails, rebuild groups from rules. |

## Performance budget

First measurements: release build, 46 items in 4 groups, Windows 11 build 26300. Proper benchmarking is Phase 5.

| Metric | Target | Measured |
|---|---|---|
| Idle CPU | 0 % — no timers while nothing changes | 0.0 ms of CPU over 15 s |
| Private bytes | < 25 MB | 18.1 MB before the appearance system; 23.9 MB with wallpaper materials and shadows in use |
| Working set | < 25 MB | 55.8 MB — over target; mostly shared shell and Direct2D DLL pages, not yet investigated |
| Start to first group visible | < 300 ms | under ~650 ms (measured with a coarse polling script, so an upper bound) |
| Binary size | — | 377 KB |
| Refresh after a desktop change | < 50 ms for 50 items | not measured (enumeration alone was 12 ms in the probe) |
