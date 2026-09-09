#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The notes app end to end: headless through `Dispatch` (what the CLI and MCP use), and the
//! windowed runtime through the control protocol on a headless event loop.

use std::path::Path;

use ashlar::app::{Driver, Headless, Runtime};
use ashlar::fs::AppDirs;
use ashlar::prelude::*;
use notes::Notes;

fn headless(root: &Path) -> Headless<Notes> {
    notes::app().dirs(AppDirs::under(root)).headless()
}

fn windowed(root: &Path) -> Driver<Notes> {
    let rt: Runtime<Notes> = notes::app().dirs(AppDirs::under(root)).runtime();
    let mut d = Driver::new(rt);
    d.settle(20);
    d
}

fn count(d: &mut impl Dispatch) -> u64 {
    d.execute("note.list", json!({"limit": 1})).unwrap().get("count").and_then(Value::as_u64).unwrap()
}

#[test]
fn add_undo_save_and_reopen_headless() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.notes");
    let mut s = headless(dir.path());
    s.execute("note.add", json!({"title": "first", "body": "hello"})).unwrap();
    s.execute("note.add", json!({"title": "second"})).unwrap();
    assert_eq!(count(&mut s), 2);
    assert_eq!(s.session().store().history().depth(), 2, "one undo step per command");
    s.execute("app.undo", json!({})).unwrap();
    assert_eq!(count(&mut s), 1);
    s.execute("app.redo", json!({})).unwrap();
    s.execute("app.undo", json!({})).unwrap();
    let saved = s.execute("file.save_as", json!({"path": file.to_string_lossy()})).unwrap();
    assert_eq!(saved["dirty"], json!(false));

    let mut t = headless(dir.path());
    t.execute("file.open", json!({"path": file.to_string_lossy()})).unwrap();
    let list = t.execute("note.list", json!({})).unwrap();
    assert_eq!(list["notes"][0]["title"], json!("first"));
    assert_eq!(list["notes"][0]["body"], json!("hello"));
    assert_eq!(list["count"], json!(1));
    assert!(t.execute("app.undo", json!({})).is_err(), "a freshly opened file has no history");
    // without a window, unsaved changes are protected unless discarded explicitly
    t.execute("note.add", json!({})).unwrap();
    let e = t.execute("file.new", json!({})).unwrap_err();
    assert!(e.to_string().contains("unsaved"), "{e}");
    t.execute("file.new", json!({"discard": true})).unwrap();
    assert_eq!(count(&mut t), 0);
}

#[test]
fn a_v1_file_is_migrated() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("old.notes");
    let v1 = json!({
        "format": notes::FORMAT_ID, "schema_version": 1, "generator": "notes 0.0.1",
        "document": {"notes": [{"text": "Shopping\nmilk\neggs"}, {"text": "Ideas"}]}
    });
    std::fs::write(&file, serde_json_string(&v1)).unwrap();
    let mut s = headless(dir.path());
    let opened = s.execute("file.open", json!({"path": file.to_string_lossy()})).unwrap();
    assert_eq!(opened["migrated_from"], json!(1));
    let list = s.execute("note.list", json!({})).unwrap();
    assert_eq!(list["notes"][0]["title"], json!("Shopping"));
    assert_eq!(list["notes"][0]["body"], json!("milk\neggs"));
    assert_eq!(list["notes"][1]["title"], json!("Ideas"));
    // new notes continue the id sequence
    let added = s.execute("note.add", json!({"title": "third"})).unwrap();
    assert_eq!(added["id"], json!(3));
    s.execute("file.save", json!({})).unwrap();
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"schema_version\": 2"), "{text}");
}

fn serde_json_string(v: &Value) -> String {
    ashlar::serde_json::to_string_pretty(v).unwrap()
}

