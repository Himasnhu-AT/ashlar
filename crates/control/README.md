# ashlar-control

The agent control port of an ashlar app: a hardened loopback JSON-lines server, a client that
implements `Dispatch` for a running app, and the standard method set (`app.*`, `engine.*`) answered
from any `Dispatch`. `ui.*` methods go to an app-provided `UiHandler`. Protocol:
[`docs/control-protocol.md`](../../docs/control-protocol.md).

```rust
use ashlar_control::{AppInfo, Config, NoUi, Server, handle};

let (server, requests) = Server::start(Config::new(7981, settings_dir), move || ctx.request_repaint())?;
// each frame, on the app thread:
while let Ok(req) = requests.try_recv() {
    handle(req, &mut session, &mut ui_handler, &AppInfo::new("notes", env!("CARGO_PKG_VERSION")));
}
// a client (another process):
let mut app = ashlar_control::Client::connect(7981, &settings_dir)?;
app.execute("edit.undo", serde_json::json!({}))?;
```

Hardening (no options to turn it off): 127.0.0.1 only, a 256-bit key in a 0600 file required on
every request, lines that are not JSON request objects close the connection (so a browser's
cross-origin POST cannot run anything), bounded lines, a connection cap, a first-request deadline,
a reply timeout, and no panics on hostile input. Tests: `tests/server.rs`.

`serve_stream` serves the same protocol on a private pipe (no key), for child processes.
