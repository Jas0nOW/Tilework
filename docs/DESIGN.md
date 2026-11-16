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

Closing, minimising or moving a window to another desktop removes it; its neighbours grow into the
space. Restoring it places it like a new window.

## 4. Drag & drop

While a tiled window is dragged, an overlay shows **the complete layout that would result** (all
windows as thin frames, the dragged window filled). The overlay is computed by applying the move to
a copy of the tree, so it is exactly what happens on release.

Drop targets, by cursor position over the window below:

| Cursor on â€¦ | Result |
|---|---|
| centre of a window (28â€“72 %) | swap the two windows |
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

Some apps refuse to get small (Chrome â‰ˆ 502 px wide, Discord â‰ˆ 802Ã—501, ChatGPT â‰ˆ 480Ã—600). The
program does not fight them:

- Every zone has a global floor (default 260Ã—160 px, configurable) so that part of the title bar
  always stays grabbable.
- Placement requests are asynchronous and skip hung windows; maximized adoption uses an
  asynchronous restore without activation, followed by normal geometry calculation on a later poll. After a 500 ms settling interval, two identical poll measurements are
  required before a refused size is learned. When a window remains larger than its zone, its real
  minimum is **learned** and
  the layout gives its zone exactly that size; the others share the rest. The layout engine
  distributes space by weight but never below a zone's minimum.
- Learned minima are forgotten when a window disappears from the top-level list. They do not decrease during its lifetime; maximized and minimized windows are not measured.
- When the current arrangement violates minima, a bounded rebuild tries windows largest first,
  with stable ordering for equal sizes. It is accepted only if it strictly reduces violations
  and retains every window; otherwise the manual arrangement is kept.

Physical limit: Windows does not let an app go below its own minimum; no tiling program can change
that. Rearranging (for example moving Discord into a column where its width hurts less) is the way
out.

## 7. Virtual desktops and monitors

- Each surface is tiled independently; the layout of a desktop you left is kept and verified when
  you come back.
- Tiling is switched **per virtual desktop** (tray icon click, `Win + Y`, tray menu). Desktops
  with tiling on and off can coexist. Paused trees preserve their manual weights for the process
  lifetime; enabling reconciles windows that have since closed or moved.
- Desktop identity requires a successful COM query confirming a visible, uncloaked window on the
  current desktop with a nonzero desktop ID. Conflicting IDs are treated as unavailable. If identity
  is unavailable, placement and hover focus pause and owned Snap changes are restored.
- A desktop switch or disabling tiling cancels any active drag. Display/work-area notifications
  rebuild surface monitor keys from live windows and trigger a full reconcile.
- Windows that Windows itself hides (cloaked, ghost UWP frames on the shown desktop) are ignored.

## 8. Things the program leaves alone

- **Maximized and full-screen windows** already tiled keep their reserved tile and are left alone until restored. Neighbours keep their positions. A newly adopted maximized window is restored into its tile; untracked full-screen windows are never adopted. A tile matching the whole monitor is still a tile.
- Dialogs, tool windows, pop-ups, menus, tooltips, the taskbar and the desktop.
- Minimised windows (they keep no tile).

## 9. Windows' own snapping

Windows Snap fights with any tiling program (dragging to a screen edge triggers it). While tiling
is on for the shown desktop, the program switches Snap off through `SystemParametersInfo`
(`SPI_SETWINARRANGING`) **for the running session only** (nothing is written to the registry), and
restores the previous state on exit and on desktops where tiling is off. `--restore-snap` restores
it manually (enabling Snap and removing the recovery marker). Before disabling Snap, the original
state is flushed to `%LOCALAPPDATA%\CosmicTile\snap-off.flag` as `1` or `0`. Normal shutdown,
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
All five common mouse buttons suppress hover focus. Focus requests use `SetForegroundWindow`
without joining input queues; Windows may decline the request silently.

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
Titles appear only on add/remove events, sanitized and limited to 40 characters. `--dump` prints the layout the program would apply,
without touching any window; `--filter TEXT` limits management to matching windows for safe tests.

## 12. Settings

Settings are created only when missing. Saves replace the file atomically; opening the file never overwrites manual edits. Read errors use defaults without replacing the unreadable file. If `LOCALAPPDATA` is unavailable, config writes and logging are disabled.

Tray menu and `config.ini`:

| Key | Default | Meaning |
|---|---|---|
| `focus_follows_mouse` | `true` | focus the window under the cursor |
| `focus_delay_ms` | `300` | how long the cursor must rest (50â€“5000) |
| `min_zone_width` / `min_zone_height` | `260` / `160` | smallest zone |

## 13. Roadmap

Near term (before a public 0.2):

- live verification of the hardening changes (see section 15), especially desktop and Snap paths
- autostart as administrator without a UAC prompt (scheduled task) and a clean uninstall
- the thin colour frame around the focused window as a real 4 px overlay
- persisted learned minimum sizes; per-app overrides and an ignore list
- rename before public release

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


## 15. Hardening

The review was checked item by item against inherited commit `b4b814f`, then completed in four
verified slices. The initial baseline had 61 passing tests; the final suite has 74. Pure tree,
placement, drag observation, pending-event, identity-consensus and filesystem helpers are tested.
Actual Windows interactions still need the live checks below. Tests never invoke Snap SPI writes.

