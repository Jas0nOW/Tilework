# Changelog

## 0.4.0 (beta)

- Give the program, its Start menu shortcut and the installer a Tilework icon.
- Check for updates automatically (about a minute after start, then at most once a day; tray toggle **Check for updates automatically**, `check_updates` in `config.ini`, default on). New releases are announced once a day per version by a tray notification and an **Install update** menu item.
- One-click update: download the official installer, verify its SHA-256 against the release's `SHA256SUMS.txt`, then close Tilework, run the per-user installer and restart the new version. Portable copies open the release page instead.
- Allow starting Tilework with administrator rights. An elevated Tilework also tiles elevated windows; a normal one keeps leaving them alone.
- Add **Restart as administrator** to the tray menu, show "Running as administrator" and an "(Administrator)" tooltip suffix when elevated.
- Treat a Tilework instance with other elevation as already running instead of logging an error.
- Re-check cached window elevation after 30 seconds so reused process ids are not misclassified.
- Make the drag preview and focus frame cheaper and steadier: unchanged previews cost no window calls, paints allocate no GDI objects, and the focus frame follows its window immediately and hides on minimize, close or cloak.
- Fix drops onto a monitor whose tiles are all minimized, the focus frame staying hidden after a drag without a drop, and a UTF-8 byte order mark hiding the first setting in `config.ini`.
- Harden against failed window creation, an unreadable monitor work area during display changes, a partially written Snap recovery marker, Explorer timing out when the tray icon is added at sign-in, and lost virtual desktop information after Explorer restarts.
- Let the installer detect a running elevated Tilework, accept tray clicks in an elevated Tilework, and never open the browser elevated.

## 0.3.0 (beta)

- Prepare the V3 tiling layout and focus frame for a standalone per-user Windows installer and portable distribution.
- Replace the elevated scheduled logon task with opt-in standard-user sign-in. Earlier development builds require manual migration.
- Replace executable self-replacement with stable-release discovery and a link to the official release page.
- Fix focus frames on stale foreground events and fullscreen transitions, delayed cross-monitor placement, stale learned minimum sizes after DPI changes, and divider resizing when windows in the same container are minimized.
- Leave windows of elevated (administrator) programs alone instead of retrying blocked placements.
- Improve desktop detection with one visible application when shell information is unavailable.
- Add the interactive V3 browser simulation, screenshots, and the release verification matrix.

## 0.1.0

- Initial tagged development version of Tilework's automatic tiling and drag preview.
