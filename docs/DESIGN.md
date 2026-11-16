# Design: how the program is meant to work

This document describes the intended behaviour of the program, the reasons behind it, and where it
is going. It is the reference that code, tests and reviews are measured against. Sections marked
**(planned)** are not implemented yet.

Working name: *Tilework* (the code still says `cosmictile`; the rename happens before the first
public release).

## 1. Vision

Windows has no real automatic tiling. Snap Layouts and PowerToys FancyZones need manual work for
every window. The COSMIC desktop (Pop!_OS 24.04) shows how pleasant it can be: windows arrange
themselves, you rearrange them with the mouse, and there is no configuration to learn.

The goal is **that experience on Windows, as a small, free, trustworthy program**:

- Open a window and it lands in a sensible place. Close it and the others take its space.
- Rearrange everything by dragging, with a live preview of exactly what will happen.
- No gaps, a thin frame, no "layout language" to learn.
- Switch it on and off in one click, per virtual desktop.
- It never gets in the way: it cannot lose a window, cannot leave Windows in a changed state, and
  stays out of full-screen apps and dialogs.

Non-goals: scripting languages, a status bar, tiling-by-keyboard as the only way to work, replacing
the shell, workspaces that hide windows. The mouse is the primary tool; keyboard shortcuts are an
optional addition **(planned)**.

## 2. Core concepts

**Surface** – one virtual desktop on one monitor. Every surface has its own layout. Surfaces never
influence each other; moving a window to another monitor or desktop moves it to that surface's
layout.

**Zone tree** – the layout of a surface. The tree is made of containers and windows:

- a *container* holds 2–4 zones side by side (horizontal) or stacked (vertical);
- a *zone* is either a window or another container;
- each zone has a weight; zones stay equal until the user resizes one.

Invariants (checked by tests): no container has fewer than 2 or more than 4 zones, a zone is never
empty, the zones always cover the surface completely without gaps or overlaps.

**Work area** – the monitor without the taskbar. Windows are placed so that their *visible* edges
match the zone (the invisible resize borders of Windows 10/11 are compensated).

## 3. Automatic placement

A new window is placed without questions:

1. It becomes a new **full-length column** next to the focused window's column, until the surface
   has 4 columns.
2. After that, columns are split in two (at most **2 windows per column** by default), starting at
   the focused window's column.
3. Anything denser (3–4 zones in one column, deep splits) is the user's choice through drag & drop;
   the program never assumes it.
4. If a window cannot be placed without squeezing anybody below their minimum size, the spot with
   the fewest violations is used. A window is never left out of the layout (except full-screen
   windows, see 8).

Closing, minimising or moving a window to another desktop removes it; its neighbours grow into the
space. Restoring it places it like a new window.

## 4. Drag & drop

While a tiled window is dragged, an overlay shows **the complete layout that would result** (all
windows as thin frames, the dragged window filled). The overlay is computed by applying the move to
a copy of the tree, so it is exactly what happens on release.

Drop targets, by cursor position over the window below:

| Cursor on … | Result |
|---|---|
| centre of a window (28–72 %) | swap the two windows |
| edge band of a window | split **that window only** on that side; every other window keeps its place |
| outer 36 px of the screen | new full-length zone along that screen edge |
| empty monitor | the window fills it |
| no valid target / would squeeze a window too small | nothing; the window snaps back |

If the target container is already full (4 zones), the drop splits only the target's zone.
Dropping onto another monitor inserts the window into that monitor's layout.

## 5. Resizing

Dragging a window edge moves the **divider** behind it. Dividers belong to zones, not windows:
dragging the edge of one window inside a stack moves the divider for the whole stack. Outer screen
edges have no divider. After a manual resize, sizes are kept (proportionally) when windows are
added or removed; there is an "equalise" operation in the tree **(UI planned)**.

## 6. Minimum sizes

Some apps refuse to get small (Chrome ≈ 502 px wide, Discord ≈ 802×501, ChatGPT ≈ 480×600). The
program does not fight them:

- Every zone has a global floor (default 260×160 px, configurable) so that part of the title bar
  always stays grabbable.
- When a window ends up larger than its zone twice in a row, its real minimum is **learned** and
  the layout gives its zone exactly that size; the others share the rest. The layout engine
  distributes space by weight but never below a zone's minimum.
- If the minimums of the windows on a surface cannot fit at all, the demanding windows are
  re-seated (largest first) where they squeeze least; only then can windows overlap slightly.

