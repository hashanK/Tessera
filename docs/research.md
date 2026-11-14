# Phase 1 — Competitive research and technical feasibility

Date: 2026-11-14. Working title for the product: **Tessera**.

## 1. Competitors

| Product | Model | How it holds icons | What users like | What users complain about |
|---|---|---|---|---|
| **Stardock Fences 6** | Paid. Sold mainly as a 1-year subscription (about $10/yr single PC at launch, multi-device tier above it); permanent licence exists but is hidden behind a link. | Real desktop icons, repositioned inside shaded regions. Runs inside `explorer.exe`. | The reference experience: roll-up, folder portals, rules, tabs, Peek (Win+Space), quick-hide, Chameleon, icon tint. | Subscription fatigue and licence confusion; activation limits; long-running layout bugs (icons bunching, overlapping or re-sorting after resolution/DPI changes or sleep; "auto icon spacing still broken after 5 years"); Explorer CPU spikes (12–40 % reported) and Explorer crash loops, because a fault in Fences is a fault in the shell. |
| **Desktop Frames+** (ex Desktop Fences+, open source, ~680★) | Free, portable. | Own-rendered frames holding shortcut entries; "portal" frames mirror a folder. | Free, portable, very feature-rich (tabs, profiles, notes, rules, search). | Feature sprawl; only user-writable locations work; config migrations between versions; visual polish is uneven. |
| **PecoFence** (open source, Rust + Direct2D, new) | Free. | Hides native icons, renders its own; files stay where they are. | Native and light; settings UI is a WebView2 loaded on demand. | Very young; unsigned builds trip SmartScreen; usage statistics and update checks are on by default. |
| **iTop Easy Desktop** | Freemium with a subscription tier. | Own "boxes". | Best-known free tier; keeps installer-added icons in their box. | Bundled extras (wallpapers, widgets, upsell); a report of hanging on "Initializing…" with every icon gone; leftovers after uninstall. |
| **Nimi Places** | Free, effectively unmaintained. | Containers that show a *folder*; files live in that folder. | Small and responsive. | Crashes, confusing install/uninstall, no rules, doesn't organise the actual desktop. |
| **Portals** | Free, proprietary. | Folder containers only. | "Looks Windows-native", precise placement. | "Links to folders instead of organizing the icons of the desktop" — a different product. |
| **Coodesker** | Paid once. | Own containers. | Repeatedly called lightweight and fast. | Small feature set, little automation. |
| **DesktopOK** | Free. | Doesn't group at all; saves and restores native icon positions. | Solves exactly one problem (layout loss) reliably. | Not an organiser. |

Evidence quality: Stardock's own forums, AlternativeTo reviews, Steam discussions and project READMEs. The search tooling did not surface Reddit threads directly, so "Reddit sentiment" here is inferred from the same complaints repeated in those venues, not quoted from Reddit.

## 2. What the market is telling us

1. **Reliability beats features.** The loudest, oldest complaints about the market leader are layout corruption after display changes and instability inherited by Explorer. Nobody complains that Fences lacks features.
2. **Trust is a feature.** Subscriptions, activation limits, default-on telemetry, bundled extras and "my icons vanished" stories are why people look for alternatives.
3. **Free tools pick one of two bad trade-offs:** either they don't manage the real desktop (Nimi, Portals), or they are large WPF/Electron-class apps with uneven polish.
4. **Nobody owns "quiet".** Coodesker is praised just for being light. There is room for a tool whose headline is *does nothing while idle, never touches your files, exits clean*.

### Positioning for Tessera

- Your files never move and the native layout is never rewritten. Quit Tessera and the desktop is exactly as it was.
- Runs outside Explorer. If Tessera dies, Explorer does not.
- Layouts are stored per display configuration, so docking/undocking and resolution changes restore instead of scramble.
- Free, no account, no telemetry, portable mode.

## 3. Technical feasibility (measured on this machine: Windows 11 26H2 build 26300, 1920×1080, 46 desktop items)

Findings come from `spikes/probe`.

