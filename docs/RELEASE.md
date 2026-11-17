# Release verification

**0.3.0 candidate · 2026-11-17 · NO-GO for publication.** The per-user installation lifecycle and public release path still need verification. The candidate is suitable for controlled local QA. Use a disposable Windows account or VM for installer tests.

| Area | Result | Evidence |
|---|---|---|
| Rust tests | PASS | `cargo test --locked` on Windows, 106 passed. Includes V3 layout, drag and drop, delayed native positioning via the fake platform, transient focus, DPI changes, minimized dividers, settings and log rotation. |
| Lint and release executable | PASS | `cargo clippy --locked --all-targets -- -D warnings` and `cargo build --locked --release` with Rust 1.100.0, x64 MSVC. Native `--version` returned 0.3.0. |
| Native Windows window lifecycle and focus frame | PASS | [Native smoke script](../scripts/native-smoke.ps1) on standard-user Windows 11, primary work area 1920×1032. Three real WinForms windows, DWM bounds checked, reflow on close and minimize/restore, overlay matched foreground bounds, exit code 0. Snap stayed enabled with isolated, intentionally unavailable app data. |
| Native graceful exit and crash while owning Snap | NOT TESTED | Recovery-marker handling was exercised in automated tests. The native test isolated the setting and confirmed it stayed unchanged. |
| Native drag, preview, resize, fullscreen, virtual desktops, monitor changes and Explorer restart | NOT TESTED | Related layout and event scenarios pass in simulation. Native capture, pinned windows, mixed DPI, hot-plug and Explorer recovery need separate runs. |
| Long desktop session and handle/resource usage | NOT TESTED | Bounded placement retries and log rotation have regression coverage. Long-run CPU, GDI object and window-handle reuse behavior remain unmeasured. |
| Legacy elevated autostart | FAIL | The prior implementation scheduled its executable with highest privileges, including a path writable by the standard user. An existing task must be removed before migrating. |
| Candidate autostart boundary | PASS | Static review found only a current-user Run value and standard-user execution. Native portable CLI refused to register; no sign-in entry appeared. Legacy task creation was removed. |
| Installed sign-in, restart and task migration | NOT TESTED | Clean-profile installer, sign-out/in, migration from the old task and uninstaller registry removal have no native lifecycle evidence yet. |
| Prior executable self-updater | FAIL | Checking MZ and PE signatures only identified a file format. The old replace-and-restart sequence had a gap between renames and discarded its backup without checking application readiness. |
| Candidate update discovery | PASS | Transport to the fixed GitHub API returned no public stable release. Fixtures check old and equal versions, the exact installer URL, prereleases, invalid JSON, HTTP errors and unexpected assets. Legacy `--update` returned a manual-install message without replacing files. |
| Automatic application update and rollback | FAIL | The candidate uses manual installation and downgrade. A signed and authenticated automatic update and recovery protocol has not been delivered. |
| Setup build, clean install, upgrade, interrupted install, manual rollback and uninstall | NOT TESTED | Inno Setup source and workflow are present. The compiler bootstrap was blocked by local command policy; offline extraction could not parse that compiler package. A clean Windows guest was unavailable. |
| GitHub Actions, draft assets, and published checksums | NOT TESTED | Workflow YAML parsed locally. No dispatch or remote artifact test was performed. Toolchain version, dependency lock and compiler digest are pinned; no independent byte-for-byte rebuild of the distribution was performed. |
| Dependency advisories | PASS | OSV batch check of 23 locked crate versions returned zero matching entries on the audit date. |
| Repository contents and history | FAIL | No hits in the checked common credential patterns or sensitive filename scan. Commit metadata contains a personal, non-noreply email. Decide its publication or an approved history sanitization before making the repository public. |
| Interactive V3 demo | PASS | Isolated Edge tested desktop and touch viewports, 1–8 window placements, centre swap, edge split, outer zone, divider resize and independent desktop state. No external asset requests or page errors. |
| Public GitHub Pages and private vulnerability submission | NOT TESTED | The repository remains private. Pages activation and private reporting settings require a maintainer action. |

## Findings and severity