Physical limit: Windows does not let an app go below its own minimum; no tiling program can change
that. Rearranging (for example moving Discord into a column where its width hurts less) is the way
out.

## 7. Virtual desktops and monitors

- Each surface is tiled independently; the layout of a desktop you left is kept and verified when
  you come back.
- Tiling is switched **per virtual desktop** (tray icon click, `Win + Y`, tray menu). Desktops
  with tiling on and off can coexist; switching never changes the off desktop.
- Windows that Windows itself hides (cloaked, ghost UWP frames on the shown desktop) are ignored.

## 8. Things the program leaves alone

- **Full-screen windows** (anything covering the whole monitor: F11, video, games) are never pulled
  back into a tile. When the window leaves full screen it returns to its tile.
- Dialogs, tool windows, pop-ups, menus, tooltips, the taskbar and the desktop.
- Minimised windows (they keep no tile).

## 9. Windows' own snapping

Windows Snap fights with any tiling program (dragging to a screen edge triggers it). While tiling
is on for the shown desktop, the program switches Snap off through `SystemParametersInfo`
(`SPI_SETWINARRANGING`) **for the running session only** (nothing is written to the registry), and
restores the previous state on exit and on desktops where tiling is off. `--restore-snap` restores
it manually. A crash can therefore never leave Snap off after the next sign-in.

## 10. Focus follows mouse

The tiled window under the cursor receives the keyboard focus after the cursor has rested on it for
a delay (default 300 ms, configurable, can be turned off). It is deliberately conservative: it only
acts when the focused window is one of ours or the desktop, never while a mouse button is held,
during drags, while menus, the Start menu, dialogs or any other unmanaged window have the focus.

## 11. Architecture

```
main.rs      argument handling (--dump, --filter, --restore-snap), starts app::run
config.rs    key=value settings file (%LOCALAPPDATA%\<name>\config.ini), tolerant parser
tree.rs      pure logic, no Windows calls: zone tree, placement policy, drop zones,
             minimum-aware layout, previews. Fully unit-tested.
platform.rs  thin Windows layer: window/monitor/virtual-desktop queries, placement, Snap control
app.rs       event loop: WinEvent hooks -> debounced reconcile -> apply; drag overlay; tray;
             hotkey; focus follows mouse
```

Key design decisions:

- **Reconcile instead of event bookkeeping.** Window events only schedule a short, debounced
  reconcile. It compares the windows that really exist with the trees and fixes the difference. A
  missed event is healed by a 1.5 s safety poll.
- **Logic is pure.** Everything that decides where windows go lives in `tree.rs` without Windows
  dependencies; the previews are computed with exactly the same code as real moves.
- **Fail safe.** The program only ever moves and resizes windows; it never closes, hides or
  changes them. Settings it changes (Snap, window border colour) are session-only or restored.
- **No dependencies** except the official `windows` crate.

Diagnostics: a log at `%LOCALAPPDATA%\<name>\cosmictile.log` records adds, removals, drops,
learned minimum sizes and desktop switches. `--dump` prints the layout the program would apply,
without touching any window; `--filter TEXT` limits management to matching windows for safe tests.

## 12. Settings

Tray menu and `config.ini`:

| Key | Default | Meaning |
|---|---|---|
| `focus_follows_mouse` | `true` | focus the window under the cursor |
| `focus_delay_ms` | `300` | how long the cursor must rest (50–5000) |
| `min_zone_width` / `min_zone_height` | `260` / `160` | smallest zone |

## 13. Roadmap

Near term (before a public 0.2):

- audit and bug-hunt findings (see `docs/` for the report), regression tests for each bug
- autostart as administrator without a UAC prompt (scheduled task) and a clean uninstall
- the thin colour frame around the focused window as a real 4 px overlay
- persisted learned minimum sizes; per-app overrides and an ignore list
- rename, MIT licence metadata, CI (build + tests on pull requests)

Later:

- keyboard shortcuts (focus/move/resize/toggle) for those who want them
- a settings window instead of the tray submenu and the text file
- smarter multi-monitor handling (hot-plug, DPI changes) and tests through a mockable platform layer
- installer / winget package, signed releases
- optional stacking/tabbed zones

## 14. Open source plan

- Licence: MIT (`LICENSE`).
- Public repository with README, this design document, a contribution guide and an issue template
  that asks for the log file and `--dump` output.
- The tree logic is intentionally independent of Windows so it can be tested and reused.
- Security: the program runs elevated to manage elevated windows. It reads no network data,
  executes nothing from configuration files and only writes its log and settings under
  `%LOCALAPPDATA%`.
