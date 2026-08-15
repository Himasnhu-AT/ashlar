# Tracer

Telemetry SDK (`tracer-sdk`) and self-hosted backend (`tracer-server`) for a family of Rust desktop apps, sharing wire types through `tracer-proto`. Read `docs/architecture.md` before changing anything; it is the spec.

## Rules

1. **Naming.** Never write the name of any app that uses this SDK — folder, product or crate name — in code, comments, docs, tests or examples. Say "host app". `cargo xtask names` enforces this.
2. **The gate.** `cargo xtask ci` must pass before every commit: fmt, names, clippy with `-D warnings`, tests, wasm32 check, MSRV 1.90 check.
3. **No `unsafe`.** It is forbidden workspace-wide. Native crash capture is a later phase in its own crate.
4. **No panics in library code.** `unwrap`, `expect`, `panic!`, `todo!`, `unreachable!` are denied outside tests. The SDK must never take a host app down: on any internal failure it drops data and counts the drop.
5. **The SDK never blocks or prints.** No call on `Tracer` waits on the network or disk queue, and nothing writes to stdout (hosts use it as a protocol channel). No tokio in the SDK.
6. **Privacy by construction.** Event names and property strings are `&'static str`. Never add an API that takes user content. Scrub messages, backtraces and log lines before they are queued. Every collected field is listed in `docs/privacy.md`; update it in the same change.
7. **The wire format only grows.** Add optional fields; never rename or repurpose one. Update `docs/wire-format.md` and the fixtures in the same change.
8. **Migrations are append-only.** Add a new SQL file; never edit one that has shipped.
9. **Dependencies:** few, small, permissively licensed (MIT/Apache/BSD), compatible with Rust 1.90.
10. **Environment files.** Never read `.env`. Server settings are documented in `.env.example` with placeholders only.

## Layout

- `crates/tracer-proto` — wire types, no I/O, builds for wasm32.
- `crates/tracer-sdk` — the client. `pipeline` (queue, spool, session, consent, sending) and `signals` (crash, hang, perf, sys, logs) meet only at the `Sink` trait in `sink.rs`.
- `crates/tracer-server` — ingest, SQLite storage, rollups, dashboard, reports.
- `xtask` — the gate.

## Tracking work

- `ROADMAP.md` — milestones and their state.
- `TODO.md` — what is done and what is next.
- `PROGRESS.md` — a dated entry per checkpoint.

Commit at each checkpoint with the milestone id as prefix, for example `M1: spool and transport`.
