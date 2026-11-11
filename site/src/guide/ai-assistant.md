# Building with an AI assistant

ashlar apps are easy for a coding agent to check: the agent can build the app, run it with no
window, look at a PNG of it, read its UI tree, and drive it through the same commands a user runs.
This page has a rules file to give the agent, and the steps to connect an agent to your running
app.

## Docs for agents

- [llms.txt](https://ashlar.himanshuat.com/llms.txt): an index of every page of this site.
- [llms-full.txt](https://ashlar.himanshuat.com/llms-full.txt): the whole site as one markdown file.
- Each page's markdown is at its URL plus `.md`, for example
  `https://ashlar.himanshuat.com/guide/commands.md`.
- The API reference: [docs.rs/ashlar-rs](https://docs.rs/ashlar-rs).

## A rules file for your project

Copy this into `AGENTS.md` or `CLAUDE.md` at the root of your app's repository and adjust the names
(`my-app`, `my-app-cli`) to yours.

````markdown
# Building this app

This is a desktop app built on ashlar (crate `ashlar-rs`, imported as `ashlar`), a Rust
framework on egui. Docs: https://ashlar.himanshuat.com/llms-full.txt and
https://docs.rs/ashlar-rs. Read the relevant page before using an API you have not used here.

## Never crash
- No `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!` or `unimplemented!` outside tests.
  The crate root has
  `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]`.
- Numbers from input (files, params, the network) are hostile: `get()` not `[i]`, checked or
  saturating math, capped allocations.
- Commands return `CommandError::bad_params` / `CommandError::failed` instead of panicking.
- No blocking I/O on the UI thread: long work is a job (`cx.jobs.spawn`).

## Adding a feature
- Every user action is a command: a `fn(&mut Session<Doc>, &Value) -> Result<Value, CommandError>`
  registered with `cmd!` or `CommandSpec::new`, with typed `Param`s, a menu path and an enabled
  predicate. Views call `cx.dispatch("id", json!({…}))`; they do not change the document directly
  except for live gestures (`cx.edit_merged`).
- Each command changes the document with one `s.edit("Label", |d| …)`: one undo step.
- Give every interactive widget a stable automation id (`notes.add`, not a generated one), so tests
  and agents can find it.
- Cache derived data by `cx.revision()`; long lists use `ashlar::ui::virtual_list`.

## Checking your work
- `cargo clippy --all-targets -- -D warnings` and `cargo test` pass.
- Run a command with no window: `cargo run --bin my-app-cli -- exec <id> '<json>'`.
- Look at the UI with no window:
  `cargo run -- --headless --size 1024x700 --scene <scene> --snap out.png --tree tree.json`,
  then read `out.png` and `tree.json`.
- Script an interaction in a `.ui` file (`exec`, `click id=…`, `fill id=… "text"`, `press Mod+S`,
  `wait --text "…"`, `assert exists id=…`) and run it with `--headless --script file.ui`
  (exit code 0 = passed, 1 = a step failed).
- New screens get a scene (`App::scene`) and a golden snapshot test (`Driver::assert_snapshot`).
````

## Connecting an agent to your app

An app with a `-cli` binary is an MCP server: `my-app-cli mcp`. Build the binary and register it
with your agent. For Claude Code:

```sh
cargo build --bin my-app-cli
claude mcp add my-app -- ./target/debug/my-app-cli mcp
```

For agents configured with JSON:

```json
{
  "mcpServers": {
    "my-app": { "command": "/path/to/my-app-cli", "args": ["mcp"] }
  }
}
```

The agent then lists and runs your commands on a headless session (`list_commands`,
`describe_command`, `execute`). To let it see and drive the running window as well, start the app
with a control port and connect the server to it:

```sh
cargo run -- --control 7700
claude mcp add my-app-live -- ./target/debug/my-app-cli mcp --connect 7700
```

With `--connect` the agent also gets `screenshot`, `ui_snapshot`, `ui_click`, `ui_fill`,
`ui_wait` and the other UI tools. Everything it does goes through the same commands and undo
history as the user, so a change it makes can be undone with `Mod+Z`.

Write `App::instructions("…")` for the agent: what the app is, what its main commands do, and how
to save. MCP clients show it to the model when they connect.

See [Agents and automation](agents.md) for the CLI and the control port, and the
[control protocol](../docs/control-protocol.md) for every method and tool.
