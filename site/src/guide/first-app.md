# Your first app

This is the example from the README: a list of notes with one command. It is compiled as a doc
test, so it stays correct.

```sh
cargo new jot && cd jot
cargo add ashlar-rs
cargo add serde --features derive
```

```rust,ignore
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

```sh
cargo run
```

## What each part does

**The document.** `Doc` is any type that is `Clone + Default + Serialize + Deserialize + Send +
Sync + 'static`; such types implement `Document` automatically. The session keeps it as an
`Arc<Doc>` and gives each state a `Revision`.

**The command.** `add` runs on the `Session<Doc>`. `s.edit(label, f)` changes the document and
records one undo step named "Add Note". The command reads its parameters from a JSON `Value` and
returns a JSON result, which is what the CLI, the control port and MCP see.

**`cmd!`** registers it: id `note.add`, label "Add Note", in the Edit menu, on `Mod+Shift+N` (`Mod`
is ⌘ on macOS and Ctrl elsewhere), with one optional string parameter `text`, always enabled. See
[Commands](commands.md).

**`Format`** names the file format (`example.jot`) and the program that writes it (`jot 0.1`).
Without one the app gets a default format called `<name>.document`. Either way the app has File >
New, Open, Save, Save As and Revert, and autosave with recovery. Setting a format (or a file
extension) also turns on opening files dropped on the window. See
[Documents and undo](documents.md).

**The view** is a closure that draws the UI each frame with egui. `Cx` gives it the document, and
`cx.dispatch` runs a command after the view returns. See [Views and components](views.md).

**`run()`** opens the window. It also adds:

- Undo and Redo (`Mod+Z`, `Mod+Shift+Z`), and the File and View menus.
- The command palette (`Mod+Shift+P`) and a keyboard shortcut editor.
- Autosave every 60 seconds and a recovery dialog after a crash.
- A crash guard: a panic in the view shows an error instead of closing the window.
- `--control <port>` for agents, and `--headless` for tests (see [Testing apps](testing.md)).

## Smaller still

The smallest app has an empty document and no commands:

```rust,ignore
use ashlar::prelude::*;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Doc {}

fn main() -> Result<(), ashlar::app::Error> {
    let mut on = false;
    App::<Doc>::new("hello", env!("CARGO_PKG_VERSION"))
        .view(move |ui, _cx: &mut Cx<'_, Doc>| {
            ui.heading("Hello, ashlar");
            ui.checkbox(&mut on, "Switch");
        })
        .run()
}
```

## More builder options

| Method | What it sets |
| --- | --- |
| `.title("Notes")` | The window title (default: the app name) |
| `.organization("com", "Acme")` | The reverse-domain prefix and organisation used for the app's folders |
| `.file_extension("notes")` | The extension of the app's documents (open dialog, drops) |
| `.window_size(960.0, 640.0)` | The first window size |
| `.viewport(\|b\| b.with_min_inner_size([480.0, 320.0]))` | Any egui `ViewportBuilder` option |
| `.icon(include_bytes!("icon.png"))` | The window and taskbar icon (PNG bytes) |
| `.setup(\|ctx\| { … })` | Runs once with the egui context before the first frame (fonts) |
| `.autosave(Some(Duration::from_secs(30)))` | Autosave interval; `None` turns autosave and recovery off |
| `.remember_window(true)` | Restore the window's position and size (default on) |
| `.follow_system_accent(true)` | Take the accent colour from the OS (default on) |
| `.instructions("…")` | What MCP clients are told about the app |
| `.scene("empty", \|s\| { … })` | A named starting state for tests and agents |

The full list is on [docs.rs](https://docs.rs/ashlar-app/latest/ashlar_app/struct.App.html).
