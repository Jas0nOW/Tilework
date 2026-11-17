# Security policy

## Reporting a vulnerability

Use the repository's [private vulnerability reporting form](https://github.com/Jas0nOW/Tilework/security/advisories/new) when available. If you cannot access the form, contact the maintainer privately through an existing channel. Include the Tilework and Windows versions, impact, and steps to reproduce. Remove credentials, private window titles, and local paths from attachments.

## Support and trust boundaries

Security fixes target the current development candidate until the first supported stable release. If you ran an earlier development build, follow the [migration steps](README.md#migrate-from-the-earlier-development-build) to remove its elevated scheduled task.

Tilework runs as a standard user. Opt-in sign-in uses the current user's Windows startup registration. A manual update check reads release metadata from the project's GitHub API over HTTPS and offers the official release page. The user downloads and runs the installer separately. Published SHA-256 sums check the downloaded bytes against the corresponding release assets. The candidate does not include publisher signing.
