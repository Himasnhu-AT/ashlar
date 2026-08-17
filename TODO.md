# To do

Updated 2026-08-15.

## Done

- [x] Workspace, lints and `cargo xtask ci` gate (fmt, names, clippy, tests, wasm32, MSRV 1.90).
- [x] `tracer-proto`: envelope, item types, frame histogram, name validation; golden fixtures.
- [x] `tracer-sdk` skeleton: public `Tracer` and `Builder` API, `Sink` seam, `Transport` trait.
- [x] `tracer-server` skeleton: schema migration `0001_init`, applied in a test.
- [x] Architecture, wire-format and privacy docs.

- [x] M1 SDK pipeline: consent, identity, sessions, queue, per-process spool, sending, event catalog macro. Checked by 32 integration tests with real child processes.
- [x] M2 SDK signals: panic hook, handled errors, child exits, hang watchdog, frame and system metrics, breadcrumbs, scrubbing, local report.
- [x] M3 Server core: ingest, write keys, rate limits, dedup, SQLite store, rollups, retention, admin commands. Checked against a real server on an ephemeral port.
- [x] M4 Dashboard and reports: login, overview, crashes, performance, usage, release reports, settings.
- [x] Container image, compose file, CI workflow and `.env.example` written.

## To do next

- [ ] M5 Demo hosts and end-to-end tests (SDK against the real server).
- [ ] M6 Integration and self-hosting guides.
- [ ] M7 Fix findings from the bug review of the SDK and the security review of the server.
- [ ] M7 Check the dashboard in a real browser, including a 360 px wide screen.
- [ ] M7 Build and run the container image once.
- [ ] M7 Tag 0.1.0.

## Needs a person

- [ ] Go-ahead to make the repository public and publish 0.1.0 to crates.io.
