//! Files from outside the app: dropped on the window, or forwarded by a second launch
//! (`App::single_instance`). Both open through `file.open`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ashlar_app::{App, Driver, Forwarded};
use serde_json::json;

/// A saved document holding `value`, at `dir/name`.
fn saved(dir: &Path, name: &str, value: i64) -> PathBuf {
    let path = dir.join(name);
    let mut h = App::<i64>::new("opentest", "0.0.0").headless();
    h.session_mut().edit("set", |n| *n = value);
    h.session_mut().save_as(&path).expect("save");
    path
}

/// A driver for an app with the `num` extension whose view records what it is handed.
fn driver(seen: Rc<RefCell<Vec<PathBuf>>>) -> Driver<i64> {
    let app = App::<i64>::new("opentest", "0.0.0").file_extension("num").view(move |ui, cx| {
        seen.borrow_mut().extend(cx.dropped_files().iter().cloned());
        ui.label(cx.doc().to_string());
    });
    let mut d = Driver::new(app.runtime());
    d.frame();
    d
}

fn shows_confirm(d: &mut Driver<i64>) -> bool {
    let tree = d.call("ui.inspect", json!({})).expect("ui.inspect");
    tree.to_string().contains("Save changes")
}

#[test]
fn a_dropped_document_opens() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = saved(dir.path(), "a.num", 7);
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut d = driver(Rc::clone(&seen));
    d.drop_files([path.clone()]);
    d.frame();
    assert_eq!(*d.runtime.session().doc(), 7);
    assert_eq!(d.runtime.session().path(), Some(path.as_path()));
    assert!(seen.borrow().is_empty(), "an opened document is not handed to the view");
}

#[test]
fn a_drop_on_a_dirty_document_asks_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = saved(dir.path(), "a.num", 7);
    let mut d = driver(Rc::default());
    d.runtime.session_mut().edit("bump", |n| *n += 1);
    d.frame();
    d.drop_files([path]);
    d.frame();
    d.frame();
    assert_eq!(*d.runtime.session().doc(), 1, "the unsaved edit is kept until the user answers");
    assert!(shows_confirm(&mut d));
}

#[test]
fn other_files_go_to_the_view() {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = saved(dir.path(), "b.num", 3);
    let image = dir.path().join("photo.png");
    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut d = driver(Rc::clone(&seen));
    d.drop_files([image.clone(), doc]);
    d.frame();
    assert_eq!(*d.runtime.session().doc(), 3);
    assert_eq!(*seen.borrow(), vec![image]);
    d.frame();
    assert_eq!(seen.borrow().len(), 1, "dropped files last one frame");
}

#[test]
fn drops_can_be_turned_off() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = saved(dir.path(), "a.num", 7);
    let app = App::<i64>::new("opentest", "0.0.0").file_extension("num").accept_drops(false);
    let mut d = Driver::new(app.runtime());
    d.frame();
    d.drop_files([path]);
    d.frame();
    assert_eq!(*d.runtime.session().doc(), 0);
}

#[test]
fn a_forwarded_launch_opens_its_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    saved(dir.path(), "c.num", 9);
    let mut d = driver(Rc::default());
    d.forward_launch(Forwarded { cwd: dir.path().to_path_buf(), args: vec!["--safe-mode".into(), "c.num".into()] });
    d.frame();
    assert_eq!(*d.runtime.session().doc(), 9);
    assert_eq!(d.runtime.session().path(), Some(dir.path().join("c.num").as_path()));
}

#[test]
fn a_forwarded_launch_on_a_dirty_document_asks_first() {
    let dir = tempfile::tempdir().expect("tempdir");
    saved(dir.path(), "c.num", 9);
    let mut d = driver(Rc::default());
    d.runtime.session_mut().edit("bump", |n| *n += 1);
    d.frame();
    d.forward_launch(Forwarded { cwd: dir.path().to_path_buf(), args: vec!["c.num".into()] });
    d.frame();
    d.frame();
    assert_eq!(*d.runtime.session().doc(), 1);
    assert!(shows_confirm(&mut d));
}
