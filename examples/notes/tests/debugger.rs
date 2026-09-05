#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The UI debugger end to end on the notes app, through the control protocol on the headless
//! event loop: snapshot, refs, click, fill, press, wait, get, screenshot, the inspector, and that
//! none of it keeps an idle app drawing.

use std::path::Path;

use ashlar::app::{Driver, Runtime};
use ashlar::control::Reply;
use ashlar::fs::AppDirs;
use ashlar::prelude::*;
use notes::Notes;

fn windowed(root: &Path) -> Driver<Notes> {
    let rt: Runtime<Notes> = notes::app().dirs(AppDirs::under(root)).runtime();
    let mut d = Driver::new(rt);
    d.settle(20);
    d
}

fn with_two_notes(root: &Path) -> Driver<Notes> {
    let mut d = windowed(root);
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "first", "body": "hello"}})).unwrap();
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "second"}})).unwrap();
    d.settle(20);
    d
}

fn count(d: &mut Driver<Notes>) -> u64 {
    d.call("engine.execute", json!({"command": "note.list", "params": {"limit": 1}})).unwrap()["count"].as_u64().unwrap()
}

fn titles(d: &mut Driver<Notes>) -> Vec<String> {
    let v = d.call("engine.execute", json!({"command": "note.list"})).unwrap();
    v["notes"].as_array().unwrap().iter().map(|n| n["title"].as_str().unwrap().to_string()).collect()
}

/// The element whose automation id is `id` in a snapshot.
fn element<'a>(snap: &'a Value, id: &str) -> &'a Value {
    snap["elements"].as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no {id} in\n{}", snap["tree"]))
}

fn error(r: Reply) -> (String, Value) {
    match r {
        Reply::Err { error, detail } => (error, detail.unwrap_or(Value::Null)),
        Reply::Ok(v) => panic!("expected an error, got {v}"),
    }
}

#[test]
fn snapshot_lists_the_notes_ui_with_roles_names_and_stable_refs() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = with_two_notes(dir.path());
    let version = d.call("app.version", json!({})).unwrap();
    let methods = version["methods"].as_array().unwrap();
    for m in ["ui.snapshot", "ui.fill", "ui.press", "ui.wait", "ui.get", "ui.highlight", "devtools.toggle", "ui.inspect"] {
        assert!(methods.contains(&json!(m)), "{m} missing from {methods:?}");
    }

    let snap = d.call("ui.snapshot", json!({})).unwrap();
    let tree = snap["tree"].as_str().unwrap();
    for line in [
        "- menu \"File\" [ref=",
        "- button \"New Note\" [ref=",
        "- label \"2 notes\" [ref=",
        "- searchbox \"Search notes\" [ref=",
        "- list [ref=",
        "  - listitem \"first\" [ref=",
        "  - listitem \"second\" [ref=",
        "- textbox \"Title\" [ref=",
        "- textbox \"Body\" [ref=",
    ] {
        assert!(tree.lines().any(|l| l.starts_with(line)), "no `{line}` in\n{tree}");
    }
    assert!(tree.contains("[id=notes.list.row.1] [selected]"), "{tree}");
    assert!(tree.contains("[id=notes.title] [value=\"second\"]"), "{tree}");
    let add = element(&snap, "notes.add");
    assert_eq!((add["role"].as_str(), add["name"].as_str(), add["enabled"].as_bool()), (Some("button"), Some("New Note"), Some(true)));
    let row = element(&snap, "notes.list.row.0");
    assert_eq!(row["parent"], element(&snap, "notes.list")["ref"], "rows sit in their list");
    assert_eq!(element(&snap, "notes.body")["value"], "");

    let interactive = d.call("ui.snapshot", json!({"interactive": true})).unwrap();
    let itree = interactive["tree"].as_str().unwrap();
    assert!(!itree.contains("label"), "{itree}");
    assert!(itree.contains("textbox \"Title\""), "{itree}");
    let scoped = d.call("ui.snapshot", json!({"scope": {"id": "notes.list"}})).unwrap();
    assert_eq!(scoped["elements"].as_array().unwrap().len(), 3, "{}", scoped["tree"]);

    // a new snapshot: same refs, next generation
    let again = d.call("ui.snapshot", json!({})).unwrap();
    assert_eq!(again["generation"].as_u64().unwrap(), snap["generation"].as_u64().unwrap() + 3);
    for id in ["notes.add", "notes.title", "notes.list.row.0"] {
        assert_eq!(element(&again, id)["ref"], element(&snap, id)["ref"], "{id}");
    }
}

