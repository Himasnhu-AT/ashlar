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

- [x] M5 Demo hosts and end-to-end tests: 15 scenarios drive the real SDK against the real server (normal, panic, abort, hang, child exit, offline and replay, consent, kill switch, selective listening, two processes, forget, first-run hold, undeclared events).
- [x] M6 Integration and self-hosting guides.
- [x] M7 Bug review of the SDK: nine findings and six lesser notes fixed, plus a lock-order deadlock between "forget" and the worker.
- [x] M7 Security review of the server: two high and six medium findings fixed; the two confirmed attacks were re-run against a real server and no longer work.
- [x] M7 Dashboard checked in a real browser at desktop width with live data from the demo host.

## To do next

- [ ] Build and run the container image once (`deploy/Dockerfile`, `deploy/compose.yml`). Written but never built.
- [ ] Run the windowed demo (`examples/demo-gui`). It compiles and passes lints but has never been opened.
- [ ] Check the dashboard on a narrow (360 px) screen, and re-check the chart margin change by eye.
- [ ] Run the gate on Windows and Linux. Everything so far ran on macOS only; the CI workflow covers all three once the repository is pushed.
- [ ] Check the login cookie (`Secure`, `__Host-` prefix) in a browser over real HTTPS.
- [ ] Decide whether to prove "no allocation" for the realtime counters, which needs an `unsafe` exemption for one test file.
- [ ] Tag 0.1.0 once the above are done.

## Needs a person

- [ ] Go-ahead to make the repository public and publish 0.1.0 to crates.io.
