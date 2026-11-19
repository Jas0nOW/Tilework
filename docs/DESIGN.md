# Design: how the program is meant to work

This document describes the intended behaviour of the program, the reasons behind it, and where it
is going. It is the reference that code, tests and reviews are measured against. Sections marked
**(planned)** are not implemented yet.

Project name: *Tilework*.

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

**Surface** â€“ one virtual desktop on one monitor. Every surface has its own layout. Surfaces never
influence each other; moving a window to another monitor or desktop moves it to that surface's
layout.

**Zone tree** â€“ the layout of a surface. The tree is made of containers and windows:

- a *container* holds 2â€“4 zones side by side (horizontal) or stacked (vertical);
- a *zone* is either a window or another container;
- each zone has a weight; zones stay equal until the user resizes one.

Invariants (checked by tests): no container has fewer than 2 or more than 4 zones, a zone is never
empty, the zones always cover the surface completely without gaps or overlaps.

**Work area** â€“ the monitor without the taskbar. Windows are placed so that their *visible* edges
match the zone (the invisible resize borders of Windows 10/11 are compensated).

## 3. Automatic placement

New windows are placed cleanly and predictably according to the confirmed V3 layout progression:

1. The **first two windows** share the work area as two full-length main columns (halves).
2. The **3rd and 4th windows** are split vertically inside these existing columns, creating a balanced 2×2 grid.
3. Only with the **5th window** is a 3rd full-length column automatically created. A 6th window splits that 3rd column vertically, etc.
4. **Manual precedence:** If the user manually creates a column (e.g. by dragging a window to the outer screen edge) or rearranges zones, manual layout decisions take precedence over the automatic default. New windows prioritize filling available slots in open columns before generating additional main columns.
5. If every existing container is full (up to 4 zones per container, bounded nesting depth), automatic placement chooses the slot or leaf that minimizes minimum-size violations. Earlier placement choices also account for minima; a window is never left out of the layout (except full-screen windows, see 8).

Closing or moving a window to another desktop removes it; its neighbours grow into the
space. Restoring it places it like a new window.

## 4. Drag & drop

While a tiled window is dragged, an overlay shows **the complete layout that would result** (all
windows as thin frames, the dragged window filled). The overlay is computed by applying the move to
a copy of the tree, so it is exactly what happens on release.

Drop targets, by cursor position over the window below:

| Cursor on … | Result |
|---|---|
| centre of a window (28–72 %) | swap the two windows |
| edge band of a window | split **that window only** nested on that side; every other window keeps its place |
| outer 36 px of the screen | new full-length zone along that screen edge (takes precedence) |
| empty monitor | the window fills it |
| no valid target / would squeeze a window too small | nothing; the window snaps back |

Nested splits work within already divided zones. The tree maintains a bounded hierarchy (maximum nesting depth ≤ 5, container child limit ≤ 4) to ensure flexible yet stable and manageable layouts.
Dropping onto another monitor inserts the window into that monitor's layout. A centre drop across
monitors inserts below the target (trees cannot swap directly). Both source and destination layouts
are previewed. The outer 36 px are measured from the work-area edge, excluding the taskbar.
Release rechecks the cursor, window edges and target existence before committing the move.

## 5. Resizing

Dragging a window edge moves the **divider** behind it. Dividers belong to zones, not windows:
dragging the edge of one window inside a stack moves the divider for the whole stack. Outer screen
edges have no divider. After a manual resize, sizes are kept (proportionally) when windows are
added or removed; there is an "equalise" operation in the tree **(UI planned)**.

## 6. Minimum sizes

Some apps refuse to get small (Chrome â‰ˆ 502 px wide, Discord â‰ˆ 802Ã—501, ChatGPT â‰ˆ 480Ã—600). The
program does not fight them:

- Every zone has a global floor (default 260Ã—160 px, configurable) so that part of the title bar
  always stays grabbable.
- Placement requests are asynchronous and skip hung and minimized windows. A displaced window is
  placed again with bounded retries and backoff. A larger result is considered a possible refusal
  only when its top-left corner matches the target within one pixel. An explicit replacement must
  yield the same larger size, followed by stable observations on separate polls after settling.
  Learned minima are capped at the monitor work area and forgotten when the window disappears.