#[test]
fn click_by_ref_and_text_fill_submit_and_press() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = with_two_notes(dir.path());
    let snap = d.call("ui.snapshot", json!({})).unwrap();
    let add = element(&snap, "notes.add")["ref"].as_str().unwrap().to_string();

    d.call("ui.click", json!({"target": {"ref": add}})).unwrap();
    assert_eq!(count(&mut d), 3);
    d.call("ui.click", json!({"target": {"text": "New Note", "role": "button"}})).unwrap();
    assert_eq!(count(&mut d), 4);
    d.call("ui.click", json!({"target": {"text": "first"}})).unwrap();
    d.settle(10);
    assert_eq!(d.call("ui.get", json!({"target": {"id": "notes.title"}, "what": "value"})).unwrap()["value"], "first");

    // fill replaces the field's text; the edit reaches the document
    d.call("ui.fill", json!({"target": {"id": "notes.title"}, "text": "renamed"})).unwrap();
    d.settle(10);
    assert_eq!(titles(&mut d)[0], "renamed");
    assert_eq!(d.call("ui.get", json!({"id": "notes.title", "what": "text"})).unwrap()["value"], "renamed");

    // fill + submit in the search field filters the list
    d.call("ui.fill", json!({"target": {"text": "Search notes"}, "text": "renam", "submit": true})).unwrap();
    d.settle(10);
    let snap = d.call("ui.snapshot", json!({"scope": {"id": "notes.list"}})).unwrap();
    let rows: Vec<&str> =
        snap["elements"].as_array().unwrap().iter().filter(|e| e["role"] == "listitem").map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(rows, vec!["renamed"]);
    d.call("ui.fill", json!({"target": {"id": "notes.search"}, "text": ""})).unwrap();
    d.call("ui.press", json!({"keys": "Escape"})).unwrap();

    // a chord runs its command
    d.call("ui.press", json!({"keys": "Mod+Shift+N"})).unwrap();
    assert_eq!(count(&mut d), 5);
    let (e, _) = error(d.request("ui.press", json!({"keys": "Mod+Nonsense"})));
    assert!(e.contains("not a keyboard shortcut"), "{e}");
}

#[test]
fn waits_resolve_on_later_frames_and_time_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = with_two_notes(dir.path());
    // the title shows up once the frames after the edit have run
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "arrives later"}})).unwrap();
    let w = d.call("ui.wait", json!({"for": "text", "text": "arrives later", "timeout_ms": 2000})).unwrap();
    assert!(w["elapsed_ms"].as_u64().is_some());
    d.call("ui.wait", json!({"for": "element", "target": {"id": "notes.list.row.2"}})).unwrap();
    d.call("ui.wait", json!({"for": "idle", "timeout_ms": 2000})).unwrap();

    let before = d.time();
    let (e, detail) = error(d.request("ui.wait", json!({"for": "text", "text": "never shown", "timeout_ms": 300})));
    assert!(e.contains("timed out"), "{e}");
    assert_eq!(detail["kind"], "timeout");
    let waited = d.time() - before;
    assert!((0.3..1.0).contains(&waited), "timed out after {waited} s of virtual time");
    assert!(detail["elapsed_ms"].as_u64().unwrap() >= 300);
}

