# ashlar-mcp

A stdio MCP server (JSON-RPC 2.0, hand-rolled, synchronous, no async runtime) generated from an
app's commands.

- **Protocol:** `initialize` with version negotiation (2025-06-18, 2025-03-26, 2024-11-05; anything
  else gets the newest), `ping`, `tools/list`, `tools/call`, notifications, legacy batches (empty
  batch is an error, all-notification batches get no reply), strict `jsonrpc`/`id`/`params`
  validation, bounded message size. Only protocol is written to stdout.
- **Tools:** `list_commands`, `describe_command`, `execute`, `batch`, plus `screenshot` and `ui_*`
  when the backend's `app.version` lists the method. The debugger tools (`ui_snapshot` → an outline
  with refs, then `ui_click`/`ui_fill`/`ui_hover`/`ui_focus`/`ui_press`/`ui_get`/`ui_wait`/
  `ui_scroll_into_view`/`ui_highlight` on `"@e3"`, and `screenshot` with `annotate`/`highlight`)
  take targets as ref strings or objects; see `docs/debugging-uis.md`. `ToolMode::PerCommand` adds one tool per
  command with its real input schema.
- **Backends:** `Headless(Box<dyn Dispatch>)` in-process; `Remote(ashlar_control::Client)` for a
  running app (screenshots, input); `Subprocess` for a child serving the control protocol on stdio
  (crash isolation: a command that aborts kills the child, the next call starts a fresh one; calls
  time out).

```rust
use ashlar_mcp::{Backend, McpServer, ServerOptions};

let opts = ServerOptions::new("notes", env!("CARGO_PKG_VERSION")).instructions("A note-taking app. …");
McpServer::new(Backend::Headless(Box::new(session)), opts).serve(std::io::stdin().lock(), std::io::stdout().lock())?;
```

Tests: golden transcripts in `tests/golden/*.jsonl` (`UPDATE_GOLDEN=1` rewrites them), all three
backends end to end in `tests/conformance.rs`. Method and tool vocabulary:
[`docs/control-protocol.md`](../../docs/control-protocol.md).
