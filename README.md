# ashlar

Build fast desktop and web applications in Rust. One dependency gives an app documents with undo
and autosave, commands that drive menus, shortcuts, a palette, a CLI and an MCP server, background
jobs, native OS features, a component library on egui, and crash safety, with zero CPU when idle.

> Status: pre-0.1, under construction. See [AGENTS.md](AGENTS.md).

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

`App::cli()` turns the same definition into a `-cli` binary with `commands`, `exec`, `run`,
`mcp` and `serve`; `--control <port>` lets an agent drive the window. See `examples/notes`.

## Telemetry

Crash, performance and usage telemetry with a server you host yourself: `ashlar-trace` (the
client SDK), `ashlar-trace-server` (ingest, SQLite storage and a dashboard in one binary),
`ashlar-trace-proto` (the wire format), and `ashlar-telemetry`, which wires the client into an app
in a few lines (command chokepoint, logging, frame loop, native dialogs). See
[docs/trace](docs/trace/README.md).
