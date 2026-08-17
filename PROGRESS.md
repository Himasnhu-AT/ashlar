# Progress

Newest first. One entry per checkpoint.

## 2026-08-15 — Checkpoint 1: SDK, server and dashboard (M1–M4)

- SDK pipeline and signals built in parallel against the `Sink` seam and merged.
- Server core and dashboard built and merged.
- Design change found during the build: several processes of one host app (window, command-line tool, child workers) share a data directory, so each process now owns a locked session folder in the spool, and a folder whose owner has died is adopted and reported exactly once. `docs/architecture.md` §3.2 has the details.
- Bug found and fixed after the merge: the combined test run aborted about once in twelve runs. A test helper restored the panic hook from a thread that was already panicking. The same hazard existed in one unusual production path (starting the SDK from a destructor during unwinding), which is now guarded.
- `Sink` trait changed: `data_dir` removed, `session_dir` and `session_summary` added.
- Schema: `daily_sessions.opens` added to `0001_init`; `0002_dashboard` adds three indexes.
- Not done: no allocation proof for the realtime counters, because a counting allocator needs `unsafe`, which is forbidden here. They are a single atomic operation by construction.

## 2026-08-15 — Checkpoint 0: foundation (M0)

- Workspace with three crates and `xtask`; gate passes: fmt, names, clippy, tests, wasm32, MSRV 1.90.
- Wire format fixed in `tracer-proto` with two golden fixtures (a full envelope, and one from a "future" client with unknown fields and types).
- SDK public API and the internal `Sink` seam are in place as stubs, so the pipeline and signal halves can be built independently.
- Server schema written and verified to apply to an empty database.
- Decisions recorded in `docs/architecture.md`.
- Note: `sysinfo` is held at 0.38 because 0.39 needs Rust 1.95 and the floor here is 1.90.