#[test]
fn a_failed_command_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = headless(dir.path());
    s.execute("note.add", json!({"title": "keep"})).unwrap();
    let rev = s.session().revision();
    let depth = s.session().store().history().depth();
    assert!(s.execute("note.edit", json!({"id": 999, "title": "x"})).is_err());
    assert!(s.execute("note.delete", json!({"id": 999})).is_err());
    assert!(s.execute("note.add", json!({"title": 5})).is_err(), "typed params are checked");
    assert!(s.execute("no.such.command", json!({})).is_err());
    assert_eq!(s.session().revision(), rev);
    assert_eq!(s.session().store().history().depth(), depth);
}

#[test]
fn the_window_is_driven_through_the_control_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("w.notes");
    let mut d = windowed(dir.path());

    let v = d.call("app.version", json!({})).unwrap();
    let methods: Vec<String> = ashlar::serde_json::from_value(v["methods"].clone()).unwrap();
    for m in ["ui.inspect", "ui.click", "ui.screenshot", "app.stats", "app.logs"] {
        assert!(methods.iter().any(|x| x == m), "{m} missing from {methods:?}");
    }

    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "from agent"}})).unwrap();
    let ins = d.call("ui.inspect", json!({})).unwrap();
    let ids: Vec<&str> = ins["elements"].as_array().unwrap().iter().filter_map(|e| e["id"].as_str()).collect();
    assert!(ids.contains(&"notes.add"), "{ids:?}");
    assert!(ids.contains(&"notes.list.row.0"), "{ids:?}");
    assert!(ids.contains(&"notes.title"), "the new note is selected and open: {ids:?}");
    assert_eq!(ins["document"]["dirty"], json!(true));
    assert!(ins["title"].as_str().unwrap().contains('•'), "dirty mark in the title: {}", ins["title"]);

    // a real click on the button
    d.call("ui.click", json!({"id": "notes.add"})).unwrap();
    assert_eq!(count(&mut d.runtime_dispatch()), 2);

    // the undo shortcut goes through the keymap
    d.call("ui.key", json!({"key": "Z", "modifiers": {"command": true}})).unwrap();
    assert_eq!(count(&mut d.runtime_dispatch()), 1);

    // menus
    let menu = d.call("ui.menu.list", json!({})).unwrap();
    let text = menu.to_string();
    assert!(text.contains("\"File\"") && text.contains("\"Note\"") && text.contains("app.redo"), "{text}");
    d.call("ui.menu.invoke", json!({"id": "app.redo"})).unwrap();
    assert_eq!(count(&mut d.runtime_dispatch()), 2);

    // typing into the title is one undo step
    d.call("ui.click", json!({"id": "notes.title"})).unwrap();
    d.call("ui.key", json!({"key": "End"})).unwrap();
    d.call("ui.type", json!({"text": " one"})).unwrap();
    d.call("ui.type", json!({"text": " two"})).unwrap();
    let list = d.call("engine.execute", json!({"command": "note.list", "params": {}})).unwrap();
    assert_eq!(list["notes"][1]["title"], json!("Untitled note one two"));
    let depth = d.runtime.session().store().history().depth();
    d.call("engine.execute", json!({"command": "app.undo"})).unwrap();
    let list = d.call("engine.execute", json!({"command": "note.list", "params": {}})).unwrap();
    assert_eq!(list["notes"][1]["title"], json!("Untitled note"));
    assert_eq!(d.runtime.session().store().history().depth(), depth - 1);

    // view state, theme, screenshot, stats, logs
    d.call("ui.set", json!({"theme": "dark", "selected": 1})).unwrap();
    assert_eq!(d.runtime.session().view::<u64>("selected"), Some(1));
    assert!(d.call("ui.set", json!({"theme": "purple"})).is_err());
    let shot = d.call("ui.screenshot", json!({})).unwrap();
    assert!(shot["png"].as_str().unwrap().starts_with("iVBORw0KGgo"), "a PNG");
    assert_eq!(shot["width"], json!(1280));
    let crop = d.call("ui.screenshot", json!({"id": "notes.add"})).unwrap();
    assert!(crop["width"].as_u64().unwrap() < 300);
    let stats = d.call("app.stats", json!({})).unwrap();
    assert!(stats["frames"].as_u64().unwrap() > 10, "{stats}");
    assert!(stats["p95_ms"].as_f64().unwrap() > 0.0, "{stats}");
    let logs = d.call("app.logs", json!({"limit": 5})).unwrap();
    assert!(logs["lines"].is_array());
    assert!(d.call("ui.click", json!({"id": "no.such.element"})).is_err());
    assert!(d.call("ui.bogus", json!({})).is_err());

    // save; the title loses its dirty mark
    d.call("engine.execute", json!({"command": "file.save_as", "params": {"path": file.to_string_lossy()}})).unwrap();
    let ins = d.call("ui.inspect", json!({})).unwrap();
    assert_eq!(ins["document"]["dirty"], json!(false));
    assert_eq!(ins["title"], json!("w.notes — Notes"));

    // quit
    d.call("app.quit", json!({})).unwrap();
    d.settle(5);
    assert!(d.is_closed());
}

