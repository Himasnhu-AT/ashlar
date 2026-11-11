# Agents and automation

Because every action is a command, an ashlar app can be driven from outside in four ways, all with
one vocabulary of methods:

| Surface | Transport | Use it for |
| --- | --- | --- |
| CLI | one-shot verbs, one-line JSON replies | scripts, CI, batch jobs |
| Control port | JSON lines over TCP on `127.0.0.1`, with a key | driving the running window |
| MCP | JSON-RPC 2.0 over stdio | AI agents |
| Child process | JSON lines over stdin/stdout | a parent process that owns the app |

The method list, the security model and the reply format are in the
[control protocol](../docs/control-protocol.md) reference.

## A CLI binary

`App::cli()` turns the app definition into a command-line tool that runs the same commands on a
headless session. Put the app definition in a library function and add a second binary:

```rust,ignore
// src/bin/notes-cli.rs
fn main() -> std::process::ExitCode {
    ashlar::app::cli_main(notes::app())
}
```

```text
notes-cli commands [--json] [--query Q]          list commands
notes-cli describe <id>                          one command in full (parameter schema)
notes-cli docs [category|path.md] [--query Q]   the documentation
notes-cli exec <id> [json|-]                     run one command
notes-cli run <file.jsonl|-> [--keep-going]    run {"command","params"} lines
notes-cli mcp [--isolate] [--per-command]        MCP server on stdio
notes-cli serve                                  control protocol on stdio
notes-cli version [--json]
```

```sh
notes-cli exec note.add '{"title": "Groceries"}'
# {"ok":true,"result":{"count":1,"id":1}}
```

Replies are one line, `{"ok":true,"result":…}` or `{"ok":false,"error":"…"}`. Exit codes are 0
(ok), 1 (the command failed) and 2 (usage). `commands`, `describe`, `exec`, `run` and `mcp` take
`--connect PORT` to drive the running app instead of a fresh headless session.

To add your own verbs, take the `ashlar::cli::Cli` from `App::cli()` and call `.verb(…)` before
`.run()`.

## The control port

Start the app with a port, and it serves the control protocol on loopback:

```sh
notes --control 7700          # or NOTES_CONTROL_PORT=7700, or App::control_port(7700)
notes-cli exec note.list '{}' --connect 7700
```

When the port opens, the app writes a random key to a file in its config folder (mode 0600). Every
request must carry it, so another local user or a web page cannot drive the app. Clients in this
repository (`--connect`, `ashlar-debug`) find the key file themselves.

Besides commands (`engine.commands`, `engine.execute`, `engine.batch`), the running window answers
UI methods: `ui.snapshot` (an outline of the UI with refs), `ui.click`, `ui.fill`, `ui.press`,
`ui.wait`, `ui.screenshot`, `app.clock`, `app.stats`, `app.logs` and more.

`ashlar-debug` drives a running app from the shell with those methods:

```sh
ashlar-debug open ./target/debug/notes
ashlar-debug snapshot -i
ashlar-debug fill @e2 "Groceries" --submit
ashlar-debug screenshot --annotate
ashlar-debug close
```

See [Testing and debugging UIs](../docs/debugging-uis.md).

## MCP

`notes-cli mcp` is an MCP server on stdio. Its tools are generated from the command registry
(`list_commands`, `describe_command`, `execute`, `batch`), plus the UI tools (`ui_snapshot`,
`ui_click`, `screenshot`, …) when it is connected to a running window with `--connect PORT`.
`--per-command` also makes each command its own tool with its real input schema.

A client config entry looks like any stdio server:

```json
{
  "mcpServers": {
    "notes": { "command": "notes-cli", "args": ["mcp"] }
  }
}
```

`App::instructions("…")` sets what MCP clients are told about the app; write it for an agent that
has never seen it.

The `ashlar-agents` crate writes these config entries, plus skills and rules, for common coding
agents, and removes exactly what it added. `ashlar-mcp-http` serves MCP over HTTP on loopback
instead of stdio.

## Agents and undo

Agents go through the same chokepoint as users: parameter validation, enabled predicates, undo.
A command an agent runs is one undo step the user can take back, and the view can tell it apart
(`cx.origin()`) to animate the change instead of jumping.
