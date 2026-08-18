# Progress

Newest first. One entry per checkpoint.

## 2026-08-15 — Checkpoint 2: end to end, reviews, release candidate (M5–M7)

- Demo command-line host and 15 end-to-end scenarios against the real server; all pass. A windowed demo compiles but has not been run.
- Integration and self-hosting guides; container image, compose file and CI workflow written (not yet built or run).
- End-to-end tests found two bugs, both fixed: a session row was created for users who had switched usage off, and no crash was ever marked fatal.
- Server security review: no markup injection, SQL injection, login bypass, CSRF or cross-app leak. Fixed: catalog replacement that made real events get dropped, one client starving an app's rate limit, unbounded growth, a counter overflow, login limiter and proxy-header weaknesses, missing connection timeouts.
- Design change: the server no longer rejects events for being undeclared. The catalog is client input, so it must never cause data loss; undeclared events are stored with a flag and shown separately.
- Design change: on a first run nothing is sent until the host calls `notice_shown()`; `Builder::send_before_notice` lifts this.
- API additions: `Tracer::begin_turn`, `end_turn`, `turn`; `Builder::send_before_notice`. `listen` calls now declare the catalog themselves.
- New server settings: `TRACER_KEY_RATE_PER_MIN`, `TRACER_MAX_DB_MB`; login cookies are `Secure` by default.
- Schema: `0003_hardening` adds the `declared` flag and per-hash catalogs.
- Live check: ran the server locally, seeded it with the demo host (sessions, a panic, an abort, a hang, a child exit), and read every dashboard page in a browser. Bulk seeding from one address hit the rate limit; the data stayed in the spool and arrived on later launches, as designed.

## 2026-08-15 — Review fixes (SDK)

- Public API added: `Tracer::begin_turn()`, `Tracer::end_turn()` and `Tracer::turn() -> TurnGuard` drive the hang watchdog by turns of the main loop, which works for loops that run only on input; `heartbeat()` stays for continuous loops. `Builder::send_before_notice(bool)` and `Config::send_before_notice`.
- Behaviour change: nothing is sent until `Tracer::notice_shown()` has been called once, unless `send_before_notice(true)` is set.
- Behaviour change: `Builder::listen`, `listen_events` and `listen_all` declare the catalog themselves; `catalog()` now adds to the catalog instead of replacing it.
- `EventCategory` gained an associated const `EVENTS` with an empty default; the `events!` macro fills it in.
- `Sink`: no method changed. A `LOG_TARGET` constant sits beside it for the SDK's own log lines. The panic hook now calls `Sink::count_error`.
- `Signals`: added `forget()`, `begin_turn()` and `end_turn()`.
- `crash.fatal` is now `true` for `child_exit` (and was already for `unclean_exit`); a `panic` stays `false` at capture and is for the server to mark.
- Nine review findings and six lower-confidence notes fixed; see `docs/architecture.md` §3.3, §3.5, §3.7 and §3.8 for the rules they changed.

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