- If an arrangement violates minima, violating windows alone are considered for repositioning.
  A candidate is accepted only when it reduces violations and retains all windows; unaffected
  subtrees and manual weights are preserved. If no improvement is possible, the layout stays intact.

Physical limit: Windows does not let an app go below its own minimum; no tiling program can change
that. Rearranging (for example moving Discord into a column where its width hurts less) is the way
out.

## 7. Virtual desktops and monitors

- Each surface is tiled independently; the layout of a desktop you left is kept and verified when
  you come back.
- Tiling is switched **per virtual desktop** (tray icon click, `Win + Y`, tray menu). Desktops
  with tiling on and off can coexist. Paused trees preserve their manual weights for the process
  lifetime; enabling reconciles windows that have since closed or moved.
- Desktop identity uses the most common confirmed visible-window ID when at least two windows
  support it. Foreground evidence breaks ties between equally supported IDs. With sparse evidence,
  the shell's current desktop registry value takes precedence; a confirmed sole foreground window
  can serve as a last fallback. Results are cached briefly and invalidated on foreground or
  desktop-switch events. The shell value is undocumented and may change. When identity is unknown,
  placement and hover focus pause and Tilework restores any Snap change it owns.
- A desktop switch or disabling tiling cancels any active drag. Display/work-area notifications
  rebuild surface monitor keys from live windows and trigger a full reconcile.
- Windows that Windows itself hides (cloaked, ghost UWP frames on the shown desktop) are ignored.

## 8. Things the program leaves alone

- **Maximized and full-screen windows** already tiled keep their reserved tile and are left alone until restored, including across minimize/restore. Neighbours keep their positions. A newly adopted maximized window is restored into its tile; genuinely full-screen untracked windows are not adopted. Full-screen detection requires monitor coverage and a style without the normal caption. A normal maximized window on a monitor without a visible taskbar is adopted and restored. A tile matching the whole monitor is still a tile.
- Dialogs, tool windows, pop-ups, menus, tooltips, the taskbar and the desktop.
- Minimized tracked windows keep their reserved tiles while they remain top-level windows; this preserves manual layouts across Win+D.

## 9. Windows' own snapping

Windows Snap fights with any tiling program (dragging to a screen edge triggers it). While tiling
is on for the shown desktop, the program switches Snap off through `SystemParametersInfo`
(`SPI_SETWINARRANGING`) **for the running session only** (nothing is written to the registry), and
restores the previous state on exit and on desktops where tiling is off. `--restore-snap` restores
it manually (enabling Snap and removing the recovery marker). Before disabling Snap, the original
state is read when ownership is taken and flushed to `%LOCALAPPDATA%\Tilework\snap-off.flag` as `1` or `0`. Normal shutdown,
a Rust panic, console control events and session end attempt restoration. A successful restoration
removes the marker; a failed one retains it. On the next managed startup, an existing marker is
recovered before changing Snap again. A forced process kill may leave Snap off until that recovery,
manual restoration or sign-in. Without a usable marker path, Snap is left unchanged.

## 10. Focus follows mouse

The tiled window under the cursor receives the keyboard focus after the cursor has rested on it for
a delay (default 300 ms, configurable, can be turned off). It is deliberately conservative: it only
acts when the focused window is one of ours or the desktop, never while a mouse button is held,
during drags, while menus (including foreground-thread popups), the Start menu, dialogs or any
other unmanaged window have the focus, or while the foreground window is full screen or maximized.
All five common mouse buttons suppress hover focus. A harmless zero-movement input precedes the
foreground request without joining input queues. Windows can still decline it; a failure is not
retried until the cursor moves to another window.

## 10a. Focus border accent overlay

Windows 10/11 apps (especially borderless, Electron, Terminal, and modern custom-rendered windows) frequently ignore DWM window border attributes. Tilework uses a layered, click-through overlay window (`TileworkFocusBorder`) to draw a 3 px accent around the focused tiled window. Eligibility follows the current foreground window and current desktop. Minimized, maximized, fullscreen and untracked windows clear the overlay.

The frame follows the focused window's own location, hide and cloak events (hooked for that window's
process only, to keep the event volume low), so it moves with asynchronous placements and disappears
at once on minimize, maximize, close or cloak instead of on the next poll. Both overlays remember what
they show: an unchanged drag preview or frame costs no window-manager call, a hide of a hidden overlay
is free, and only a new target re-asserts the topmost position. Painting uses the DC's stock brush,
so no GDI object is created per paint. A display change makes the next update unconditional.

