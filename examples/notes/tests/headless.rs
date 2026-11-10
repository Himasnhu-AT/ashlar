#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `notes --headless`: the real binary with no window writes a PNG of the requested size and the
//! UI tree, after a `.ui` script.

use std::process::Command;

/// Width and height from a PNG's IHDR chunk.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(bytes.get(..8), Some(&b"\x89PNG\r\n\x1a\n"[..]), "not a PNG");
    let be = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn headless_writes_a_snapshot_and_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let (snap, tree, script) = (dir.path().join("out.png"), dir.path().join("tree.json"), dir.path().join("add.ui"));
    std::fs::write(&script, "exec note.add '{\"title\": \"headless\"}'\nsleep 500\nassert exists id=notes.add\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_notes"))
        .args(["--headless", "--size", "640x400", "--scale", "1", "--theme", "dark", "--at", "0.5"])
        .arg("--script")
        .arg(&script)
        .arg("--snap")
        .arg(&snap)
        .arg("--tree")
        .arg(&tree)
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert_eq!(png_size(&std::fs::read(&snap).unwrap()), (640, 400), "{err}");
    let tree = std::fs::read_to_string(&tree).unwrap();
    assert!(tree.contains("notes.add"), "{tree}");
    serde_json::from_str::<serde_json::Value>(&tree).unwrap();
}

#[test]
fn headless_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| Command::new(env!("CARGO_BIN_EXE_notes")).arg("--headless").args(args).output().unwrap().status.code();
    assert_eq!(run(&["--size", "huge"]), Some(2));
    let failing = dir.path().join("fail.ui");
    std::fs::write(&failing, "assert exists id=no.such.thing\n").unwrap();
    assert_eq!(run(&["--script", failing.to_str().unwrap()]), Some(1));
}
