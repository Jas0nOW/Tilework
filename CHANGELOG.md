# Changelog

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
