# To do

Updated 2026-08-15.

## Done

- [x] Workspace, lints and `cargo xtask ci` gate (fmt, names, clippy, tests, wasm32, MSRV 1.90).
- [x] `tracer-proto`: envelope, item types, frame histogram, name validation; golden fixtures.
- [x] `tracer-sdk` skeleton: public `Tracer` and `Builder` API, `Sink` seam, `Transport` trait.
- [x] `tracer-server` skeleton: schema migration `0001_init`, applied in a test.
- [x] Architecture, wire-format and privacy docs.

## To do next

- [ ] M1 SDK pipeline.
- [ ] M2 SDK signals.
- [ ] M3 Server core.
- [ ] M4 Dashboard and reports.
- [ ] M5 Demo hosts and end-to-end tests.
- [ ] M6 Guides and packaging.
- [ ] M7 Hardening; tag 0.1.0.

## Needs a person

- [ ] Go-ahead to make the repository public and publish 0.1.0 to crates.io.
