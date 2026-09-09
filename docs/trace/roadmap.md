# Telemetry (trace) roadmap

**Status, 2026-09-09:** release candidate, load-tested. The SDK (`ashlar-trace`), server
(`ashlar-trace-server`), dashboard, demo hosts and guides are built; the SDK is tested end to end
against the real server; a bug review of the SDK and a security review of the server are done and
their findings fixed. Moved into ashlar with its history; apps use it through `ashlar-telemetry`.
Not yet published.

## Version 0.1

| Id | Milestone | State |
|---|---|---|
| M0 | Foundation: workspace, gate, wire format with fixtures, SDK and server skeletons, schema, docs | Done |
| M1 | SDK pipeline: consent, identity, session, queue, spool, transport, event catalog, usage | Done |
| M2 | SDK signals: panic hook, handled errors, unclean exit, child exit, hang watchdog, frame and system metrics, breadcrumbs, local report | Done |
| M3 | Server core: ingest, write keys, rate limits, dedup, SQLite store, rollups, retention, admin commands | Done |
| M4 | Dashboard and reports: overview, crashes, performance, usage, per-release report export | Done |
| M5 | Demo hosts and end-to-end tests | Done |
| M6 | Guides and packaging: integration guide, self-hosting guide, container image | Done |
| M7 | Hardening and 0.1.0 release | Release candidate |
| M8 | Tests, stress and load: edge-case and fuzz suites, SDK hot-path and server ingest optimisation, large-database read path | Done; two load targets missed (below) |

## Still open

- Find why ingest latency collapses at 2,000 envelopes a second once the database passes about 3 GB. Suspected: index inserts outgrowing the cache; not proven.
- Cut ingest tail latency while retention deletes a large backlog (p99 about 300 ms against about 50 ms).
- Repeat the load runs on Linux and on a quiet machine; all numbers so far are from one busy macOS machine.
- Build and run the container image once (`deploy/trace/Dockerfile`, `deploy/trace/compose.yml`). Written but never built.
- Run the windowed demo (`examples/trace-demo-gui`). It compiles and passes lints but has never been opened.
- Check the dashboard on a narrow (360 px) screen.
- Check the login cookie (`Secure`, `__Host-` prefix) in a browser over real HTTPS.
- Decide whether to prove "no allocation" for the realtime counters, which needs an `unsafe` exemption for one test file.

## Later

| Milestone | Notes |
|---|---|
| Native crashes | Out-of-process monitor, minidumps, symbol upload and server-side symbolication, in an isolated crate that is the only one allowed `unsafe`. |
| Postgres | A second `Store` implementation for larger deployments. |
| Web target | A transport for wasm32, where the SDK is currently a no-op. |
| Alerts | Email or webhook on a new issue or a crash-rate spike. |
| OTLP export | Forward stored data to OpenTelemetry collectors. |