| Review items | Offline status / evidence |
|---|---|
| A, K, M17 | Done: normalization uses original child index; deterministic PRNG mixes add/remove/swap/insert/divider operations, checks invariants; minimum settings are isolated across test threads (`src/tree.rs:547,838,878`). |
| B, decision 1 | Done: placement reservation is checked before force; a legitimate monitor-filling tile remains managed (`src/app.rs:317,1275`). |
| C, M6; D, M8; E | Done: early DPI with error logging, direct foreground request without input attachment, invalid/duplicate filters rejected before run (`src/main.rs:12,88`, `src/app.rs:658`). |
| F, M3, M4 | Done: release uses the same fresh geometry/target path as ticks; failed observations clear old targets; live target membership is rechecked (`src/app.rs:528,1045,1173`). |
| G, M5; H9, H10, H11, decisions 2/3 | Done: real subtree minima clamp only adjacent divider pair; bounded reseat, ordered automatic slots, minimum-violation fallback, target-only manual split (`src/tree.rs:161,276,580,729,926,977,984`). |
| H, M15; I; J, H8 | Done: preserve existing settings, atomic saves, Explorer tray recovery, prune dead handles, two consecutive eligible settled measurements; minima never decrease during a window lifetime (`src/config.rs:95,112,125`, `src/app.rs:232,323,890`). |
| H1, H2 | Done: checked Snap SPI with pvParam, read-only status, flushed original-state marker, panic/control/session-end recovery; marker tests cover absent, corrupt, mismatching and failed restoration (`src/platform.rs:307,344,412`). |
| H4, M9, M10; H5, H7 | Done: skip hung/iconic windows; async nonactivating restore before later placement, async positioning, released modal borrow, typed pending causes, lost-end timeout, desktop recheck (`src/platform.rs:239`, `src/app.rs:440,763,869,1127`). |
| H6 | Partly: confirmed uncloaked application windows and nonzero consistent COM IDs are required; missing/conflicting identity pauses placement, hover focus and Snap ownership (`src/platform.rs:166,281`, `src/app.rs:232`). See pinned-window limitation below. |
| M1, M2 | Done: cross-monitor center inserts below; preview includes the changed source layout (`src/app.rs:1045,1201`, section 4). |
| M7, M11, M12 | Done: five-button/menu/fullscreen focus guards; display/work-area events rederive monitor keys, including deferred callbacks, and force placement; paused desktop trees retain weights (`src/app.rs:85,271,410,464,616,1082,1112`). |
| M13, M14, M16 | Done: bounded sanitized rotating log, no absent/empty/relative app-data writes, named mutex and checked registration results (`src/app.rs:150,162,998,1002`, `src/config.rs:89`, `src/main.rs:49`). |
| L1, L2, L3 | Done: original DWM borders restored, including unmanaged focus; hooks/COM/icons/timers/hotkey cleaned up; menu and message-loop errors handled (`src/app.rs:381,398,780,1016,1025`, `src/platform.rs:435`). |
| L4, L5 | Done: saturating/i64 minimum arithmetic; dump uses config minima and excludes untracked fullscreen/unknown desktop (`src/tree.rs:648,660`, `src/main.rs:60`). |
| Open source | Done: MIT/package metadata, dependency-free embedded MSVC manifest, Windows PR/main/tag test/clippy/release workflow (`Cargo.toml:1`, `build.rs:1`, `app.manifest:1`, `.github/workflows/ci.yml:1`). |

The supplied list has no H3 or standalone bug-hunt L/M; no finding was inferred for those labels.

**Pinned-window limitation (H6):** the documented [IVirtualDesktopManager interface](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager)
has no pin-status query. We deliberately avoid version-sensitive private shell interfaces. Conflicting
visible-window IDs suspend tiling instead of selecting one arbitrarily. A pinned window alone (or
several reporting the same stale ID) cannot be identified reliably by this implementation; reliable
exclusion of every pinned window is skipped. This remains a live compatibility limitation.

**Live checks still required:** normal/maximized/F11 reservation and restore (including auto-hide
 taskbar and mixed DPI); asynchronous/hung-app placement and focus preservation; fast moves/resizes,
 invalidated targets and both cross-monitor previews; desktop changes during drags and unavailable
 COM identity; pinned windows; manual layout across off/on; foreground menus and all mouse buttons;
 display hot-plug/work-area changes; Explorer tray recreation; Snap status, original-off/on recovery,
 panic/session-end paths and failed recovery; single-instance/hotkey conflicts; DWM border restoration
 and resource cleanup. Use isolated test windows with `--filter`; a forced kill can require manual
 Snap recovery. No live manager test, installation, push or merge was performed in this continuation.

Release verification: `cargo test` (74 passed), `cargo clippy --all-targets` (clean),
`cargo build --release` (successful). Five invalid-filter process probes returned exit code 2
and console errors; read-only `--dump` and `--snap-status --dump` succeeded with app-data redirected
inside `target/`. The release PE resource contains the asInvoker application manifest.
