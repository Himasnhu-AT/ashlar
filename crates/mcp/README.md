# ashlar-mcp

A stdio MCP server (JSON-RPC 2.0, hand-rolled, no async runtime) generated from an app's commands.
Requests are handled one at a time, in order; a reader thread answers `ping` and takes
cancellations while one runs.

- **Protocol:** `initialize` with version negotiation (2025-06-18, 2025-03-26, 2024-11-05; anything
  else gets the newest), `ping` (answered even while a call runs), `tools/list`, `tools/call`,
  `resources/list` / `resources/read` (`app://info`, `app://commands`, and `app://ui/snapshot` when
  the backend draws a UI), `notifications/cancelled` (a cancelled request gets no response; a
  stepwise batch stops before its next step), `notifications/progress` for a `batch` called with a
  `progressToken` (one per step), notifications, legacy batches (empty
  batch is an error, all-notification batches get no reply), strict `jsonrpc`/`id`/`params`
  validation, bounded message size. Only protocol is written to stdout.
- **Tools:** `list_commands`, `describe_command`, `execute`, `batch`, plus `screenshot` and `ui_*`
  when the backend's `app.version` lists the method. The debugger tools (`ui_snapshot` → an outline
  with refs, then `ui_click`/`ui_fill`/`ui_hover`/`ui_focus`/`ui_press`/`ui_get`/`ui_wait`/
  `ui_scroll_into_view`/`ui_highlight` on `"@e3"`, and `screenshot` with `annotate`/`highlight`)
  take targets as ref strings or objects; see `docs/debugging-uis.md`. `ToolMode::PerCommand` adds one tool per
  command with its real input schema. `McpServer::tool` adds the app's own tools: a handler gets the arguments and a `ToolCx` (call the
  backend, report progress, see a cancellation) and returns a result now or a `PendingCall` the
  server polls about every 100 ms while it answers other requests (a long export as a job). A
  pending call that is cancelled gets `PendingCall::cancel` to clean up and no response.
  `McpServer::on_execute` lets the app take over `execute` for some commands (e.g. run one as a
  pending job), `McpServer::resource` adds the app's own resources, and
  `ServerOptions::strict_arguments(true)` refuses arguments a tool's schema does not list. Every tool carries
  hints: read-only (and idempotent), undoable (a command that changes the document: not
  destructive), or anything (`execute`, `batch`, UI input: destructive).
- **Backends:** `Headless(Box<dyn Dispatch>)` in-process; `Remote(ashlar_control::Client)` for a
  running app (screenshots, input); `Subprocess` for a child serving the control protocol on stdio
  (crash isolation: a command that aborts kills the child, the next call starts a fresh one; calls
  time out).

```rust
use ashlar_mcp::{Backend, McpServer, ServerOptions};

let opts = ServerOptions::new("notes", env!("CARGO_PKG_VERSION")).instructions("A note-taking app. …");
McpServer::new(Backend::Headless(Box::new(session)), opts).serve(std::io::BufReader::new(std::io::stdin()), std::io::stdout())?;
```

Tests: golden transcripts in `tests/golden/*.jsonl` (`UPDATE_GOLDEN=1` rewrites them), all three
backends end to end in `tests/conformance.rs`; ping, cancellation, progress, resources, hints and
custom tools in `tests/concurrency.rs`. Method and tool vocabulary:
[`docs/control-protocol.md`](../../docs/control-protocol.md).