| Question | Result |
|---|---|
| Desktop window tree | `Progman → SHELLDLL_DefView (layered) → SysListView32`. No wallpaper `WorkerW` child by default on this build. These class names are undocumented and have changed before (24H2 broke wallpaper apps). |
| Read desktop items and positions | `IShellWindows::FindWindowSW(SWC_DESKTOP)` → `IFolderView2`. Bind 14.5 ms, enumerate 46 items with names and positions 12.3 ms (cross-process COM, ~0.27 ms/item). Documented, supported path. |
| Write native icon positions | `IFolderView::SelectAndPositionItems` is the only supported way since Windows 10 1809; `LVM_SETITEMPOSITION` is silently discarded. Not exercised here because the chosen design never moves native icons. |
| Hide native icon layer from outside Explorer | `ShowWindow(SysListView32, SW_HIDE)` works, Explorer does not undo it, `SW_SHOWNA` restores. |
| Window as cross-process **child of Progman** | Created, receives clicks, **but is not composed** (invisible). Rejected. |
| Top-level layered window **owned by Progman** | Visible on the desktop, but appears above normal app windows until pushed down, and is destroyed whenever Explorer restarts. |
| Unowned top-level layered tool window pinned just above the desktop window | Hidden behind apps, receives input, survives Explorer restarts. **Chosen.** |
| Show Desktop (Win+D) | On this build the shell lifts `Progman` above every normal window about a second after the toggle, covering bottom-pinned windows (the first probe screenshot was taken too early and wrongly suggested otherwise; Rainmeter's desktop skin is covered too). Solved by anchoring groups to "directly above the desktop window" and re-checking on the foreground-change event. Verified: groups stay visible in Show Desktop and return behind apps afterwards. |
| Per-pixel alpha | `UpdateLayeredWindow` with a premultiplied 32-bpp DIB works. Content is static, so there is no swap chain and no per-frame cost. |

### Two possible architectures

**A. Corral native icons** (what Fences does): draw panels behind the real `SysListView32` and move real icons with `SelectAndPositionItems`.
Free native behaviour, but out-of-process there is no way to draw between the wallpaper and the icons, no event when the user drags an icon, no way to hide one icon, clip a scrolled fence or roll one up without parking icons off-screen, and Explorer re-arranges on every display change. Doing it properly means injecting into Explorer — which is exactly where the incumbent's instability comes from.

**B. Own the icon layer** (chosen): hide the native list while running; each group is our own window that renders items obtained from the shell (names, icons, context menus, launch verbs all come from shell COM, so behaviour stays native). Files and the native layout are never modified.

### Feasibility classification

| Feature | Class | Notes |
|---|---|---|
| Groups: create, move, resize, rename, roll-up, hide | Straightforward | Our own windows. |
| Native context menu, open, properties, rename, delete | Straightforward | `IContextMenu` on the item's PIDL. |
| Detect new/removed/renamed desktop items | Straightforward | `SHChangeNotifyRegister`, event-driven, no polling. |
| Rules / auto-organise, sorting, search | Straightforward | Pure logic over the item list. |
| Profiles, snapshots, per-display-configuration layouts | Straightforward | Layout is our own data. |
| Folder portals | Straightforward | Same renderer pointed at another folder + one more change notification. |
| Drag between groups | Straightforward | Internal. |
| Drag to/from Explorer and other apps | Moderate | OLE `IDropTarget` / `DoDragDrop`. Standard but fiddly. |
| Quick-hide hotkey, Peek above windows | Straightforward | Z-order changes only. |
| Double-click empty desktop to hide | Needs a workaround | Requires a low-level mouse hook; only installed if the option is on. |
| Frosted-glass backdrop | Needs a workaround | Layered windows cannot use DWM backdrop; needs DirectComposition surface + undocumented accent policy. Prototype before promising. |
| Free-floating icons outside any group | Complex | Would mean re-implementing the desktop surface. Replaced by an always-present **Desktop** group that catches unassigned items. |
| Explorer restart recovery | Straightforward | `TaskbarCreated` broadcast; our windows survive because they are unowned. |
| Crash safety of hidden native icons | Needs care | Restore on exit and in the panic hook; `tessera --restore` and next launch recover from a hard kill. |

## Sources

- [Stardock Fences product page](https://www.stardock.com/products/fences/)
- [BetaNews: Fences 6.0 launch](https://betanews.com/2025/06/02/fences-6-0-is-the-essential-desktop-upgrade-for-windows-10-and-11-users-get-it-today/) · [Fences 6.20](https://betanews.com/2025/12/11/stardock-releases-fences-6-20-with-new-rollup-options-and-desktop-fixes/)
- [Fences 6 Upgrade FAQ](https://forums.wincustomize.com/534855/page/1/)
- Stardock forum threads: [auto icon spacing](https://forums.deadmansdrawgame.com/541312), [per-monitor layout breaks after resolution change](https://forums.deadmansdrawgame.com/533167), [icons bunching/overlapping](https://forums.stardock.com/523396/reply/3907004), [conflict with Explorer](https://forums.ashesofthesingularity.com/529217/fences-conflict-with-windows-explorer), [very slow Fences](https://forums.stardock.com/490929), [desktop crashes/repaints](https://forums.sorcererking.com/499379/reply/3775196)
- [AlternativeTo: Fences alternatives and reviews](https://alternativeto.net/software/fences/) · [Nimi Places](https://alternativeto.net/software/nimi-places) · [Portals](https://alternativeto.net/software/portals)
- [Desktop Frames+ / DesktopFences on GitHub](https://github.com/limbo666/DesktopFences)
- [PecoFence on GitHub](https://github.com/DayuanJiang/PecoFence)
- [iTop Easy Desktop Steam discussion](https://steamcommunity.com/app/2620310/discussions/0/4637114871638457580)
- [Raymond Chen: Manipulating the positions of desktop icons](https://devblogs.microsoft.com/oldnewthing/20130318-00/?p=4933) · [1809 change](https://devblogs.microsoft.com/oldnewthing/20211122-00/?p=105948)
- [The Register: 24H2 wallpaper-app compatibility hold](https://www.theregister.com/2025/04/14/windows_11_24h2_hold/)