#[test]
fn stale_refs_are_reported_with_the_generation() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = with_two_notes(dir.path());
    let snap = d.call("ui.snapshot", json!({})).unwrap();
    let second = element(&snap, "notes.list.row.1")["ref"].as_str().unwrap().to_string();
    d.call("engine.execute", json!({"command": "note.delete", "params": {"id": 2}})).unwrap();
    d.settle(10);
    let (e, detail) = error(d.request("ui.click", json!({"target": {"ref": second}})));
    assert!(e.contains("stale ref"), "{e}");
    assert_eq!(detail["kind"], "stale_ref");
    assert_eq!(detail["ref"], second.as_str());
    assert_eq!(detail["generation"], snap["generation"]);
    let (e, detail) = error(d.request("ui.get", json!({"target": {"id": "no.such.thing"}})));
    assert!(e.contains("no element"), "{e}");
    assert_eq!(detail["kind"], "not_found");
}

#[test]
fn screenshots_crop_annotate_and_highlight() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = with_two_notes(dir.path());
    let full = d.call("ui.screenshot", json!({})).unwrap();
    assert_eq!((full["width"].as_u64(), full["height"].as_u64()), (Some(1280), Some(800)));

    let rect = d.call("ui.get", json!({"id": "notes.add", "what": "rect"})).unwrap()["value"].clone();
    let (w, h) = (rect[2].as_f64().unwrap(), rect[3].as_f64().unwrap());
    let shot = d.call("ui.screenshot", json!({"target": {"id": "notes.add"}, "pad": 2, "annotate": true, "highlight": {"id": "notes.add"}})).unwrap();
    assert_eq!(shot["width"].as_u64(), Some((w + 4.0).round() as u64), "{rect}");
    assert_eq!(shot["height"].as_u64(), Some((h + 4.0).round() as u64));
    let notes = shot["annotations"].as_array().unwrap();
    assert!(!notes.is_empty());
    let snap = d.call("ui.snapshot", json!({})).unwrap();
    let add = element(&snap, "notes.add")["ref"].clone();
    let a = notes.iter().find(|a| a["ref"] == add).unwrap();
    assert_eq!((a["rect"][0].as_f64(), a["rect"][1].as_f64()), (Some(2.0), Some(2.0)), "image pixels, relative to the crop");

    let whole = d.call("ui.screenshot", json!({"annotate": true})).unwrap();
    assert!(whole["annotations"].as_array().unwrap().len() >= 8, "{}", whole["annotations"]);
    d.call("ui.highlight", json!({"target": {"text": "second"}, "ms": 200})).unwrap();
}

#[test]
fn the_inspector_opens_and_an_idle_app_draws_nothing_either_way() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = with_two_notes(dir.path());
    // the debugger has been busy…
    d.call("ui.snapshot", json!({})).unwrap();
    d.call("ui.click", json!({"target": {"id": "notes.list.row.0"}})).unwrap();
    d.call("ui.screenshot", json!({"annotate": true})).unwrap();
    d.call("ui.wait", json!({"for": "idle"})).unwrap();
    d.settle(20);
    // …and with the inspector closed, 5 s of idle draw nothing (after the app's own debounced
    // settings write for the selection the click made)
    assert_eq!(d.call("devtools.state", json!({})).unwrap()["open"], false);
    d.advance(2.0);
    let frames = d.advance(5.0);
    assert_eq!(frames, 0, "closed inspector repainted: {:?}", d.runtime.wakes());

    // F12 opens it (the key never reaches the app); its panels come from the runtime
    d.push_event(egui::Event::Key { key: egui::Key::F12, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    d.settle(20);
    let st = d.call("devtools.state", json!({})).unwrap();
    assert_eq!(st["open"], true);
    let snap = d.call("ui.snapshot", json!({})).unwrap();
    assert!(!snap["tree"].as_str().unwrap().contains("Devtools"), "{}", snap["tree"]);
    d.advance(2.0);
    assert_eq!(d.advance(5.0), 0, "an open inspector does not animate either");
    let st = d.call("devtools.toggle", json!({"open": false})).unwrap();
    assert_eq!(st["open"], false);
    d.advance(2.0);
    assert_eq!(d.advance(5.0), 0);
}