## 11. Architecture

```
main.rs      DPI awareness, single instance, CLI and diagnostics
setup.rs     per-user sign-in entry and stable-release checks against the fixed GitHub repository
packaging/   per-user Inno Setup installer, Start Menu shortcut and uninstall registration
config.rs    key=value settings file (%LOCALAPPDATA%\<name>\config.ini), tolerant parser
tree.rs      pure logic, no Windows calls: zone tree, placement policy (V3 progression), drop zones,
             nested splitting, bounded depth, minimum-aware layout, previews
platform.rs  thin Windows layer: window/monitor/virtual-desktop queries, placement, Snap control
app.rs       event loop: WinEvent hooks -> debounced reconcile -> apply; drag preview overlay;
             focus border accent overlay; tray menu (autostart, updates, settings);
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
- **Manual update installation.** Release discovery uses HTTPS and validates the version and expected installer metadata. The application can open the official release page. The standalone installer handles installation and replacement after Tilework exits.
- **Dependencies.** The official `windows` bindings provide Windows APIs, and `serde_json` parses release metadata.

The tray icon is re-added after Explorer restarts. A named process mutex prevents a second manager in the same session. Tilework runs with the rights it was started with. A standard-user instance skips windows of elevated processes (UIPI blocks moving them); an elevated instance manages them. A standard instance that finds an elevated instance's mutex (access denied) treats it as already running. The tray item "Restart as administrator" starts an elevated copy with the internal `--replace` flag, which waits up to 10 s for the old instance to release the mutex; the old instance exits only after the UAC prompt was accepted.

Diagnostics: a log at `%LOCALAPPDATA%\<name>\tilework.log` records adds, removals, drops,
learned minimum sizes and desktop switches. The log is capped at 1 MB with one rotated backup.
Timestamps include the UTC date. Window titles are omitted by default; `verbose_log=true` enables
sanitized titles limited to 40 characters. `--dump` prints the layout the program would apply,
without touching any window; `--filter TEXT` limits management to matching windows for safe tests.

## 12. Settings

Settings are created only when missing. Tray changes reread the file and atomically update only the selected keys, preserving other manual edits. Read errors use defaults without replacing the unreadable file. If `LOCALAPPDATA` is unavailable, config writes and logging are disabled.

Tray menu and `config.ini`:

| Key | Default | Meaning |
|---|---|---|
| `verbose_log` | `false` | include sanitized window titles in the log |
| `focus_follows_mouse` | `true` | focus the window under the cursor |
| `focus_delay_ms` | `300` | how long the cursor must rest (50–5000) |
| `min_zone_width` / `min_zone_height` | `260` / `160` | smallest zone |

## 13. Browser simulation and release verification

The [V3 browser demo](demo/index.html) exercises the layout and drop rules on one simulated monitor. The browser controls represent window transitions using local state. Native window sizing, handles, focus, accessibility permissions, mixed-DPI monitors, Explorer and the installer are covered by separate Windows checks.

The [release matrix](RELEASE.md) lists the current verification evidence. The `Win` test interface drives the same reconcile, drag and placement logic with simulated delayed moves, DPI changes, foreground changes, minimize and restore. Native behavior can differ where Windows or the target app declines a requested placement or focus change.

Per-monitor layouts use Windows monitor handles and monitor work areas. After a successful cross-monitor drop Tilework briefly allows queued positioning to complete before reconciling against the observed monitor. Learned physical window minima are invalidated when DPI changes. Resizing a divider uses the displayed layout and preserves the stored share of minimized siblings.

Virtual desktop detection uses visible window evidence and Explorer's registry fallback. With one visible app and no usable shell information, a confirmed foreground desktop can supply the result. A sole pinned window is ambiguous on systems without shell information. An empty desktop with no usable shell identity pauses until Tilework can identify it.

Tilework uses per-user storage. It runs elevated or not under the same user profile, so files an elevated instance creates inherit the profile's ACL and stay editable by the standard user. Sign-in autostart is a per-user Run entry and always starts without elevation. The previous development build requires manual removal of its elevated scheduled task before migration.
