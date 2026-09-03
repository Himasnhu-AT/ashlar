# Architecture

ashlar is a workspace of small crates stacked in ranks. An app depends on the umbrella crate
`ashlar` and turns on the features it needs; each layer below it can also be used on its own.

## Crate ranks

A crate depends only on crates of a **lower** rank (dev-dependencies are exempt). Ranks 0–4 never
depend on a UI crate (`egui`, `eframe`, `winit`, `egui-wgpu`, `egui_kittest`), and ranks 0–1 build
for `wasm32-unknown-unknown`. `cargo xtask layers` and `cargo xtask wasm` enforce this; the table
below must match `LAYERS` in `xtask/src/main.rs` (a crate missing from `LAYERS` fails the gate).

| Rank | Crate | What it is | Headless | wasm32 |
|---|---|---|---|---|
| 0 | `ashlar-command` | Commands: registry, typed params, one execute chokepoint, discovery | yes | yes |
| 0 | `ashlar-fs` | Files and settings: app dirs, durable atomic writes, keyed settings, recent files | yes | yes |
| 0 | `ashlar-jobs` | Background jobs: progress, cancel, panic isolation, apply-on-UI-thread | yes | yes |
| 0 | `ashlar-log` | Logging and crash safety: directive filters, stderr + rotating file + ring, panic hook, launch guard | yes | yes |
| 1 | `ashlar-store` | Documents: `Arc` snapshots, revisions, undo history, versioned formats, autosave | yes | yes |
| 2 | `ashlar-native` | Native OS features behind one API: dialogs, menus, clipboard, power, notifications, secrets | yes | – |
| 2 | `ashlar-control` | Agent control port: hardened JSON-lines server, client, standard method set | yes | – |
| 2 | `ashlar-print` | System PDF print spooler (CUPS, Windows) | yes | – |
| 3 | `ashlar-mcp` | MCP server generated from the command registry (stdio), headless or bridged | yes | – |
| 4 | `ashlar-cli` | Standard CLI verbs over the command registry | yes | – |
| 5 | `ashlar-ui` | UI components on egui: theme tokens, controls, layout, docking, icons | no | – |
| 6 | `ashlar-app` | Runtime: App builder, frame scheduler, crash guard, logging, telemetry, agent control | no | – |
| 7 | `ashlar` | Umbrella crate: re-exports the others behind features | no | – |
| 8 | `ashlar-example-gallery`, `ashlar-example-notes` | Example apps | no | – |
| 9 | `xtask` | Repo gates and tools (`cargo xtask …`) | – | – |

Adding a crate: give it the lowest rank its dependencies allow, add it to `LAYERS` and to this
table, add `[lints] workspace = true` to its manifest, and add it to `[workspace.dependencies]`
with both `path` and `version`.

## Design principles

1. **Never crash.** No `unwrap`/`expect`/`panic!`/`unreachable!`/`todo!`/`unimplemented!` and no
   `unsafe` outside tests. Every input-derived number is hostile: `get()` instead of indexing,
   checked or saturating math, capped allocations, bounded recursion. Lock poisoning is not fatal.
   Background work runs under `catch_unwind`. Enforced by the workspace clippy lints and
   `cargo xtask deny-panics`.
2. **No app code, no app names.** A crate here serves more than one app and never depends on one.
   App-specific widgets (a timeline, a CAD canvas) stay in the apps and are built from ashlar's
   primitives. Docs say "a video editor", "a DAW". Enforced by `cargo xtask names`.
3. **A headless core.** Documents, commands, jobs and files never touch the UI, so they run in
   tests, in a CLI, behind MCP and in the browser. The UI is openly egui; there is no
   renderer-agnostic abstraction.
4. **Everything is a command.** Menus, shortcuts, the palette, the CLI and MCP all go through one
   registry and one execute chokepoint, so anything a user can do an agent can do.
5. **Performance is a feature.** Idle apps do zero work (repaint on demand). Caches are keyed by
   revision numbers, not by diffing. Long lists are virtualized. No blocking I/O on the UI thread.
   Edits are copy-on-write with structural sharing. Every hot path has a perf test, and
   `cargo xtask bench --compare` guards it (see below).
6. **Stable contracts.** Items marked `CONTRACT` in `ashlar-command` are shared across crates
   built in parallel and change only additively.

## Performance gate

Hot paths have perf tests: `#[test] #[ignore]` functions whose names start with `perf`, usually in
`tests/perf.rs`. They print their numbers as

```text
bench: <metric> <value> <unit>        e.g. bench: edit_100k 812 ns
```

Units ending in `/s` are higher-is-better; all others lower-is-better. A test that prints no
`bench:` line is measured by wall time, which is noisy, so prefer explicit metrics.

```sh
cargo xtask bench                             # writes target/bench/bench.{json,md}
cp target/bench/bench.json baseline.json      # before a change
cargo xtask bench --repeat 3 --compare baseline.json   # fails on a >5% regression
```

`--repeat N` keeps the best value of each metric over N rounds; `--threshold PCT` changes the 5%;
`--filter TEXT` runs only matching tests.

## Dependencies

`cargo xtask ci` checks duplicate versions and sources of every dependency when cargo-deny is
installed (`deny.toml`); `cargo xtask advisories` also checks the advisory database.
