#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The notes app's named scenes: `--list-scenes`, `--scene` and `Driver::scene`.

use std::process::Command;

use ashlar::app::Driver;
use ashlar::fs::AppDirs;
use serde_json::json;

fn driver(dir: &std::path::Path) -> Driver<notes::Notes> {
    let mut d = Driver::new(notes::app().dirs(AppDirs::under(dir)).runtime());
    d.settle(60);
    d
}

#[test]
fn a_scene_applied_through_the_driver_changes_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(dir.path());
    let tree = |d: &mut Driver<notes::Notes>| d.call("ui.snapshot", json!({})).unwrap()["tree"].as_str().unwrap_or("").to_string();
    assert!(!tree(&mut d).contains("Groceries"));
    d.scene("two-notes").unwrap();
    let after = tree(&mut d);
    assert!(after.contains("Groceries") && after.contains("Ideas"), "{after}");
    assert_eq!(d.call("ui.scene", json!({})).unwrap(), json!({"scenes": ["empty", "two-notes"]}));
    let e = d.scene("nope").unwrap_err();
    assert!(e.contains("no scene `nope`") && e.contains("two-notes"), "{e}");
    let methods = d.call("app.version", json!({})).unwrap();
    assert!(methods.to_string().contains("ui.scene"), "{methods}");
}

#[test]
fn list_scenes_and_scene_flags() {
    let out = Command::new(env!("CARGO_BIN_EXE_notes")).arg("--list-scenes").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "empty\ntwo-notes\n");
    let dir = tempfile::tempdir().unwrap();
    let tree = dir.path().join("tree.json");
    let out = Command::new(env!("CARGO_BIN_EXE_notes")).args(["--headless", "--scene", "two-notes", "--tree"]).arg(&tree).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(std::fs::read_to_string(&tree).unwrap().contains("Groceries"));
    let out = Command::new(env!("CARGO_BIN_EXE_notes")).args(["--headless", "--scene", "nope"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no scene `nope`"));
}
