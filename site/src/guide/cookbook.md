# Cookbook

Short recipes for common tasks. Each uses `use ashlar::prelude::*;` and a document type `Doc`
unless it says otherwise.

## A custom file format, saved and opened from code

```rust,ignore
let app = App::<Doc>::new("sketch", env!("CARGO_PKG_VERSION"))
    .format(Format::new("acme.sketch", concat!("Sketch ", env!("CARGO_PKG_VERSION"))).migrations(vec![v1_to_v2]))
    .file_extension("sketch");
```

The built-in File commands now read and write `.sketch` files. To save or open without a dialog
(a script, an agent, a test), pass a path:

```rust,ignore
cx.dispatch("file.save_as", json!({"path": "/tmp/drawing.sketch"}));
cx.dispatch("file.open", json!({"path": "/tmp/drawing.sketch"}));
```

From the shell: `sketch-cli exec file.save_as '{"path": "out.sketch"}'`.

## A long job with progress and a Cancel button

```rust,ignore
use ashlar::ui::status_bar;

if ui.button("Export").clicked() {
    let doc = cx.snapshot();
    cx.jobs.spawn("export", "Exporting", doc, |doc, progress| {
        let total = doc.pages.len() as u64;
        for (i, page) in doc.pages.iter().enumerate() {
            if !progress.report(i as f64 / total.max(1) as f64, format!("Page {} of {total}", i + 1)) {
                return Err("cancelled".into());
            }
            render(page)?;
        }
        Ok(apply(|s: &mut Session<Doc>| {
            s.toast(ToastLevel::Success, "Export finished");
            Ok(Value::Null)
        }))
    });
}
for job in cx.jobs.list() {
    status_bar::progress(ui, "export.progress", &job.label, job.fraction.map(|f| f as f32));
    if job.cancellable && ui.button("Cancel").clicked() {
        cx.jobs.cancel(job.id);
    }
}
```

## A settings page stored in the app settings

```rust,ignore
use ashlar::ui::{controls::Switch, form};

let mut autosave: bool = cx.settings.get_or("autosave", true);
let out = form::section(ui, "settings.general", "General", |s| {
    s.row("autosave", "Autosave")
        .description("Save a recovery copy every minute")
        .bind(&mut autosave, true)
        .control(|ui, id, on| Switch::new(id, "", on).show(ui));
});
if autosave != cx.settings.get_or("autosave", true) {
    let _ = cx.settings.set("autosave", &autosave);
}
```

The row shows a "modified" marker and a reset button when the value differs from the default.

## A sortable table of 100,000 rows

```rust,ignore
use ashlar::ui::virtual_list::{Column, RowHeights, Sort, VirtualTable};

let columns = [Column::new("Name", 220.0), Column::new("Size", 80.0)];
let out = VirtualTable::new("files", &columns, files.len(), RowHeights::Uniform(22.0))
    .selected(selected)
    .sort(Some(sort))
    .show(ui, |ui, row, col| {
        let f = &files[row];
        match col {
            0 => { ui.label(&f.name); }
            _ => { ui.label(format!("{} KB", f.size / 1024)); }
        }
    });
if let Some(col) = out.header_clicked {
    sort = Sort { column: col, ascending: !(sort.column == col && sort.ascending) };
    // re-sort `files` by `sort`
}
if let Some(row) = out.list.select {
    selected = Some(row);
}
```

Only the visible rows are laid out, so the cost per frame does not grow with the row count.

## A context menu that runs commands

```rust,ignore
use ashlar::ui::commands::{Item, MenuNode, MenuSpec, Submenu, menu};

let spec = MenuSpec::new(vec![Submenu::new("Note", vec![
    MenuNode::Item(Item::new("note.rename", "Rename…")),
    MenuNode::Item(Item::new("note.delete", "Delete").accelerator("Mod+Backspace")),
])]);
let row = ui.label(&note.title);
if let Some(id) = menu::context(&row, &spec) {
    cx.dispatch(&id, json!({"id": note.id}));
}
```

A spec with one menu lists its items directly; with several, each becomes a submenu. Build the
spec once and keep it, rather than every frame.

## A toast with an Undo button

```rust,ignore
use ashlar::ui::toasts::Toast;

// on the App: .run_toast_actions(true)
cx.toasts.push(Toast::info("Note deleted").action("Undo", "app.undo"));
```

## An undoable drag on a numeric field

```rust,ignore
use ashlar::ui::edit::EditPhase;
use ashlar::ui::value_field::{Decimal, ValueField};

let mut opacity = cx.doc().opacity;
let out = ValueField::new("inspector.opacity", "Opacity", &mut opacity, &Decimal { decimals: 0, suffix: " %" })
    .range(0.0..=100.0)
    .show(ui);
if let Some(edit) = out.edit
    && edit.phase != EditPhase::Begin
{
    // Update and Commit carry the new value; Cancel (Escape) carries the value before the drag
    cx.edit_merged("Opacity", "inspector.opacity", |d| d.opacity = edit.value);
}
```

All the edits of one drag share a label and merge key, so the drag is one undo step called
"Opacity".

## A golden screenshot test of a scene

```rust,ignore
// tests/snapshots.rs
use ashlar::app::{Driver, Renderer};
use ashlar::fs::AppDirs;

#[test]
fn the_empty_scene_looks_as_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let rt = my_app::app().dirs(AppDirs::under(dir.path())).runtime();
    let mut d = Driver::with_renderer(rt, Renderer::wgpu());
    d.set_screen(640.0, 400.0);
    d.pixels_per_point(1.0);
    d.scene("empty").unwrap();
    d.settle(60);
    d.assert_snapshot("empty").unwrap();
}
```

Record the baseline once with `ASHLAR_UPDATE_SNAPSHOTS=1 cargo test`, then commit
`tests/snapshots/empty.png` and its `.meta.json`.

## A picture of the app for a bug report or a PR

```sh
cargo run -- --headless --scene empty --size 1024x700 --snap app.png --tree tree.json
```
