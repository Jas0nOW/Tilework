# Security

Tilework can run with administrator rights to manage elevated windows. It reads no network data and executes nothing from its configuration file. It writes only its log, its settings and a small recovery marker under `%LOCALAPPDATA%\Tilework`.

If you find a security problem (for example a way to make the elevated process write or delete files outside that folder), please report it privately to the maintainer instead of opening a public issue.

Known open item: a hostile process of the same user could replace the data directory while Tilework is writing to it (see the hardening notes in [docs/DESIGN.md](docs/DESIGN.md)).
