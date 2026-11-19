# Security policy

## Reporting a vulnerability

Use the repository's [private vulnerability reporting form](https://github.com/Jas0nOW/Tilework/security/advisories/new) when available. If you cannot access the form, contact the maintainer privately through an existing channel. Include the Tilework and Windows versions, impact, and steps to reproduce. Remove credentials, private window titles, and local paths from attachments.

## Support and trust boundaries

Security fixes target the latest beta release (currently 0.4.1) until the first stable release. If you ran an earlier development build, follow the [migration steps](README.md#migrate-from-the-earlier-development-build) to remove its elevated scheduled task.

Tilework runs as a standard user by default. It runs as administrator only when the user starts it that way. Opt-in sign-in uses the current user's Windows startup registration and always starts as a standard user. The automatic and manual update checks read release metadata from the project's GitHub API over HTTPS. One-click install downloads the installer and `SHA256SUMS.txt` from the same official release, compares the installer's SHA-256 with the published value, and only then runs it. Portable copies and administrator instances only open the release page. The checksum guards against corrupted or altered downloads, not against a compromised release. The binaries are not code-signed.
