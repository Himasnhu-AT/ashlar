# Control protocol

Every ashlar app can be driven by an agent in three ways, all speaking **one method vocabulary**:

| Surface | Transport | Crate |
|---|---|---|
| Control port | JSON lines over TCP `127.0.0.1:<port>`, keyed | `ashlar-control` (`Server`, `Client`) |
| Child process | JSON lines over stdin/stdout, no key (private pipe) | `ashlar-control::serve_stream`, `<app>-cli serve` |
| MCP | JSON-RPC 2.0 over stdio; tools map onto the methods below | `ashlar-mcp` |
| CLI | one-shot verbs, one-line replies | `ashlar-cli` |

Protocol version: **1** (reported by `app.version`).

## Framing

One JSON object per line (`\n`, an optional `\r` is stripped), each way, one reply per request, in
order.

Request:

```json
{"id": 7, "key": "<64 hex digits>", "method": "engine.execute", "params": {"command": "edit.undo"}}
```

- `id` (optional, any JSON): echoed in the reply; omitted from the reply when absent.
- `key`: the app's control key (TCP only; see Keys). Not needed after `auth` on the same connection.
- `method` (required string).
- `params` (optional; `null` or absent means `{}`).

Reply:

```json
{"id": 7, "ok": true, "result": …}
{"id": 7, "ok": false, "error": "human-readable message", "detail": …}
```

`detail` is optional machine-readable context: for a failed command, the command error
`{"kind": "unknown"|"disabled"|"bad_params"|"failed"|"panicked", "id": …, …}`; for a failed batch,
`{"steps": [...]}`.

## Hardening (TCP)

The server is built so that a hostile local process or web page cannot drive or crash the app:

- **Loopback only.** Binds `127.0.0.1`; the client only connects there.
- **Key.** Once the port is bound the app writes 32 random bytes (64 hex digits) to
  `<dir>/control-key.<instance>`, created with mode 0600, where `<dir>` is a folder the app chooses
  (its settings folder) and `<instance>` is the Flatpak instance id or `local-<port>`. The file is
  removed when the server is dropped; a stale one is replaced. Every request must carry the key, as
  the top-level `key` member or once per connection with `auth`. A missing or wrong key gets
  `{"ok":false,"error":"unauthorized"}` and the connection is closed. Keys are compared in constant
  time. `params.key` is an ordinary parameter for every method but `auth` (`ui.key {"key":"Enter"}`).
- **Strict lines.** A line that is not a JSON object with a string `method` (invalid UTF-8, bad JSON,
  an array, a number, an HTTP request line) gets one error reply and the connection is closed, so
  nothing after it runs. This is what stops a browser's cross-origin `fetch` POST: its request line
  `POST / HTTP/1.1` is rejected before the JSON body is read.
- **Bounded lines.** Longer than `max_line` (4 MiB default): error reply, the rest of the line is
  drained (at most 64 MiB, 2 s) so the client can read the error, then close. A peer can never make
  the server buffer more than `max_line`.
- **Connection cap.** At most `max_connections` (16) at once; extra ones get
  `{"ok":false,"error":"too many control connections"}` and are closed.
- **First-request deadline.** A connection must send an authorised request within 30 s. It is one
  overall deadline: blank lines or a slow drip of bytes do not extend it. After that, no idle limit.
- **Reply timeout.** The app thread answers within 60 s or the client gets `timeout: …`.
- **Never panics.** No `unwrap`/indexing on input; junk never reaches the app thread; a request the
  app drops unanswered replies with an error by itself.

## Standard methods

Answered by `ashlar_control::handle` from any `Dispatch` (headless or windowed):

| Method | Params | Result |
|---|---|---|
| `auth` | `{key}` or top-level `key` | `{}` (transport-level) |
| `app.ping` | — | `"pong"` |
| `app.version` | — | `{name, version, protocol, methods: [...]}`: `methods` lists every method this app answers, so clients know whether `ui.*` works |
| `engine.commands` | `{query?}` (case-insensitive, matches id, label, menu path) | `[CommandInfo]`: `{id, label, menu, shortcut, params_schema, params_text, enabled, disabled_reason, mutates}` |
| `engine.execute` | `{command, params?}` | the command's result |
| `engine.batch` | `{commands: [{command, params?}], keepGoing?}` | `[{command, ok, result}]`; on failure an error with `detail.steps` (stops at the first failure unless `keepGoing`; at most 10 000 steps) |

## UI methods

Routed to the app's `UiHandler` (implemented by the egui runtime). A handler may keep a request and
answer it on a later frame. Headless sessions refuse them with a hint. The canonical vocabulary:

| Method | Params | Result |
|---|---|---|
| `ui.inspect` | — | UI state plus every registered element `{id, label, rect:[x,y,w,h]}` |
| `ui.click` | `{id}` or `{x, y}`, `button?` (`primary`/`secondary`/`middle`), `count?`, `modifiers?` | `{}` once processed |
| `ui.drag` | `{from: {id}│{x,y}, to: {id}│{x,y}, steps?, modifiers?}` | `{}` |
| `ui.scroll` | `{x, y, dx, dy}` | `{}` |
| `ui.key` | `{key, modifiers?}` (`modifiers`: `{command, shift, alt, ctrl}`; `command` = ⌘ on macOS, Ctrl elsewhere) | `{}` |
| `ui.type` | `{text}` | `{}` |
| `ui.screenshot` | `{id?}` (crop to an element) | `{png: <base64>, width, height}`: pictures travel inline, never through a predictable temp file |
| `ui.menu.list` | — | the menu tree with enablement |
| `ui.menu.invoke` | `{id}` | as `engine.execute` |
| `ui.set` | app-defined view state (`{theme?, workspace?, …}`) | `{}` |
| `app.quit` | — | `{}`, then the app quits |

Apps may add their own methods (`ui.timeline.hit`…); list them in `UiHandler::methods` so they show
in `app.version`.

## MCP tools

`ashlar-mcp` exposes the same vocabulary as tools:

| Tool | Method |
|---|---|
| `list_commands {query?}` | `engine.commands` (compact: id, label, enabled, mutates) |
| `describe_command {command}` | `engine.commands`, one entry in full, with its parameter schema |
| `execute {command, params?}` | `engine.execute` |
| `batch {commands, keepGoing?}` | `engine.batch` |
| `screenshot {id?}` | `ui.screenshot`, returned as an MCP image |
| `ui_inspect`, `ui_click`, `ui_drag`, `ui_scroll`, `ui_key`, `ui_type`, `ui_menu_list`, `ui_menu_invoke`, `ui_set` | the `ui.*` method of the same name |

The `ui_*` tools and `screenshot` are listed only when the backend's `app.version` reports the
method. With `ToolMode::PerCommand` every command is also its own tool (dots become `_`) with its
real input schema.

## CLI reply contract

`<app>-cli exec <id> [json]`, `describe`, `commands --json`, `run`, `version --json` print one line:
`{"ok":true,"result":…}` or `{"ok":false,"error":"…"}` (plus `detail`). Exit codes: 0 ok, 1 the
command failed, 2 usage error.
