# Progress

Newest first. One entry per checkpoint.

## 2026-08-15 — Checkpoint 0: foundation (M0)

- Workspace with three crates and `xtask`; gate passes: fmt, names, clippy, tests, wasm32, MSRV 1.90.
- Wire format fixed in `tracer-proto` with two golden fixtures (a full envelope, and one from a "future" client with unknown fields and types).
- SDK public API and the internal `Sink` seam are in place as stubs, so the pipeline and signal halves can be built independently.
- Server schema written and verified to apply to an empty database.
- Decisions recorded in `docs/architecture.md`.
- Note: `sysinfo` is held at 0.38 because 0.39 needs Rust 1.95 and the floor here is 1.90.
