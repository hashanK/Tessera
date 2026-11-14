# Product definition and design

## MVP

A user installs nothing, runs one exe, and their desktop icons are sorted into titled, translucent groups. They can rearrange everything by dragging, the result persists, and quitting returns the desktop to exactly what it was.

**In the MVP**
- Groups: create, rename, move, resize, roll up, delete, per-group tint / opacity / icon size / sort.
- Every desktop item appears in exactly one group; unassigned items land in **Desktop**.
- First run sorts items by kind (Apps, Folders, Documents, Media, System…).
- New items are filed automatically by the group's rule.
- Native behaviour for items: double-click opens, right-click shows the real shell menu.
- Drag items between groups; drop position sets manual order.
- Persistent layout per display configuration.
- Tray menu, quick-hide and Peek hotkeys.

**Explicitly not in the MVP:** settings window, profiles, folder portals, drag to/from other apps, search, custom rule editor, backdrop blur, keyboard navigation. See `ROADMAP.md`.

## Core journeys

1. **First run** — launch → icons regroup by kind in under a second → nothing to configure.
2. **Tidy** — drag an icon onto another group; drag a title bar to move; drag an edge to resize; double-click a title to roll up.
3. **New download lands on the desktop** — it appears in the matching group, or in Desktop.
4. **Focus** — hotkey hides every group; same hotkey brings them back.
5. **Leave** — tray → Exit. Native icons are back, in their old positions.

## Identity

Tessera: a tessera is one tile of a mosaic. Groups are quiet tiles on the wallpaper, not windows. The mark is four rounded tiles, one accented.

Principles: the wallpaper is the hero; chrome appears on hover; one accent colour; motion under 160 ms; nothing glows.

## Tokens (at 96 DPI; scaled by monitor DPI)

| Token | Value |
|---|---|
| `radius.group` | 10 |
| `radius.item` | 6 |
| `space.pad` | 8 |
| `title.height` | 30 |
| `font.family` | Segoe UI Variable Text → Segoe UI |
| `font.title` | 12.5 px, semibold, 85 % opacity |
| `font.label` | 12 px, regular, 2 lines, centred, ellipsis |
| `icon.sizes` | 32 / 48 / 64 |
| `cell` | icon + 28 wide, icon + 44 high |
| `resize.grip` | 6 px edge band |
| `surface.dark` | #14161A at 62 % |
| `surface.light` | #F4F5F7 at 72 % |
| `border` | white 10 % (dark) / black 10 % (light) |
| `hover` | white 9 % |
| `selected` | accent 32 % + accent 60 % 1 px outline |
| `accent` | #5B8DEF (default); per-group tints: slate, blue, teal, green, amber, rose, violet |
| `motion.rollup` | 140 ms ease-out |

## Interaction rules

- Title bar: drag moves, double-click rolls up, right-click opens the group menu.
- Edges and corners resize; groups snap to the work-area edge and to each other within 8 px.
- Item: click selects, double-click opens, right-click opens the shell menu, drag past 5 px starts a move with a ghost icon; the target group shows an insertion bar.
- Wheel scrolls a group whose content overflows; a 3 px scroll thumb appears only then.
- Groups never take focus and never rise above application windows unless Peek is on.
