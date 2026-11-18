# Roadmap

Status key: ✅ done · 🔨 in progress · ⬜ not started

## Phase 1 — Research ✅
Competitive analysis and feasibility probe: `docs/research.md`, `spikes/probe`.

## Phase 2 — Product definition ✅
MVP scope, journeys, architecture and decision log: `docs/design.md`, `docs/architecture.md`.

## Phase 3 — UX/UI
- ✅ Identity, tokens, interaction rules (`docs/design.md`)
- ✅ Settings window (on-demand WebView2 process; groups, appearance, behaviour, about)
- ✅ Appearance editor with live preview (in the window and on the desktop), theme library
- ⬜ Settings: configurable hotkeys, custom rule editor
- ✅ App icon (`assets/icon.png`), embedded in the executables at 16–256 px; also used for the tray, settings window and installer

## Phase 4 — MVP 🔨
See the status table in `README.md`, which is kept in step with the code.
- ✅ Groups, drag between groups, persistence, per-group customisation, native item behaviour, auto-filing
- ⬜ Roll-up and move animations
- ⬜ Themed rename box and menus (currently stock Win32 controls)
- ✅ Desktop keyboard shortcuts: arrows, type-to-find, Enter, F2, Delete, Ctrl+C/X/V, Ctrl+Shift+N, Alt+Enter, Shift+F10, F5, Esc
- ✅ Multi-select: Ctrl/Shift+click, Ctrl+A, Shift+arrows; drag, delete, copy, cut, open, menu and Properties on all
- ⬜ Rubber-band selection, selection across groups, undo for Tessera's own file operations
- ⬜ Refresh icons when a shortcut's icon changes

## Phase 5 — Optimisation
- ⬜ Benchmark idle CPU, working set, start-up, refresh at 50 / 500 / 5 000 items
- ⬜ Virtualised icon cache (LRU) for very large desktops
- ⬜ Multi-monitor and mixed-DPI test matrix
- ⬜ Explorer-restart and display-change soak tests

## Phase 6 — Advanced
- ✅ OLE drag and drop to and from Explorer and other apps: files, browser links, drop onto folders and the Recycle Bin
- ⬜ Drag and drop extras: drop onto an app icon to open with it, plain text and images, multi-select drag out
- ✅ Folder portals: live listing, browse into subfolders, drag and drop in and out
- ⬜ Portal extras: breadcrumb path, per-portal filters, benchmark and virtualise very large folders
- ✅ Tabbed groups: combine, add, switch, reorder, split out; equal-width or fit-to-name tab styles
- ⬜ Tab extras: drag a tab out of the strip to split it, scroll-wheel over tabs to switch
- ✅ Profiles: independent layouts, copy-as-snapshot, tray menu, hotkeys, settings page
- ⬜ Profile extras: switch automatically by display configuration or running app, configurable hotkeys
- ⬜ Rule editor (extension, name pattern, shortcut target, age)
- ⬜ Search across groups
- ⬜ Double-click empty desktop to hide (opt-in mouse hook)
- ✅ Appearance system: materials (tinted, acrylic, frosted, Mica-like, gradient, solid), borders, shadows, title bar, typography, icon effects, per-group looks, built-in and custom themes, import/export, low-effects mode
- ✅ Hover and selection motion (None, Fade, Grow, Float, Pop; None, Fade, Pulse, Pop), following the Windows animation setting
- ✅ Wallpaper colours: palette from the wallpaper, apply one to every group or new groups, quick picks in the editor
- ✅ Follow the wallpaper: groups take its most prominent colour whenever it changes
- ⬜ Appearance extras: a wallpaper slideshow being noticed straight away, per-tab looks
- ⬜ Keyboard navigation and accessibility (UI Automation providers)
- ✅ Installer: per-user, Start menu, Settings → Apps uninstall, start with Windows, silent mode
- ⬜ Code signing (removes the SmartScreen warning), winget manifest, update check
