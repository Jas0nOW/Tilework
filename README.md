# Tilework

[![CI](https://github.com/Jas0nOW/Tilework/actions/workflows/ci.yml/badge.svg)](https://github.com/Jas0nOW/Tilework/actions/workflows/ci.yml) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**Automatic tiling for Windows 10/11, inspired by the COSMIC desktop (Pop!_OS 24.04).**
Open a window and it finds its place. Drag it to rearrange, with a live preview of the exact result.
No layout language, no configuration needed, no gaps.

Written in Rust. The only dependency is the official [`windows`](https://crates.io/crates/windows) crate.

> **Status: early development (0.1).** It is used daily, but only parts of it can be verified automatically.
> See [Status and limitations](#status-and-limitations) before relying on it.

## Features

- **Automatic placement.** First two windows form two full columns (halves), 3rd and 4th windows split vertically inside these columns (2x2 grid), and the 5th window creates a 3rd full column automatically. Manual screen edge drops take precedence and create through zones.
- **Zones.** Every area holds up to 4 zones side by side or stacked, and every zone can be split again. Dragging a divider resizes whole zones.
- **Drag & drop with a live preview** of the complete resulting layout:
  centre of a window = swap, edge of a window = split only that window nested, screen edge = new full-length through zone.
- **Active focus border overlay.** A crisp 3 px accent frame overlay surrounds the focused tiled window across all apps without stealing focus or intercepting clicks.
- **Per virtual desktop and monitor.** Every desktop/monitor has its own layout. Tiling can be switched on or off per desktop (tray icon click or `Win + Y`); manual layouts survive switching off and on.
- **Integrated installer & autostart.** Install in one click to `%LOCALAPPDATA%\Tilework` with Start Menu shortcut and silent admin autostart via Windows Task Scheduler (no UAC prompt on logon).
- **Production-ready self-updater.** Automatic or manual update checks directly from GitHub releases with PE binary verification, atomic swap with rollback, and startup cleanup.
- **Respects minimum sizes.** Apps that refuse to get small (Chrome, Discord, ...) get a zone of exactly the size they need instead of overlapping their neighbours.
- **Leaves full screen alone.** A tiled window that you maximise or put into full screen (F11, video, game) is not pulled back; its tile stays reserved and it returns when you restore it, also across minimise/restore and `Win + D`.
- **Windows Snap is switched off while tiling is active** (session only, nothing is written to the registry) and restored on exit. A recovery marker restores it after a crash.
- **Focus follows mouse** after a configurable delay (default 300 ms), with guards for mouse buttons, menus and full-screen windows.
- **Fail safe.** Tilework only moves and resizes windows. It never closes or hides anything.

How it is meant to behave in detail: [docs/DESIGN.md](docs/DESIGN.md).

## Install

Tilework can run portably or be installed with a single click:

### Quick Install (GUI)
1. Download `tilework.exe` from the latest release and run it.
2. Right-click the tray icon and select **Install Tilework**.
3. It installs to `%LOCALAPPDATA%\Tilework\tilework.exe`, creates a Start Menu shortcut, and registers silent administrator autostart via Windows Task Scheduler (no UAC prompt on logon).

### Command Line Install
```powershell
tilework.exe --install
```

### Uninstallation
Right-click the tray icon and select **Uninstall Tilework**, or run:
```powershell
tilework.exe --uninstall
```

### Self-Updater
- **Via Tray:** Right-click tray icon -> **Check for updates...**
- **Via CLI:** `tilework.exe --check-update` or `tilework.exe --update`

## Usage

| Action | How |
|---|---|
| Turn tiling on/off for the current desktop | Click the tray icon or press `Win + Y` |
| Rearrange | Drag a window by its title bar; the overlay shows the result |
| Resize | Drag a window edge; the divider moves for the whole zone |
| Settings | Right-click the tray icon, or edit `%LOCALAPPDATA%\Tilework\config.ini` (applied after restart) |
| Quit | Tray menu, Exit |

### Settings (`config.ini`)

| Key | Default | Meaning |
|---|---|---|
| `focus_follows_mouse` | `true` | focus the window under the cursor |
| `focus_delay_ms` | `300` | how long the cursor must rest (50-5000) |
| `min_zone_width` / `min_zone_height` | `260` / `160` | smallest zone a window can be squeezed into |
| `verbose_log` | `false` | include (shortened) window titles in the log |

The log is `%LOCALAPPDATA%\Tilework\tilework.log` (1 MB, one backup).
If you used the former name *CosmicTile*, your settings are migrated on the first start.

### Command line

| Argument | Effect |
|---|---|
| `--install` | install Tilework to `%LOCALAPPDATA%\Tilework`, create Start Menu shortcut & autostart task |
| `--uninstall` | clean uninstall: remove autostart task, Start Menu shortcut, and installed executable |
| `--autostart-on` | enable scheduled task autostart with highest privileges (no UAC prompt on logon) |
| `--autostart-off` | disable scheduled task autostart |
| `--autostart-status` | report whether silent admin autostart is enabled |
| `--check-update` | check GitHub releases for available updates (non-destructive) |
| `--update` | download latest release, verify PE integrity, atomically replace binary, and restart |
| `--dump` | print what would be tiled and where, without touching any window |
| `--snap-status` | report whether Windows Snap is enabled (read-only) |
| `--restore-snap` | enable Windows Snap again and clear the recovery marker (refused while Tilework runs, unless `--force`) |
| `--filter TEXT` | manage only windows whose title contains `TEXT` (safe testing) |

## Build

Requires a recent stable Rust toolchain on Windows (MSVC).

```
cargo build --release     # target\release\tilework.exe
cargo test
cargo clippy --all-targets -- -D warnings
```

## Status and limitations

- The layout logic is covered by unit and property-style tests. The window handling is tested against a simulated platform (windows that refuse a size, drift, rescale or hide). Real desktop behaviour (mixed DPI, pinned windows, Explorer restart, multi-monitor hot-plug) still needs live checks; the list is in [docs/DESIGN.md](docs/DESIGN.md#15-hardening).
- Windows enforces the minimum size of some apps; no tiling program can shrink them further.
- Pinned windows (shown on all desktops) can confuse virtual-desktop detection; Tilework then pauses instead of guessing.
- Planned for 0.3: optional keyboard shortcuts for moving/focusing.

## Contributing

Bug reports and pull requests are welcome, see [CONTRIBUTING.md](CONTRIBUTING.md).
Security issues: [SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE)