#[test]
fn closing_with_unsaved_changes_asks_first() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = windowed(dir.path());
    d.call("engine.execute", json!({"command": "note.add"})).unwrap();
    d.request_close();
    d.settle(10);
    assert!(!d.is_closed(), "unsaved changes keep the window open");
    let ins = d.call("ui.inspect", json!({})).unwrap();
    assert_eq!(ins["dialogs"], json!(1));
    // "Don't Save"
    d.call("ui.click", json!({"id": "ashlar.confirm_discard.button.1"})).unwrap();
    d.settle(5);
    assert!(d.is_closed());
}

#[test]
fn a_crashed_session_is_offered_for_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let rt = notes::app().dirs(AppDirs::under(dir.path())).autosave(Some(std::time::Duration::from_secs(1))).runtime();
    let mut d = Driver::new(rt);
    d.settle(20);
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "unsaved work"}})).unwrap();
    // the autosave timer wakes the loop by itself
    let frames = d.advance(3.0);
    assert!((1..40).contains(&frames), "autosave woke the loop {frames} times");
    d.into_runtime().abandon(); // the process "dies"

    let rt = notes::app().dirs(AppDirs::under(dir.path())).runtime();
    let mut d = Driver::new(rt);
    d.settle(20);
    let ins = d.call("ui.inspect", json!({})).unwrap();
    assert_eq!(ins["dialogs"], json!(1), "{ins}");
    d.call("ui.click", json!({"id": "ashlar.recovery.button.2"})).unwrap();
    let list = d.call("engine.execute", json!({"command": "note.list", "params": {}})).unwrap();
    assert_eq!(list["notes"][0]["title"], json!("unsaved work"));
    assert!(d.runtime.session().is_dirty(), "recovered work is unsaved");
    d.call("engine.execute", json!({"command": "app.undo"})).unwrap();
    assert_eq!(count(&mut d.runtime_dispatch()), 0, "recovery is one undo step");
}

#[test]
fn a_panicking_view_is_contained() {
    let dir = tempfile::tempdir().unwrap();
    let mut armed = true;
    let rt = notes::app()
        .dirs(AppDirs::under(dir.path()))
        .view(move |ui, cx: &mut Cx<'_, Notes>| {
            ui.label(format!("{} notes", cx.doc().notes.len()));
            if armed && !cx.doc().notes.is_empty() {
                armed = false;
                let v: Vec<u8> = Vec::new();
                let _ = v[3]; // an indexing bug in app code
            }
        })
        .runtime();
    let mut d = Driver::new(rt);
    d.settle(20);
    d.call("engine.execute", json!({"command": "note.add"})).unwrap();
    d.settle(5);
    assert!(d.runtime.view_error().is_some());
    let ins = d.call("ui.inspect", json!({})).unwrap();
    assert!(ins["view_error"].is_string());
    // the document survived and commands still work
    assert_eq!(count(&mut d.runtime_dispatch()), 1);
    d.call("ui.click", json!({"id": "ashlar.view_error.button.1"})).unwrap();
    assert!(d.runtime.view_error().is_none(), "Try Again resumes the view");
}

