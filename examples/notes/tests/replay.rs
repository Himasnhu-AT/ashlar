#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A recorded notes session (real pointer and key input plus agent requests) replays into a
//! fresh driver with the same document revisions and the same snapshot tree.

use std::path::Path;

use ashlar::app::{Driver, Runtime};
use ashlar::egui::{Event, Key, Modifiers, PointerButton, pos2};
use ashlar::fs::AppDirs;
use ashlar::serde_json::{Value, json};
use notes::Notes;

fn driver(root: &Path, record: Option<&Path>) -> Driver<Notes> {
    let mut rt: Runtime<Notes> = notes::app().dirs(AppDirs::under(root)).runtime();
    if let Some(path) = record {
        rt.record_to(path).unwrap();
    }
    Driver::new(rt)
}

/// Click the centre of the element `id` with real pointer events.
fn click(d: &mut Driver<Notes>, id: &str) {
    let r = d.call("ui.get", json!({"id": id, "what": "rect"})).unwrap();
    let n = |i: usize| r["value"][i].as_f64().unwrap() as f32;
    let at = pos2(n(0) + n(2) / 2.0, n(1) + n(3) / 2.0);
    d.push_event(Event::PointerMoved(at));
    d.frame();
    for pressed in [true, false] {
        d.push_event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        d.frame();
    }
    d.settle(60);
}

fn undo_labels(d: &Driver<Notes>) -> Vec<String> {
    d.runtime.session().store().history().undo_labels().map(str::to_string).collect()
}

fn snapshot(d: &mut Driver<Notes>) -> Value {
    let mut s = d.call("ui.snapshot", json!({})).unwrap();
    // a fresh generation counter is not part of what was drawn
    if let Some(o) = s.as_object_mut() {
        o.remove("generation");
    }
    s
}

#[test]
fn a_recorded_session_replays_to_the_same_document_and_tree() {
    let dir = tempfile::tempdir().unwrap();
    let rec = dir.path().join("session.jsonl");

    let mut d = driver(&dir.path().join("a"), Some(&rec));
    let start = d.runtime.session().revision().0;
    d.settle(20);
    click(&mut d, "notes.add");
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "from agent"}})).unwrap();
    click(&mut d, "notes.add");
    click(&mut d, "notes.title");
    d.push_event(Event::Key { key: Key::End, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    d.push_event(Event::Text(" typed".into()));
    d.frame();
    d.settle(60);
    d.call("ui.key", json!({"key": "Z", "modifiers": {"command": true}})).unwrap();
    d.call("ui.set", json!({"selected": 0})).unwrap();
    d.settle(60);
    // revisions come from one process-wide counter: compare how far each session moved it
    let revisions = d.runtime.session().revision().0 - start;
    let labels = undo_labels(&d);
    let notes = d.call("engine.execute", json!({"command": "note.list", "params": {}})).unwrap();
    assert!(notes["count"].as_u64().unwrap() >= 3, "the session made notes: {notes}");
    let tree = snapshot(&mut d);
    drop(d);

    let mut r = driver(&dir.path().join("b"), None);
    let start = r.runtime.session().revision().0;
    let frames = r.replay(&rec).unwrap();
    assert!(frames > 10, "{frames} frames recorded");
    r.settle(60);
    assert_eq!(r.runtime.session().revision().0 - start, revisions);
    assert_eq!(undo_labels(&r), labels);
    assert_eq!(r.call("engine.execute", json!({"command": "note.list", "params": {}})).unwrap(), notes);
    assert_eq!(snapshot(&mut r), tree);
}
