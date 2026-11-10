#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The frame scheduler end to end: pending work (a settings write, an autosave) asks for its own
//! frame when it is due, so an idle app with unsaved changes and changed settings draws nothing,
//! saves still happen on time, and `app.stats` says why each frame ran.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ashlar::app::wake::{Reason, WakeStats};
use ashlar::app::{Driver, Runtime};
use ashlar::egui;
use ashlar::fs::AppDirs;
use ashlar::prelude::*;
use notes::Notes;

fn windowed(root: &Path, autosave: Duration) -> Driver<Notes> {
    let rt: Runtime<Notes> = notes::app().dirs(AppDirs::under(root)).autosave(Some(autosave)).runtime();
    let mut d = Driver::new(rt);
    d.settle(20);
    d
}

/// Autosave slots written so far.
fn slots(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for session in std::fs::read_dir(root.join("data/Recovery")).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(session.path()).into_iter().flatten().flatten() {
            if f.file_name().to_string_lossy().starts_with("slot-") {
                out.push(f.path());
            }
        }
    }
    out
}

fn settings_file(root: &Path) -> String {
    std::fs::read_to_string(root.join("config/settings.json")).unwrap_or_default()
}

#[test]
fn idle_with_unsaved_changes_and_changed_settings_draws_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = windowed(dir.path(), Duration::from_secs(60));
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "unsaved"}})).unwrap();
    d.call("ui.set", json!({"wake.test": 7})).unwrap();
    assert!(d.runtime.session().is_dirty());
    assert!(d.runtime.settings().is_dirty(), "the view setting waits for the debounce");
    d.settle(20); // the frames the edit and the control requests asked for
    assert!(d.runtime.settings().is_dirty(), "the view setting waits for the debounce");
    let before = d.runtime.wakes();

    // the debounced write asks for one frame of its own
    let drain = d.advance(1.0);
    assert_eq!(drain, 1, "{drain} frames to write the settings: {:?}", d.runtime.wakes());
    assert!(!d.runtime.settings().is_dirty());
    assert!(settings_file(dir.path()).contains("wake.test"), "{}", settings_file(dir.path()));
    let after = d.runtime.wakes();
    assert!(after.frames_for(Reason::SETTINGS) > before.frames_for(Reason::SETTINGS), "{after:?}");

    let frames = d.advance(5.0);
    assert_eq!(frames, 0, "5 s of dirty idle repainted: {:?}", d.runtime.wakes());
    assert!(d.runtime.session().is_dirty(), "the document still has unsaved changes");
    assert!(slots(dir.path()).is_empty(), "the 60 s autosave is not due yet");
}

#[test]
fn autosave_writes_on_time_without_polling() {
    let dir = tempfile::tempdir().unwrap();
    // the autosave clock starts at the first frame (t = 0): the first write is due at t = 2
    let mut d = windowed(dir.path(), Duration::from_secs(2));
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "unsaved"}})).unwrap();
    d.advance(1.9 - d.time());
    assert!(slots(dir.path()).is_empty(), "written before the interval");
    assert_eq!(d.runtime.wakes().frames_for(Reason::AUTOSAVE), 0, "{:?}", d.runtime.wakes());

    // the autosave's wake hook asked for the frame at t = 2, which hands the write to the writer
    d.advance(0.2);
    assert!(d.runtime.wakes().frames_for(Reason::AUTOSAVE) >= 1, "{:?}", d.runtime.wakes());
    let waited = std::time::Instant::now();
    while slots(dir.path()).is_empty() && waited.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(slots(dir.path()).len(), 1, "the autosave was written");
    // the writer wakes the loop once the write finished, to collect the result
    let autosave_frames = d.runtime.wakes().frames_for(Reason::AUTOSAVE);
    let waited = std::time::Instant::now();
    while d.runtime.wakes().frames_for(Reason::AUTOSAVE) == autosave_frames && waited.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(5));
        d.advance(0.01);
    }
    assert!(d.runtime.wakes().frames_for(Reason::AUTOSAVE) > autosave_frames, "{:?}", d.runtime.wakes());

    // written: nothing is pending, the dirty app stays idle
    let frames = d.advance(5.0);
    assert_eq!(frames, 0, "5 s of dirty idle after the autosave repainted: {:?}", d.runtime.wakes());
    assert!(d.runtime.session().is_dirty());
    assert_eq!(slots(dir.path()).len(), 1);
}

