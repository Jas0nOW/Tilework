# Tilework 0.3.0 release verification

Evidence was collected on 2026-11-17 using Windows 11 and a disposable Windows Server 2025 GitHub-hosted runner. Release decision: **BETA**. Version 0.3.0 is published for volunteer testers. The interactive desktop checks marked NOT TESTED below are still open.

| Area | Result | Evidence |
|---|---|---|
| Rust tests, Clippy and release build | PASS | 106 locked tests passed on Windows. Clippy passed with warnings denied and the x64 MSVC release executable reported 0.3.0. Rust 1.100.0. |
| Release binary and setup reproducibility | PASS | Two independent local target directories built the same portable executable. Inno Setup 6.7.4 packed both copies into identical installers on the same workstation. See SHA-256 values below. Cross-toolchain reproducibility remains unverified. |
| Native window lifecycle and focus frame | PASS | On a standard-user Windows 11 desktop, three WinForms fixture windows reflowed on close and minimize/restore. The focus overlay matched the foreground frame in physical DWM coordinates. |
| Native move and drop preview | PASS | Real pointer input exercised centre swap, nested split and the bottom screen edge. The captured preview for the dragged tile matched its final DWM bounds, accounting for the preview's 6-pixel inset. Primary work area 1920×1032. |
| Native Snap state recovery | PASS | Tilework took ownership of Snap on Windows 11. The harness terminated its process, checked the recovery marker, restarted Tilework and observed Snap restored after a graceful exit. |
| Native multi-monitor, mixed DPI, interactive resize and Explorer recovery | NOT TESTED | Single-monitor Windows session only. Monitor hot-plug, the full-screen and virtual desktop interactions, and every neighbouring preview rectangle need separate runs. Related conditions have automated layout and event coverage. |
| Long-session resource consumption | NOT TESTED | Bounded placement retries and log rotation have automated coverage. Long-run CPU, GDI objects and handle reuse remain unmeasured. |
| Legacy file path migration guard | PASS | The actual local installer exited 1 with the migration message against an existing legacy executable. A SHA-256 comparison confirmed the old file stayed intact. No candidate files were installed. |
| Installer lifecycle in a fresh Windows VM | PASS | Private GitHub Actions run 44070587446 (Windows runner, new standard-user profile): legacy-path guard, fresh per-user install and launch, opt-in autostart entry, upgrade 0.3.0 to 0.3.1 fixture preserving config, rejected truncated installer leaving the working version intact, return to 0.3.0, autostart off/on, uninstall removing program, shortcuts, Apps entry and autostart while keeping settings, and unrelated same-name scheduled task left untouched. Not covered: real sign-in/reboot and the interactive wizard. |
| Legacy task migration on an existing interactive desktop | NOT TESTED | Follow the verified-ownership procedure in the [README](../README.md#migrate-from-the-earlier-development-build). Setup checks tasks visible under the installing account and leaves all tasks intact. An administrator must check tasks registered under another identity. |
| Autostart execution after sign-in or reboot | NOT TESTED | A runner process started under a fresh user token does not start an interactive Windows shell. Actual sign-in, reboot and the next sign-in after uninstall require a disposable desktop. |
| Update discovery | PASS | Automated fixtures exercise older and equal versions, prereleases, the exact official installer URL, invalid responses and network failures. A native call reached GitHub without finding a public stable release. The legacy command prints manual update instructions. |
| Public positive update discovery | NOT TESTED | Requires a newer public stable release. Version 0.3 installs updates manually from the official Releases page after checksum verification. |
| Interactive installer and in-progress interruption | NOT TESTED | Windows client setup wizard, interruption after setup starts writing, and recovery by rerunning a verified installer require a disposable interactive session. Rejecting a truncated download covers a distinct failure before installation. |
| Draft attachments and public checksums | PASS | Workflow run for tag v0.3.0 built the draft from commit fbc164b. The downloaded Setup and Portable SHA-256 values matched SHA256SUMS.txt, and BUILDINFO.txt named the tagged commit. |
| Dependency advisories | PASS | OSV returned zero matching advisories for the 23 audited locked crate versions. |
| Repository history for public visibility | PASS | Common credential patterns and sensitive filenames returned zero hits. History was rewritten so every commit and tag carries the GitHub no-reply address, and the remote holds only the sanitized refs. |
| Interactive V3 demo | PASS | Edge exercised desktop and touch viewports, one to eight windows, centre swap, edge split, screen edge, divider resize and independent simulation desktop state. The README embeds the standalone demo and two screenshots. |
| Public Pages and private vulnerability reporting | PASS | The demo answers HTTP 200 without authentication at the Pages URL linked in the README. Private vulnerability reporting is enabled. |

## Findings and release gates

| Priority | Finding and disposition |
|---|---|
| P0 | Previous builds used a scheduled task that could launch an executable in a user-writable directory with elevated rights. The candidate uses a per-user Run entry and refuses elevated management. Review and remove a confirmed legacy Tilework task before migration. |
| P0 | The previous updater treated executable headers as adequate verification and replaced the running application. The candidate discovers updates using a fixed HTTPS endpoint and gives the user the official release link. The application no longer downloads or replaces executables. |
| P1 | Per-user installation, removal, migration and actual sign-in need independent evidence. Confirm the remaining interactive checks before stable publication. |
| P1 | Real multi-monitor and mixed-DPI placement and Explorer recovery remain open. Confirm public Git metadata and the positive public update path before widening distribution. |
| P2 | Pinned windows can be ambiguous without Explorer desktop information. Handle reuse has no reproduced failure. |

### Reproducible local package hashes

| File | SHA-256 |
|---|---|
| Portable executable | 746114f31c931b5de79dc53a2266ee4fe88ca4e1aac2e5824317a6aa144d49a2 |
| Installer | 54f10cb447e695037dd676334d845599eb6abad0e6b59afa63583c6f6be6df16 |

These hashes refer to the two-build local experiment. Use each GitHub artifact's own SHA256SUMS.txt for distribution.

## Reproduce checks

Use a standard-user Windows session with Tilework closed. Keep the installer log, test OS version, profile type and package SHA-256 with the result.


~~~powershell
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --release
.\scripts\native-smoke.ps1 -Executable .\target\release\tilework.exe -TestDrag -TestSnapRecovery
~~~

The native script limits management to its three fixture windows. The Snap recovery option temporarily changes the current user's Windows Snap setting and restores it on cleanup. Keep the pointer free during the drag test.

From a private GitHub branch, manually dispatch **Windows CI** with **installer_qa** enabled and **release_tag** empty. The job runs a real 0.3.0 setup and a version-bumped 0.3.1 test fixture in a new standard-user profile. The VM is discarded after the run. GitHub stores the generated installer, portable build, checksums and source commit with the successful run.

Use a disposable interactive Windows desktop to finish the remaining gates:

1. On Windows 11 and Windows 10 22H2, install as a standard user. Start Tilework from Start and check that Windows shows the correct version in Installed apps. For an older installation, verify the exact task identity, action and account as described in the README, then complete the migration and confirm only that task was removed.
2. Enable autostart in the tray. Sign out and back in, then reboot. Check for one standard-user Tilework process on each login. Disable autostart and repeat sign-in. After uninstall, verify that the app stays closed after another sign-in.
3. Try an upgrade while Tilework is running. Exit Tilework and retry. In a snapshot of the VM, cancel after installation starts writing and verify whether the previous version still starts. Rerun a checksum-verified setup to confirm recovery. Reinstall the previous verified version and check settings and launch. Uninstall through Windows Settings and check the optional desktop shortcut as well.
4. Repeat drag and resize across two monitors with different scales. Compare preview and window bounds, including neighbours. Disconnect and reconnect a monitor. Test fullscreen, Win+D, switching to an empty desktop, and restarting Explorer. Confirm windows, focus and Snap after Tilework exits and restarts.

## Prepare the release after approval

1. Resolve the private commit metadata before exposing Git history. Verify private vulnerability reporting before relying on the reporting form.
2. On the approved release commit, create the matching tag and dispatch **Windows CI** against that tag with the same **release_tag** and with **installer_qa** enabled. Download the draft attachments and check each hash and BUILDINFO.txt. Keep the tested package as the release artifact.
3. Publish the demo from the default branch's /docs folder only after approval. Open the README link without authentication and exercise the demo on desktop and touch.
4. When a newer stable release exists, check the positive live update result on 0.3.0 and open the release link in the browser. Document the unsigned package and Windows publisher warning for testers. Decision to publish a stable release stays with the maintainer.