#[test]
fn an_idle_app_draws_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = windowed(dir.path());
    d.call("engine.execute", json!({"command": "note.generate", "params": {"count": 1000}})).unwrap();
    d.call("engine.execute", json!({"command": "file.save_as", "params": {"path": dir.path().join("i.notes").to_string_lossy()}})).unwrap();
    d.call("ui.scroll", json!({"id": "notes.list", "dy": 400.0})).unwrap();
    d.settle(60);
    // pending work finishes by itself (the debounced settings write wakes the loop once)
    let drain = d.advance(2.0);
    assert!(drain <= 2, "{drain} frames to finish pending work");
    let frames = d.advance(5.0);
    println!("bench: idle_repaints_5s {frames} frames");
    assert_eq!(frames, 0, "an idle app must not repaint");
}

/// `Dispatch` over the driver's runtime (commands without going through the protocol).
trait RuntimeDispatch {
    fn runtime_dispatch(&mut self) -> RtDispatch<'_>;
}

struct RtDispatch<'a>(&'a mut Runtime<Notes>);

impl Dispatch for RtDispatch<'_> {
    fn commands(&self) -> Vec<CommandInfo> {
        Vec::new()
    }
    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        self.0.execute(id, params)
    }
}

impl RuntimeDispatch for Driver<Notes> {
    fn runtime_dispatch(&mut self) -> RtDispatch<'_> {
        RtDispatch(&mut self.runtime)
    }
}

#[test]
fn palette_keymap_and_theme_from_the_keyboard_and_menus() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = windowed(dir.path());
    d.call("ui.key", json!({"key": "P", "modifiers": {"command": true, "shift": true}})).unwrap();
    let ins = d.call("ui.inspect", json!({})).unwrap();
    assert_eq!(ins["palette_open"], json!(true));
    d.call("ui.type", json!({"text": "new note"})).unwrap();
    d.call("ui.key", json!({"key": "Enter"})).unwrap();
    assert_eq!(count(&mut d.runtime_dispatch()), 1, "the palette ran note.add");
    let ins = d.call("ui.inspect", json!({})).unwrap();
    assert_eq!(ins["palette_open"], json!(false));

    d.call("ui.menu.invoke", json!({"id": "app.keymap"})).unwrap();
    assert_eq!(d.call("ui.inspect", json!({})).unwrap()["keymap_open"], json!(true));

    d.call("engine.execute", json!({"command": "view.theme", "params": {"mode": "dark"}})).unwrap();
    assert!(d.ctx().global_style().visuals.dark_mode);
    // settings are written after a quiet period, by a timer, not by polling
    d.advance(2.0);
    let settings = std::fs::read_to_string(dir.path().join("config/settings.json")).unwrap();
    assert!(settings.contains("\"theme\": \"dark\""), "{settings}");
}

#[cfg(feature = "telemetry")]
#[test]
fn telemetry_asks_nothing_and_without_an_endpoint_records_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let rt = notes::app().dirs(AppDirs::under(dir.path())).telemetry("test-write-key", None).runtime();
    let mut d = Driver::new(rt);
    d.settle(20);
    // telemetry is not optional: no notice, no dialog
    assert_eq!(d.call("ui.inspect", json!({})).unwrap()["dialogs"], json!(0));
    d.call("engine.execute", json!({"command": "note.add"})).unwrap();
    drop(d);
    // no endpoint: no client, nothing on disk
    fn has_tracer(p: &std::path::Path) -> bool {
        std::fs::read_dir(p).into_iter().flatten().flatten().any(|e| e.file_name() == "tracer" || (e.path().is_dir() && has_tracer(&e.path())))
    }
    assert!(!has_tracer(dir.path()));
}