#[test]
fn app_stats_reports_frames_and_requests_per_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = windowed(dir.path(), Duration::from_secs(60));
    d.call("engine.execute", json!({"command": "note.add"})).unwrap();
    let v = d.call("app.stats", json!({})).unwrap();
    let w = &v["wakes"];
    assert!(w["frames"].as_u64().unwrap() >= 1, "{v}");
    assert!(w["frames_by"].is_object() && w["requests_by"].is_object(), "{v}");
    // the control requests asked for frames, and got them
    assert!(w["requests_by"]["control"].as_u64().unwrap_or(0) >= 1, "{w}");
    assert!(w["frames_by"]["control"].as_u64().unwrap_or(0) >= 1, "{w}");
    // the edit left an autosave and a settings write (the selection) pending: both asked
    assert!(w["requests_by"]["autosave"].as_u64().unwrap_or(0) >= 1, "{w}");
    assert!(w["requests_by"]["settings"].as_u64().unwrap_or(0) >= 1, "{w}");
    // the first frame was asked for by no one: input
    assert!(w["frames_by"]["input"].as_u64().unwrap_or(0) >= 1, "{w}");
    // every frame is credited to at least one reason
    let credited: u64 = w["frames_by"].as_object().unwrap().values().filter_map(Value::as_u64).sum();
    assert!(credited >= w["frames"].as_u64().unwrap(), "{w}");
    // the same numbers as the typed API, and they round-trip
    let typed: WakeStats = ashlar::serde_json::from_value(w.clone()).unwrap();
    assert!(typed.frames <= d.runtime.wakes().frames);
    assert!(typed.requests() >= 2);
}

/// A note is open and its title field has the keyboard (clicked, as a person would), then the
/// app settles: returns the driver.
fn focused_title(root: &Path) -> Driver<Notes> {
    let mut d = windowed(root, Duration::from_secs(60));
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "focus me"}})).unwrap();
    let id = d.call("engine.execute", json!({"command": "note.list", "params": {}})).unwrap()["notes"][0]["id"].as_u64().unwrap();
    d.call("engine.execute", json!({"command": "note.select", "params": {"id": id}})).unwrap();
    d.settle(20);
    let ins = d.call("ui.inspect", json!({})).unwrap();
    let title = ins["elements"].as_array().unwrap().iter().find(|e| e["id"] == "notes.title").unwrap_or_else(|| panic!("no title field: {ins}"));
    let r: Vec<f32> = title["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
    let at = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.push_event(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        d.push_event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        d.advance(0.05);
    }
    d.settle(20);
    assert!(d.ctx().egui_wants_keyboard_input(), "the title field has the keyboard");
    d
}

/// Frames a focused, untouched text field costs over 30 s of virtual time.
fn idle_focused_frames(d: &mut Driver<Notes>) -> u64 {
    d.advance(30.0)
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_idle_frames_focused_field() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = focused_title(dir.path());
    let before = d.runtime.wakes();
    let frames = idle_focused_frames(&mut d);
    println!("bench: idle_frames_focused_field_30s {frames} frames");
    let after = d.runtime.wakes();
    println!("wakes over the 30 s: before {before:?} after {after:?}");
}

/// Mod+Shift+N pressed or released, as the keyboard sends it on this platform.
fn new_note_chord(pressed: bool) -> egui::Event {
    let modifiers = if cfg!(target_os = "macos") {
        egui::Modifiers { shift: true, mac_cmd: true, command: true, ..Default::default() }
    } else {
        egui::Modifiers { shift: true, ctrl: true, command: true, ..Default::default() }
    };
    egui::Event::Key { key: egui::Key::N, physical_key: None, pressed, repeat: false, modifiers }
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_frames_per_command() {
    const RUNS: u64 = 100;
    let dir = tempfile::tempdir().unwrap();
    let mut d = windowed(dir.path(), Duration::from_secs(3600));
    // through the engine (the control port), one command at a time, each settled
    let start = d.frames();
    for i in 0..RUNS {
        d.call("engine.execute", json!({"command": "note.add", "params": {"title": format!("n{i}")}})).unwrap();
        d.settle(20);
    }
    let per = (d.frames() - start) as f64 / RUNS as f64;
    println!("bench: frames_per_command {per:.2} frames");
    // from the keyboard, the way a person runs one
    let notes = d.runtime.session().doc().notes.len();
    let start = d.frames();
    for _ in 0..RUNS {
        d.push_event(new_note_chord(true));
        d.push_event(new_note_chord(false));
        d.settle(20);
    }
    assert_eq!(d.runtime.session().doc().notes.len(), notes + RUNS as usize, "each chord ran note.add");
    let per = (d.frames() - start) as f64 / RUNS as f64;
    println!("bench: frames_per_key_command {per:.2} frames");
}
