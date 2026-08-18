# ashlar: rules for agents and developers

ashlar is a Rust library for building fast desktop and web apps: documents and undo, commands,
background jobs, files and settings, native OS features, UI components (egui), and a runtime that
gives every app crash safety, telemetry and agent control (control port, MCP, CLI) for free.
Domain widgets (a timeline, a CAD canvas…) are app-specific and stay in the apps; ashlar only
provides the general building blocks they are made from. The plan is `../FRAMEWORK-PLAN.md`.

## 0. Never crash
No `unwrap`/`expect`/`panic!`/`unreachable!`/`todo!`/`unimplemented!` outside tests (denied in
`[workspace.lints.clippy]`), no `unsafe` (forbidden; only `ashlar-native` may later relax this for
FFI, one module at a time, with `// SAFETY:` comments). Every input-derived number is hostile:
`get()` not `[i]`, checked/saturating math, capped allocations, bounded recursion. Lock poisoning is
not fatal (`lock().unwrap_or_else(PoisonError::into_inner)`). Background work runs under
`catch_unwind`. Integration-test files start with `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`.

## 1. No app code, no app names
A crate here must serve more than one app and never depend on one. Never name the apps that use
ashlar in public API, docs or identifiers: say "a video editor", "a DAW". (`cargo xtask names` will enforce this.)

## 2. Layering (`cargo xtask layers`)
Crates have a rank (see `xtask/src/main.rs`); a crate depends only on lower ranks. Ranks 0–4 never
depend on egui/eframe/winit: the core is headless and builds for wasm32.

## 3. Performance is a feature
Idle apps do zero work (repaint on demand). Caches are keyed by revision numbers, not by diffing.
Long lists are virtualized. No blocking I/O on the UI thread. Benches guard every hot path.

## 4. Gates before every commit
`cargo xtask ci` (fmt check, layers, clippy -D warnings, tests). One task id per commit
(`AS-P1.2: history merge keys`), only green states, attribution line at the end.

## 5. Stable contracts
Items marked `CONTRACT` in `ashlar-command` are shared across crates built in parallel: change
them only additively.

## 6. Working in parallel
One git worktree per agent under `../ashlar-wt/<name>`, branch `wt/<name>`, own `CARGO_TARGET_DIR`
(`target/agent-<name>` inside the worktree). Never commit on `main`; the integrator merges.
Build and test only your crates (`cargo test -p <crate>`); the full gate runs at integration.
