# Roadmap

**Status, 2026-08-18:** release candidate, load-tested. SDK, server, dashboard, demo hosts and guides are built; the SDK is tested end to end against the real server; a bug review of the SDK and a security review of the server are done and their findings fixed. Not yet published. See `TODO.md` for what is still unverified.

## Version 0.1

| Id | Milestone | State |
|---|---|---|
| M0 | Foundation: workspace, gate, wire format with fixtures, SDK and server skeletons, schema, docs | Done |
| M1 | SDK pipeline: consent, identity, session, queue, spool, transport, event catalog, usage | Done |
| M2 | SDK signals: panic hook, handled errors, unclean exit, child exit, hang watchdog, frame and system metrics, breadcrumbs, local report | Done |
| M3 | Server core: ingest, write keys, rate limits, dedup, SQLite store, rollups, retention, admin commands | Done |
| M4 | Dashboard and reports: overview, crashes, performance, usage, per-release report export | Done |
| M5 | Demo hosts and end-to-end tests | Done |
| M6 | Guides and packaging: integration guide, self-hosting guide, container image, CI | Done |
| M7 | Hardening and 0.1.0 release | Release candidate; publishing waits for a go-ahead |
| M8 | Tests, stress and load: edge-case and fuzz suites, SDK hot-path and server ingest optimisation, large-database read path | Done; two load targets missed, see `PROGRESS.md` |

## Later

| Milestone | Notes |
|---|---|
| Host integration | Wire the SDK into each host app. Done in those repositories. |
| Native crashes | Out-of-process monitor, minidumps, symbol upload and server-side symbolication, in an isolated crate that is the only one allowed `unsafe`. |
| Postgres | A second `Store` implementation for larger deployments. |
| Web target | A transport for wasm32, where the SDK is currently a no-op. |
| Alerts | Email or webhook on a new issue or a crash-rate spike. |
| OTLP export | Forward stored data to OpenTelemetry collectors. |
