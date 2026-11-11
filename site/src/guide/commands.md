# Commands

A command is a named action with typed parameters. Every command an app registers shows up in the
menus, the command palette, the keyboard shortcut editor, the app's CLI, its control port and its
MCP server. They all run it through the same execute chokepoint, which validates the parameters,
checks that the command is enabled, catches panics and records undo.

## Writing one

A command body is a plain function from the session and the JSON parameters to a JSON result:

```rust,ignore
fn add(s: &mut Session<Notes>, p: &Value) -> Result<Value, CommandError> {
    let title = p.get("title").and_then(Value::as_str).unwrap_or("Untitled note").to_string();
    let id = s.edit("New Note", |d| d.push(title));
    s.set_view("selected", &id);
    Ok(json!({"id": id, "count": s.doc().notes.len()}))
}
```

Return `CommandError::bad_params(id, msg)` for bad input and `CommandError::failed(id, msg)` when the
command cannot do its work. An empty `id` is filled in with the command's own id.

## Registering it

`CommandSpec::new` and its `const` setters:

```rust,ignore
const ADD: &[Param] = &[Param::string("title"), Param::string("body")];
const ID: &[Param] = &[Param::int_in("id", 1, i64::MAX).about("default: the selected note")];

pub fn commands() -> Vec<CommandSpec<Session<Notes>>> {
    vec![
        CommandSpec::new("note.add", "New Note", add).menu(&["Note"]).shortcut("Mod+Shift+N").params(Params::Static(ADD)),
        CommandSpec::new("note.delete", "Delete Note", delete)
            .menu(&["Note"])
            .shortcut("Mod+Backspace")
            .params(Params::Static(ID))
            .enabled(has_target),
        CommandSpec::new("note.list", "List Notes", list).params(Params::Static(LIST)).query(),
    ]
}
```

or the `cmd!` and `query!` macros, which fit a command on one line:

```rust,ignore
cmd!("note.generate", "Generate Notes…", ["Note"], None, [Param::int_in("count", 1, 1_000_000).about("how many")], always, generate)
query!("note.count", "Count Notes", [], count)
```

`cmd!` takes the id, label, menu path, shortcut, parameters, enabled predicate and body. `query!`
makes a command that only reads: always enabled, not in menus, not recorded in the command
journal. `.query()` does the same for a `CommandSpec`.

Pass the list to the app with `App::commands(…)`, or one at a time with `App::command(…)`.

## Parameters

| Constructor | JSON |
| --- | --- |
| `Param::bool("name")` | boolean |
| `Param::int("name")`, `Param::int_in("name", min, max)` | integer |
| `Param::float("name")`, `Param::float_in("name", min, max)` | number |
| `Param::string("name")` | string |
| `Param::one_of("name", &["a", "b"])` | one of these strings |
| `Param::json("name")` | any JSON value |

`.required()` makes a parameter mandatory and `.about("…")` describes it. The registry turns the
list into a JSON Schema, which the MCP server and `describe` show, and validates every call against
it before the body runs.

## Enabled predicates

An enabled predicate returns `Err(reason)` to disable a command. Menus and the palette grey it out
and show the reason; a call through the CLI or MCP fails with that reason.

```rust,ignore
fn has_target(s: &Session<Notes>) -> Result<(), String> {
    match s.view::<u64>("selected") {
        Some(id) if s.doc().get(id).is_some() => Ok(()),
        _ => Err("no note selected".into()),
    }
}
```

`always` is the predicate of a command that is always available.

## Menus and shortcuts

`.menu(&["Note"])` puts the command in the Note menu; a longer path makes submenus. The built-in
File, Edit and View menus are already there. Menus are drawn in the window; with the
`native-menu` feature they go to the macOS and Windows menu bar.

Shortcuts use one portable syntax: `Mod+Shift+S`, where `Mod` is ⌘ on macOS and Ctrl elsewhere,
and `Ctrl` is the literal Control key. Several shortcuts are separated by ` / `
(`"Mod+Shift+Z / Mod+Y"`). Users can rebind them in the Keyboard Shortcuts editor (View >
Keyboard Shortcuts…, the `app.keymap` command), which also shows conflicts.

## The palette

`Mod+Shift+P` (the `app.palette` command) opens the command palette. It lists the app's
commands with their menu paths and shortcuts and filters them with a fuzzy matcher.

## Running commands from a view

```rust,ignore
if ui.button("New Note").clicked() {
    cx.dispatch("note.add", json!({"title": "Groceries"})); // runs after the view returns; errors become toasts
}
if let Ok(result) = cx.execute("note.list", json!({"limit": 1})) { // runs now, returns the result
    ui.label(result["count"].to_string());
}
```

`dispatch` is the usual choice: the command runs after the frame's view returns, so it never
changes the document while the view is reading it.

## Built-in commands

| Id | Label | Shortcut |
| --- | --- | --- |
| `file.new` | New | `Mod+N` |
| `file.open` | Open… | `Mod+O` |
| `file.save` | Save | `Mod+S` |
| `file.save_as` | Save As… | `Mod+Shift+S` |
| `file.revert` | Revert to Saved | |
| `app.quit` | Quit | `Mod+Q` |
| `app.undo` | Undo | `Mod+Z` |
| `app.redo` | Redo | `Mod+Shift+Z / Mod+Y` |
| `app.palette` | Command Palette… | `Mod+Shift+P` |
| `app.keymap` | Keyboard Shortcuts… | |
| `view.theme` | Switch Theme | |
| `app.document` | Document Info | |

When two commands share an id, the first one registered wins and a warning is logged. The File and
Edit built-ins come before the app's commands and the View ones after, so an app can replace
`view.theme` but not `file.save`.
