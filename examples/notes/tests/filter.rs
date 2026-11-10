#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The note list's filter stays right while the selected note is edited under a query (the view
//! re-checks only the edited note instead of filtering every note again).

use ashlar::app::{Driver, Runtime};
use ashlar::fs::AppDirs;
use ashlar::prelude::*;
use notes::Notes;

/// The ids of the rows the list shows. `ui.inspect` answers from the last frame drawn, and an edit
/// made by typing shows in the list from the frame after it, so let that frame run first.
fn rows(d: &mut Driver<Notes>) -> Vec<String> {
    d.advance(0.1);
    let ins = d.call("ui.inspect", json!({})).unwrap();
    ins["elements"].as_array().unwrap().iter().filter_map(|e| Some(e["id"].as_str()?.strip_prefix("notes.list.row.")?.to_string())).collect()
}

fn key(d: &mut Driver<Notes>, k: &str) {
    d.call("ui.key", json!({"key": k})).unwrap();
}

#[test]
fn editing_a_note_moves_it_in_and_out_of_the_filter() {
    let dir = tempfile::tempdir().unwrap();
    let rt: Runtime<Notes> = notes::app().dirs(AppDirs::under(dir.path())).autosave(None).runtime();
    let mut d = Driver::new(rt);
    // tall enough that the virtual list shows (and registers) every row
    d.set_screen(900.0, 1600.0);
    d.settle(10);
    d.call("engine.execute", json!({"command": "note.generate", "params": {"count": 40}})).unwrap();
    d.settle(10);
    assert_eq!(rows(&mut d).len(), 40);

    d.call("ui.click", json!({"id": "notes.search"})).unwrap();
    d.call("ui.type", json!({"text": "NOTE 3"})).unwrap();
    // "Note 3" and "Note 30" … "Note 39"
    assert_eq!(rows(&mut d).len(), 11);

    d.call("engine.execute", json!({"command": "note.select", "params": {"id": 3}})).unwrap();
    d.call("ui.click", json!({"id": "notes.title"})).unwrap();
    key(&mut d, "End");
    // "Note 3" → "Note ": no longer a match
    key(&mut d, "Backspace");
    assert_eq!(rows(&mut d).len(), 10, "the edited note left the filter");
    d.call("ui.type", json!({"text": "3"})).unwrap();
    assert_eq!(rows(&mut d).len(), 11, "and came back");
    d.call("ui.type", json!({"text": "x"})).unwrap();
    assert_eq!(rows(&mut d).len(), 11, "still a match");

    // the same answer as filtering from scratch: undo the typing (a document change the view did
    // not make) and redo it
    d.call("engine.execute", json!({"command": "app.undo"})).unwrap();
    assert_eq!(rows(&mut d).len(), 11);
    let list = d.call("engine.execute", json!({"command": "note.list", "params": {"query": "note 3"}})).unwrap();
    assert_eq!(list["notes"].as_array().unwrap().len(), 11);
    d.call("engine.execute", json!({"command": "app.redo"})).unwrap();
    let list = d.call("engine.execute", json!({"command": "note.list", "params": {"query": "note 3"}})).unwrap();
    assert_eq!(list["notes"].as_array().unwrap().len(), 11);
    assert_eq!(rows(&mut d).len(), 11);
}
