# ashlar

**A lightweight, modern application framework for Rust, powered by [egui](https://github.com/emilk/egui).**

[![CI](https://github.com/Himasnhu-AT/ashlar/actions/workflows/ci.yml/badge.svg)](https://github.com/Himasnhu-AT/ashlar/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/ashlar-rs.svg)](https://crates.io/crates/ashlar-rs)
[![docs.rs](https://img.shields.io/docsrs/ashlar-rs)](https://docs.rs/ashlar-rs)
[![MSRV](https://img.shields.io/badge/MSRV-1.95-blue.svg)](#compatibility)
[![License: BSD-3-Clause](https://img.shields.io/badge/license-BSD--3--Clause-blue.svg)](LICENSE)

ashlar gives a desktop (and, for its headless core, web) application the parts every app
rebuilds: documents with undo and autosave; commands that drive menus, shortcuts, a command
palette, a CLI and an MCP server; background jobs; files and settings; native OS features; a
component library on egui; and crash safety and telemetry. An idle ashlar app does no work at all.

> **Status:** early (0.1). The API will change between minor versions until 1.0.

## Why ashlar

- **Everything is a command.** Menus, shortcuts, the palette, the CLI, the control port and MCP all
  go through one registry and one execute chokepoint, so anything a user can do, a script or an
  AI agent can do too.
- **Never crashes.** No `unwrap`, `panic!` or `unsafe` outside tests (enforced by lints and a CI
  gate). Panics in a view, a command or a job are caught; the session survives and unsaved work
  is recovered on the next launch.
- **Zero work when idle.** Frames are drawn only when something asks for one: input, a document
  change, a finished job. No polling.
- **Headless core.** Documents, commands, jobs and files never touch the UI, so they run in tests,
  in a CLI, behind MCP and in the browser.
- **Small crates.** Use the umbrella crate, or pick only the layers you need: each `ashlar-*` crate
  works on its own.

## Quick start

```sh
cargo add ashlar-rs
```

The package is published as `ashlar-rs`; the library is still imported as `ashlar`.

```rust,no_run
use ashlar::prelude::*;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Doc {
    notes: Vec<String>,
}

fn add(s: &mut Session<Doc>, p: &Value) -> Result<Value, CommandError> {
    let text = p.get("text").and_then(Value::as_str).unwrap_or("New note").to_string();
    s.edit("Add Note", |d| d.notes.push(text)); // one undo step per command
    Ok(json!(s.doc().notes.len()))
}

fn main() -> Result<(), ashlar::app::Error> {
    App::<Doc>::new("jot", env!("CARGO_PKG_VERSION"))
        .format(Format::new("example.jot", "jot 0.1"))
        .commands([cmd!("note.add", "Add Note", ["Edit"], Some("Mod+Shift+N"), [Param::string("text")], always, add)])
        .view(|ui, cx: &mut Cx<'_, Doc>| {
            for n in &cx.doc().notes {
                ui.label(n);
            }
            if ui.button("Add").clicked() {
                cx.dispatch("note.add", json!({}));
            }
        })
        .run() // undo/redo, save/open/autosave, menus, palette, crash guard, control port
}
```

`App::cli()` turns the same definition into a `-cli` binary with `commands`, `exec`, `run`, `mcp`
and `serve`. `--control <port>` lets an agent drive the window. Try the examples:

```sh
cargo run -p ashlar-example-notes      # a small document app
cargo run -p ashlar-example-gallery    # every UI component
```

The umbrella crate's optional features are `native-menu` (the app's commands in the macOS and
Windows menu bar) and `telemetry` (crash, performance and usage reports to a server you host):

```sh
cargo add ashlar-rs --features native-menu,telemetry
```

## Crates

Every crate is published on [crates.io](https://crates.io/search?q=ashlar-) under the same
version, with its API docs at `https://docs.rs/<crate>`. Depend on a single layer when that's all
you need, for example `cargo add ashlar-store` for documents and undo in a headless tool.

| Crate | What it is |
|---|---|
| [`ashlar-rs`](crates/ashlar) | The umbrella crate (`use ashlar::…`): re-exports the layers below behind features |
| [`ashlar-app`](crates/app) | The runtime: App builder, frame scheduler, crash guard, logging, agent control |
| [`ashlar-ui`](crates/ui) | UI components on egui: theme tokens, controls, layout, docking, icons |
| [`ashlar-store`](crates/store) | Documents: `Arc` snapshots, revisions, undo history, versioned formats, autosave |
| [`ashlar-command`](crates/command) | Commands: registry, typed params, one execute chokepoint, discovery |
| [`ashlar-jobs`](crates/jobs) | Background jobs: progress, cancel, panic isolation, apply on the UI thread |
| [`ashlar-fs`](crates/fs) | Files and settings: app folders, atomic writes, keyed settings, recent files |
| [`ashlar-log`](crates/log) | Logging and crash safety: filters, rotating file, in-memory ring, panic hook |
| [`ashlar-native`](crates/native) | OS features behind one API: dialogs, menus, clipboard, power, notifications, secrets |
| [`ashlar-control`](crates/control) | The agent control port: a hardened JSON-lines server and client |
| [`ashlar-cli`](crates/cli) | Standard CLI verbs over the command registry |
| [`ashlar-mcp`](crates/mcp), [`ashlar-mcp-http`](crates/mcp-http) | An MCP server generated from the command registry (stdio, or HTTP on loopback) |
| [`ashlar-agents`](crates/agents) | Connect coding agents to an app: MCP config, skills and rules |
| [`ashlar-assistant`](crates/assistant) | An in-app AI assistant harness, independent of the model provider |
| [`ashlar-integrations`](crates/integrations) | Code-signed tool servers installed from a catalog and run over MCP |
| [`ashlar-debug`](crates/debug) | Drive a running app's UI from the shell: snapshot, click, wait, screenshot |
| [`ashlar-docs`](crates/docs) | Documentation for agents, embedded at build time |
| [`ashlar-update`](crates/update) | Update checks against a releases API |
| [`ashlar-print`](crates/print) | Send a PDF to the system print spooler (CUPS, Windows) |
| [`ashlar-obfuscate`](crates/obfuscate) | Compile-time string obfuscation |
| [`ashlar-telemetry`](crates/telemetry), [`ashlar-trace`](crates/trace), [`ashlar-trace-server`](crates/trace-server), [`ashlar-trace-proto`](crates/trace-proto) | Self-hosted crash, performance and usage telemetry ([docs](docs/trace/README.md)) |

How the crates stack is in [docs/architecture.md](docs/architecture.md).

## Compatibility

| | Supported |
|---|---|
| Rust | 1.95 or newer (edition 2024). CI tests on the latest stable and checks the MSRV. |
| egui / eframe | 0.36, wgpu renderer |
| macOS | Apple silicon and Intel (CI: `macos-latest`, arm64) |
| Windows | 10 or newer, x86_64 (CI: `windows-latest`) |
| Linux | x86_64, X11 and Wayland (CI: `ubuntu-latest`). Native dialogs use the XDG desktop portal. |
| Web | `wasm32-unknown-unknown` for the headless core (commands, store, files, jobs, logging; checked in CI). The windowed runtime on the web is experimental. |

**Linux build dependencies** (Debian/Ubuntu names):

```sh
sudo apt-get install libgtk-3-dev libxdo-dev libxkbcommon-dev libdbus-1-dev
```

Native features differ by platform. Where one isn't available, the call returns
`NativeError::Unsupported` and nothing panics. Some highlights (the full matrix is in
[crates/native](crates/native/README.md#support-matrix)):

| Feature | macOS | Windows | Linux | Web |
|---|---|---|---|---|
| File dialogs | yes | yes | yes (portal) | open only |
| Native menu bar | yes | yes | in-window menu | in-window menu |
| Clipboard (text, image) | yes | yes | yes | no |
| Notifications | bundled apps | no | yes (D-Bus) | no |
| Secrets (OS keychain) | Keychain | Credential Manager | Secret Service | no |
| Printing | CUPS | Windows spooler | CUPS | no |

Built on [egui](https://github.com/emilk/egui), [eframe](https://github.com/emilk/egui/tree/master/crates/eframe),
[wgpu](https://github.com/gfx-rs/wgpu), [rfd](https://github.com/PolyMeilex/rfd),
[muda](https://github.com/tauri-apps/muda) and [rustls](https://github.com/rustls/rustls).

## Built with ashlar

- **bay** and **bay-effects** by [Marl Studio](https://www.marlstudio.com) are built on top of ashlar.

Building something with ashlar? Open a pull request to add it here.

## Documentation

- [Architecture](docs/architecture.md): crate ranks, design principles, the performance gate
- [Control protocol](docs/control-protocol.md): driving an app from scripts and agents
- [Debugging UIs](docs/debugging-uis.md) with `ashlar-debug`
- [Telemetry](docs/trace/README.md): integration, privacy, self-hosting, wire format
- UI: [docking](docs/ui/ui-docking.md), [visual regression tests](docs/ui/ui-visual-regressions.md)
- API reference on [docs.rs](https://docs.rs/ashlar-rs)

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first. Every change must pass
`cargo xtask ci`. Please follow the [Code of Conduct](CODE_OF_CONDUCT.md), and report security
issues privately as described in [SECURITY.md](SECURITY.md).

## License

ashlar is licensed under the [BSD 3-Clause License](LICENSE). Unless you say otherwise, any
contribution you submit for inclusion in ashlar is licensed the same way.
