#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Golden snapshots of the notes app's scenes (`tests/snapshots`).
//!
//! The baselines were rendered on the machine named in their `.meta.json`; on another adapter the
//! comparison is skipped with a note. `ASHLAR_UPDATE_SNAPSHOTS=1` records them again.

use ashlar::app::{Driver, Renderer};
use ashlar::fs::AppDirs;
use ashlar_debug::SnapshotOutcome;

fn driver(dir: &std::path::Path) -> Driver<notes::Notes> {
    let rt = notes::app().dirs(AppDirs::under(dir)).runtime();
    let mut d = Driver::with_renderer(rt, Renderer::wgpu());
    d.set_screen(640.0, 400.0);
    d.pixels_per_point(1.0);
    d.settle(60);
    d
}

fn passed(name: &str, out: Result<SnapshotOutcome, String>) {
    match out {
        Ok(o) => eprintln!("{name}: {}", o.text()),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn the_empty_and_two_note_scenes_look_as_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(dir.path());
    d.scene("empty").unwrap();
    passed("notes-empty", d.assert_snapshot("notes-empty"));
    d.scene("two-notes").unwrap();
    d.advance(1.0);
    d.settle(60);
    passed("notes-two", d.assert_snapshot("notes-two"));
}

#[test]
fn a_ui_script_asserts_a_screenshot() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = driver(dir.path());
    d.scene("two-notes").unwrap();
    d.advance(1.0);
    d.settle(60);
    // the script step compares with the same baseline as the Driver call
    match d.run_ui("assert screenshot notes-two\n") {
        Ok(_) => {}
        Err(e) => panic!("{e}"),
    }
}
