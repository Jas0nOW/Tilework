# CosmicTile

An auto-tiling window manager for Windows 10/11, modelled on the COSMIC desktop (Pop!_OS 24.04).
Written in Rust, no runtime dependencies besides the `windows` crate.

## Features

- New windows are tiled automatically: new columns first (up to 4), then up to 2 windows per column.
- Zones: every area holds up to 4 zones, each zone can be split again. Divider drags resize whole zones.
- Drag & drop with a live preview of the *complete* resulting layout:
  centre of a window = swap, edge of a window = split that window only, screen edge = new full-length zone.
- One independent layout per virtual desktop and monitor; tiling can be switched on/off per desktop
  (tray icon click or `Win + Y`).
- Windows with a minimum size (Chrome, Discord, ...) get a zone of exactly the size they need.
- Full-screen windows are left alone.
- Windows' own edge snapping is switched off while tiling is active (restored on exit,
  `cosmictile.exe --restore-snap` restores it manually).
- Focus follows mouse after a configurable delay (default 300 ms).

## Settings

Tray icon right-click menu, or `%LOCALAPPDATA%\CosmicTile\config.ini`.
Log file: `%LOCALAPPDATA%\CosmicTile\cosmictile.log`.

## Build

```
cargo build --release
cargo test
```

`cosmictile.exe --dump` prints what would be tiled without touching any window;
`--filter TEXT` manages only windows whose title contains TEXT (safe testing).

Run it as administrator if you also want to tile elevated windows (e.g. admin terminals).

## Status

Early development. Not yet implemented: autostart, keyboard shortcuts for moving/focusing, workspaces.
