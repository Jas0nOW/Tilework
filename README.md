# CosmicTile

An auto-tiling window manager for Windows 10/11, modelled on the COSMIC desktop (Pop!_OS 24.04).
Written in Rust, no runtime dependencies besides the `windows` crate.

## Features

- New windows are tiled automatically: new columns first (up to 4), then second rows, then free container slots before deeper splits.
- Zones: every area holds up to 4 zones, each zone can be split again. Divider drags resize whole zones.
- Drag & drop with a live preview of the *complete* resulting layout:
  centre of a window = swap (across monitors: insert below), edge of a window = split that window only, screen edge = new full-length zone.
- One independent layout per virtual desktop and monitor; tiling can be switched on/off per desktop
  (tray icon click or `Win + Y`); manual layouts survive off/on for the process lifetime.
- Windows with a minimum size (Chrome, Discord, ...) get a zone of exactly the size they need.
- Tiled windows that become maximized or full screen keep their reserved tile until restored.
- Windows' own edge snapping is switched off while tiling is active (restored on exit,
  `cosmictile.exe --restore-snap` enables it manually and clears the recovery marker).
  A marker recovers the previous state on next startup after an interrupted run.
- Focus follows mouse after a configurable delay (default 300 ms), with mouse-button, menu and full-screen/maximized guards. Windows can refuse focus requests.

## Settings

Tray icon right-click menu, or `%LOCALAPPDATA%\CosmicTile\config.ini`.
Log file: `%LOCALAPPDATA%\CosmicTile\cosmictile.log` (1 MB, one backup). Settings are saved atomically; opening them preserves existing edits. Without `LOCALAPPDATA`, config writes and logging are disabled.

## Build

```
cargo build --release
cargo test
```

`cosmictile.exe --dump` prints what would be tiled without touching any window;
`--snap-status` reports Windows Snap without changing it.
`--filter TEXT` accepts a single non-empty value, cannot be repeated and manages only windows whose title contains TEXT (safe testing).

Run it as administrator if you also want to tile elevated windows (e.g. admin terminals).

## Status

Tree and helper logic have 74 unit and invariant tests; Windows desktop interactions require live checks.
Windows CI tests, checks Clippy and builds release artifacts on pull requests, main updates and tags.

Early development. Not yet implemented: autostart, keyboard shortcuts for moving/focusing, workspaces.

Hardening status, the pinned-window limitation and required desktop checks: [Design: Hardening](docs/DESIGN.md#15-hardening).