| Priority | Finding and disposition |
|---|---|
| P0 | An earlier build could elevate a replaceable executable at logon. Removed elevated scheduling and rejected elevated manager execution in the candidate. Follow the README migration for existing tasks. |
| P0 | The earlier updater accepted executable-looking bytes as trusted content and could replace a file used by elevated autostart. Removed download-to-execution and executable replacement from the app. |
| P1 | The old setup could leave a partial install and report successful removal despite failures. Replaced it with the per-user Windows installer and explicit file ownership. Installer lifecycle remains a release gate. |
| P1 | Desktop detection could suspend tiling with a lone app, focus could outline fullscreen or stale windows, and a valid drop onto another monitor could be discarded before Windows applied its queued move. Added focused fixes with application-level regressions. |
| P1 | Setup lifecycle, real multi-monitor drag/resize and crash/session recovery require the tests below. Resolve personal Git metadata before going public. |
| P2 | A sole pinned window can be ambiguous without Explorer desktop information. Recycled numeric window handles could briefly inherit cached state. Handle reuse has no reproduced failure in this audit. |

## Reproduce the checks

Run the locked Cargo commands from the README, then use an interactive, standard-user PowerShell 7 session with Tilework closed:

```powershell
.\scripts\native-smoke.ps1 -Executable .\target\release\tilework.exe
```

The script opens three fixture windows and filters management to their unique titles. It checks physical DWM bounds and uses temporary app data that disables ownership of Windows Snap.

Use verified Inno Setup 6.7.4 in a separate QA environment:

```powershell
ISCC.exe /DMyAppVersion=0.3.0 .\packaging\Tilework.iss
```

Installer output is `packaging/output/Tilework-Setup-0.3.0-x64.exe`. Keep install logs with `/LOG` and note OS version, profile type and artifact hashes.

1. On clean Windows 11 and Windows 10 22H2, install from a standard account. Check that Windows shows an Apps entry, that the Start menu starts version 0.3.0, and that autostart is initially off. On a test profile that has the previous build, follow the README migration and verify the legacy task is absent before proceeding.
2. Enable autostart from the installed app. Sign out and in, then check for one standard-user instance. Disable it, sign out and in again, and check that no Tilework instance starts. Repeat the upgrade and uninstall checks with autostart enabled.
3. From two verified installer builds of different candidate versions, try an upgrade with Tilework open, then close it and retry. Check the installed version, that settings survive and that relaunch works. Repeat after cancelling setup, after testing a truncated installer copy, and after interrupting an install in the disposable guest. Record whether the prior version still runs and whether rerunning verified Setup repairs it. Downgrade with the previously verified installer and check version, settings and startup again.
4. Uninstall via Settings and check that the executable, both Start menu entries, any selected desktop shortcut, Apps entry and Run value are gone. Check that settings remain until explicitly removed. Sign out and in and check that Tilework stays closed.
5. In an interactive desktop, test title-bar drag (centre, edge, outer edge and cancellation), resize with a minimized sibling, full screen, Win+D, moving across two different DPI settings, monitor disconnect/reconnect, pinned/empty virtual desktops and Explorer restart. Compare preview to actual bounds. In the disposable session, force-stop Tilework while it owns Snap, then verify recovery on next launch and clean exit. Observe CPU, handles and GDI objects across a repetitive window-open/close session.

## Prepare a draft after the gates pass

1. Review the source and private metadata before changing repository visibility. Enable private vulnerability reporting before relying on the reporting form.
2. Merge the verified candidate, create and push the matching tag only after approval, and dispatch **Windows CI** on that tag with the same `release_tag`. Inspect `Tilework-Setup-x64.exe`, `Tilework-Portable-x64.exe` and `SHA256SUMS.txt` on the draft prerelease. Exercise the full lifecycle with those exact files. Run the positive update-discovery path against a released stable test fixture or verify it when a newer public stable version exists.
3. Configure GitHub Pages to deploy from the default branch and `/docs` after visibility and publication approval. Open `/Tilework/demo/` without authentication and verify that the README link, controls and screenshots work.
4. Decide publisher signing and whether manual update installation is acceptable for the first release. An unsigned download may trigger Windows reputation checks. Stable publication needs a separate maintainer decision.
