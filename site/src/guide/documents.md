# Documents and undo

## The session

A command runs on a `Session<D>`, and a view reaches the same session through `Cx`. The session
holds the document store, the file it came from and view state.

| Call | What it does |
| --- | --- |
| `s.doc()` | The current document, `&D` |
| `s.snapshot()` | The current document as an `Arc<D>`, to keep or send to a job |
| `s.revision()` | The `Revision` of the current state; key caches by it |
| `s.is_dirty()` | Unsaved changes since the last save |
| `s.edit(label, \|d\| …)` | Change the document as one undo step called `label` |
| `s.try_edit(label, \|d\| …)` | The same, but `f` returns a `Result`; on `Err` nothing changes |
| `s.edit_merged(label, key, \|d\| …)` | Fold this edit into the previous one with the same label and key |
| `s.undo()` / `s.redo()` | Step through the history; returns the label |
| `s.can_undo()` / `s.can_redo()` | Whether there is a step |

Edits are copy-on-write: the store clones the document once per edit, so keep large parts behind
`Arc` and use `Arc::make_mut` for the part you change. The `notes` example keeps each note as
`Arc<Note>`, so editing one note copies one note, not all of them.

```rust,ignore
fn delete(s: &mut Session<Notes>, p: &Value) -> Result<Value, CommandError> {
    let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| CommandError::bad_params("", "`id` is required"))?;
    s.try_edit("Delete Note", |d| {
        let i = d.index_of(id).ok_or_else(|| CommandError::failed("", format!("no note {id}")))?;
        d.notes.remove(i);
        Ok(json!({"deleted": id}))
    })
}
```

## Gestures as one step

Typing into a field or dragging a slider produces many edits. `edit_merged` folds them into one
undo step as long as the label and merge key stay the same:

```rust,ignore
if response.changed() {
    cx.edit_merged("Edit Note", &format!("note-{id}"), |d| d.set_title(id, &title));
}
```

## View state

View state is data that is not in the document and not undoable: the selection, a scroll target,
an open panel. It is JSON keyed by name, and agents can set it with the `ui.set` control method.

```rust,ignore
s.set_view("selected", &id);
let selected: Option<u64> = s.view("selected");
let zoom = s.view_or("zoom", 1.0_f64);
```

## File formats

A `Format` describes how documents are stored: a JSON envelope with the format id, the program that
wrote it, a schema version and the document.

```rust,ignore
fn v1_to_v2(mut file: Value) -> Result<Value, String> {
    // rewrite file["document"] from schema 1 to schema 2
    Ok(file)
}

let format = Format::<Notes>::new("acme.notes", concat!("Notes ", env!("CARGO_PKG_VERSION")))
    .migrations(vec![v1_to_v2]);
```

- The schema version is the number of migrations plus one. Add a migration whenever the document
  shape changes; older files are migrated on open.
- Files from a newer version, or of another format, are refused with a `FormatError`
  (`TooNew`, `WrongFormat`) instead of being misread.
- `.max_bytes(n)` caps the size of a file that will be read; `.pretty(true)` writes indented JSON.
- Saves are atomic: a crash during a save leaves the old file or the new one, never a mix.

## Saving and opening

The built-in commands handle files: `file.new`, `file.open`, `file.save`, `file.save_as`,
`file.revert` and `app.quit`. They ask before discarding unsaved changes, and agents can pass
`{"path": "…"}` to `file.open` and `file.save_as` to skip the dialog.

From code, the session has `s.open(path)`, `s.save()`, `s.save_as(path)`, `s.new_document()`,
`s.path()` and `s.display_name()`.

## Autosave and recovery

Autosave is on by default, every 60 seconds while the document has unsaved changes. Snapshots go
to a `Recovery` folder in the app's data folder. When the app ends normally they are removed. When
it crashes or is killed, the next launch finds them and shows a dialog that offers to recover the
work.

```rust,ignore
App::<Doc>::new("jot", "0.1.0")
    .autosave(Some(std::time::Duration::from_secs(30))) // or None to turn it off
```

## Asking the window for something

A command has no window. It asks for one with an `Effect`, which a headless session (CLI, MCP)
ignores:

```rust,ignore
s.toast(ToastLevel::Success, "Exported");
s.request(Effect::OpenPalette);
s.request(Effect::PickSaveAs);
```

The effects are `Toast`, `OpenPalette`, `OpenKeymap`, `Theme`, `PickOpen`, `PickSaveAs`,
`ConfirmDiscard` and `Quit`.

## Using the store on its own

The store is the `ashlar-store` crate. It is headless and builds for wasm32, so a CLI tool or a
server can use `Store`, `History`, `Format` and `Autosave` without the UI. See
[docs.rs/ashlar-store](https://docs.rs/ashlar-store).
