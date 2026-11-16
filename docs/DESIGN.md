# Design: how the program is meant to work

This document describes the intended behaviour of the program, the reasons behind it, and where it
is going. It is the reference that code, tests and reviews are measured against. Sections marked
**(planned)** are not implemented yet.

Project name: *CosmicTile*.

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
3. Existing containers with a free slot are used next (up to **4 zones per container**), before
   creating deeper splits. Second rows take precedence over these denser slots.
4. Only when every existing container is full may automatic placement split another leaf. It
   checks all such fallback candidates and takes the one with the fewest minimum-size violations.
   Earlier placement choices also account for minima; a window is never left out of the layout
   (except full-screen windows, see 8).

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
| edge band of a window | split **that window only** on that side; every other window keeps its place |
| outer 36 px of the screen | new full-length zone along that screen edge |
| empty monitor | the window fills it |
| no valid target / would squeeze a window too small | nothing; the window snaps back |

If the target container is already full (4 zones), the drop splits only the target's zone.
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

Some apps refuse to get small (Chrome ≈ 502 px wide, Discord ≈ 802×501, ChatGPT ≈ 480×600). The
program does not fight them:

- Every zone has a global floor (default 260×160 px, configurable) so that part of the title bar
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
- Desktop identity uses the foreground window's confirmed ID where possible, then a majority of
  visible confirmed window IDs. This tolerates an isolated pinned window. Results are cached briefly
  and invalidated on foreground or desktop-switch events. For an empty desktop, the shell's current
  desktop registry value is a best-effort fallback. This shell data is undocumented and may change;
  if identity is unavailable, placement and hover focus pause and owned Snap changes are restored.
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
state is read when ownership is taken and flushed to `%LOCALAPPDATA%\CosmicTile\snap-off.flag` as `1` or `0`. Normal shutdown,
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

## 11. Architecture

```
main.rs      early DPI, single instance, arguments (--dump, --filter, --snap-status, --restore-snap)
config.rs    key=value settings file (%LOCALAPPDATA%\<name>\config.ini), tolerant parser
tree.rs      pure logic, no Windows calls: zone tree, placement policy, drop zones,
             minimum-aware layout, previews. Covered by unit and invariant tests; Win32 integration needs desktop checks.
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

The tray icon is re-added after Explorer restarts. A named process mutex prevents a second manager.
The Windows manifest requests the caller's existing privileges; it does not request elevation.

Diagnostics: a log at `%LOCALAPPDATA%\<name>\cosmictile.log` records adds, removals, drops,
learned minimum sizes and desktop switches. The log is capped at 1 MB with one rotated backup.
Timestamps include the UTC date. Window titles are omitted by default; `verbose_log=true` enables
sanitized titles limited to 40 characters. `--dump` prints the layout the program would apply,
without touching any window; `--filter TEXT` limits management to matching windows for safe tests.

## 12. Settings

Settings are created only when missing. Tray changes reread the file and atomically update only the selected keys, preserving other manual edits. When elevated, the settings command shows the path instead of launching its file handler. Read errors use defaults without replacing the unreadable file. If `LOCALAPPDATA` is unavailable, config writes and logging are disabled.

Tray menu and `config.ini`:

| Key | Default | Meaning |
|---|---|---|
| `verbose_log` | `false` | include sanitized window titles in the log |
| `focus_follows_mouse` | `true` | focus the window under the cursor |
| `focus_delay_ms` | `300` | how long the cursor must rest (50–5000) |
| `min_zone_width` / `min_zone_height` | `260` / `160` | smallest zone |

## 13. Roadmap

Near term (before a public 0.2):

- live verification of the hardening changes (see section 15), especially desktop and Snap paths
- autostart as administrator without a UAC prompt (scheduled task) and a clean uninstall
- the thin colour frame around the focused window as a real 4 px overlay
- persisted learned minimum sizes; per-app overrides and an ignore list

Later:

- keyboard shortcuts (focus/move/resize/toggle) for those who want them
- a settings window instead of the tray submenu and the text file
- more live multi-monitor coverage (hot-plug and DPI changes); the platform test seam covers simulated geometry changes
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


## 15. Hardening and live verification

The manager uses the caller's privileges and does not request elevation. Data paths containing a
reparse point are rejected for managed writes. Desktop and filesystem behavior still depend on
Windows APIs and require live verification, especially on elevated and mixed-DPI desktops.

**Live checks:** ordinary, maximized and full-screen reservation across minimize/restore and Win+D;
apps with real minimum sizes, saved geometry, self-resizing and DPI changes; manual layouts and
divider weights; pinned windows and empty virtual desktops; drag/drop across monitors; focus behavior
and foreground restrictions; Explorer restart, work-area changes, and Snap restoration after changes
on a paused desktop. Use isolated test windows with `--filter`.


### Audit outcome and remaining boundary

F1�F6 and F8�F14 are implemented with offline regression coverage where feasible. The `Win`
interface drives the real apply/reconcile/drag logic against simulated refusal, drift, DPI
rescaling, self-resizing, minimize/restore and full-screen style loss. Each message dispatch
uses one desktop snapshot; window desktop fallback reuses that evidence during enumeration.
Minimized tracked windows reserve their tiles for their top-level lifetime, with no expiry.

F7 is **partly resolved**. Elevated settings commands show the path, existing reparse points
are refused, leaf reads/creates use `FILE_FLAG_OPEN_REPARSE_POINT`, and directory handles deny
replacement during data operations. Log append also rejects hard-linked files. Directory
replacement and hard-linked log regressions pass. Three focused directory-guard attempts
showed that denying write sharing also prevents ordinary atomic saves and log rotation;
the working guard allows read/write sharing and denies delete sharing. A writable directory
handle can still change reparse data between validation and a path-based operation. Complete
race resistance would require handle-relative file operations or a separate protected storage
boundary. Elevated hostile-filesystem safety is therefore not certified by this change.

Empty-desktop detection uses Explorer's undocumented registry value only when there is no
visible application evidence. If that value is absent, identity pauses with a tray explanation;
it does not guess the desktop from the last one visited. This fallback needs live checks on
supported Windows versions. Synthetic zero-movement mouse input also remains subject to
Windows foreground/UIPI policy; a rejected focus request is attempted only once per hovered
window until the cursor visits another window.

Offline validation: `cargo test` (90 tests), `cargo clippy --all-targets -- -D warnings`, and
one final `cargo build --release`. No manager instance was started, installed or committed.
CI artifact publication is configured and must be exercised by the orchestrator's next PR.
