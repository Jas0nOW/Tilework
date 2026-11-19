# Tilework lessons

## 2026-11-17 — Verify setup under a clean Windows user

- A GitHub runner starts with an administrator token. The per-user Tilework installer rejects that context.
- A child started with a new local account and LoadUserProfile can still inherit runner environment variables. Validate the child token and profile, then set the test paths from that profile before touching files.
- Verify with the private Windows CI workflow, installer_qa enabled and release_tag empty. This verifies standard-user installation and registration. Shell sign-in and reboot require an interactive Windows desktop.

## 2026-11-19 — Test the one-click update live from an elevated shell

- Symptom: the agent shell is elevated, so setup refuses to run and Tilework skips standard-user windows. `taskkill` without `/F` does not close Tilework.
- Run setup and Tilework with a standard-user token through a temporary scheduled task with `-RunLevel Limited -LogonType Interactive`, then delete the task.
- Close Tilework by posting WM_CLOSE to the window of class `TileworkMain`. In PowerShell, pass `[NullString]::Value` as the title, because `$null` becomes "" and FindWindow finds nothing.
- Trigger an install without UI by posting WM_TRAY (0x8002) with lParam NIN_BALLOONUSERCLICK (0x405). Reset the 24 h gate by deleting `%LOCALAPPDATA%\Tilework\update-state.ini`.
- Older machines can hold `%LOCALAPPDATA%\Tilework\tilework.exe` from a development build. Setup then stops at the legacy guard; remove it as the README migration describes.
- Verify: the installed exe hash equals the release's Portable hash, and the log shows "verified; shutting down for the installer" followed by "started".
