# Tilework lessons

## 2026-11-17 — Verify setup under a clean Windows user

- A GitHub runner starts with an administrator token. The per-user Tilework installer rejects that context.
- A child started with a new local account and LoadUserProfile can still inherit runner environment variables. Validate the child token and profile, then set the test paths from that profile before touching files.
- Verify with the private Windows CI workflow, installer_qa enabled and release_tag empty. This verifies standard-user installation and registration. Shell sign-in and reboot require an interactive Windows desktop.
