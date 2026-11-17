# Contributing

Thanks for helping. Tilework moves real windows, so changes are checked carefully.

## Ground rules

- Read [docs/DESIGN.md](docs/DESIGN.md): it is the specification the code and the tests are measured against. If you change behaviour, change the document in the same pull request.
- Keep it simple: no new dependencies without a strong reason, no unrelated refactoring.
- Code, comments, commit messages and docs are in English. Use Conventional Commits (`feat:`, `fix:`, `docs:`, `test:`, `refactor:`).
- Every bug fix needs a regression test where feasible. The layout logic lives in `src/tree.rs` (pure, no Windows calls). Event handling in `src/app.rs` is tested against a simulated platform (the `Win` trait).

## Before you open a pull request

```
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

CI runs the same on Windows for every pull request.
The [release runbook](docs/RELEASE.md) covers the native smoke test and installer verification.

## Testing safely

Do not test an experimental build on your real windows. Use `--dump` (read-only) or `--filter TEXT` with disposable test windows, and `--restore-snap` if Windows Snap stays off after a crash.

## Reporting bugs

Use the bug report template. Please attach the output of `tilework.exe --dump` and the relevant part of the log. Window titles are left out of the log by default; still check the files for private data before attaching them.
