#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Performance gates for the runtime, measured on the notes app (headless, no GPU: the cost of
//! producing a frame, which is what the runtime controls).
//!
//! `cargo test -p ashlar-example-notes --release --test perf -- --ignored --nocapture --test-threads=1`

use std::time::Instant;

use ashlar::app::{Driver, Runtime};
use ashlar::egui;
use ashlar::fs::AppDirs;
use ashlar::prelude::*;
use notes::Notes;

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_first_frame() {
    let dir = tempfile::tempdir().unwrap();
    let t0 = Instant::now();
    let rt: Runtime<Notes> = notes::app().dirs(AppDirs::under(dir.path())).runtime();
    let mut d = Driver::new(rt);
    d.frame();
    let wall = t0.elapsed().as_secs_f64() * 1000.0;
    let first = d.runtime.stats().summary().first_frame_ms.unwrap();
    println!("bench: notes_first_frame {first:.2} ms");
    println!("bench: notes_first_frame_wall {wall:.2} ms");
    if !cfg!(debug_assertions) {
        assert!(first < 300.0, "cold start budget is 300 ms to first frame");
    }
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_frame_p95_scrolling_100k_notes() {
    let dir = tempfile::tempdir().unwrap();
    let rt: Runtime<Notes> = notes::app().dirs(AppDirs::under(dir.path())).autosave(None).runtime();
    let mut d = Driver::new(rt);
    d.settle(10);
    let t = Instant::now();
    d.call("engine.execute", json!({"command": "note.generate", "params": {"count": 100_000}})).unwrap();
    println!("bench: generate_100k_notes {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
    let t = Instant::now();
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "one more"}})).unwrap();
    println!("bench: command_on_100k_notes {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
    d.call("engine.execute", json!({"command": "note.select", "params": {"id": 50}})).unwrap();
    d.settle(20);
    d.runtime.stats_mut().reset_window();
    let over_list = egui::pos2(140.0, 400.0);
    let frames0 = d.frames();
    let wall = Instant::now();
    for i in 0..600 {
        d.push_event(egui::Event::PointerMoved(over_list));
        let dy = if (i / 150) % 2 == 0 { -120.0 } else { 120.0 };
        d.push_event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        d.advance(1.0 / 60.0);
        if i == 149 {
            let ins = d.call("ui.inspect", json!({})).unwrap();
            let deepest = ins["elements"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|e| e["id"].as_str()?.strip_prefix("notes.list.row.")?.parse::<u64>().ok())
                .max()
                .unwrap();
            assert!(deepest > 500, "the list scrolled (deepest visible row {deepest})");
        }
    }
    let per_frame = wall.elapsed().as_secs_f64() * 1000.0 / (d.frames() - frames0) as f64;
    println!("bench: frame_wall_incl_egui_end_pass_scroll_100k {per_frame:.3} ms");
    let s = d.runtime.stats().summary();
    println!("bench: frame_p50_scroll_100k {:.3} ms", s.p50_ms);
    println!("bench: frame_p95_scroll_100k {:.3} ms", s.p95_ms);
    println!("bench: frame_max_scroll_100k {:.3} ms", s.max_ms);
    assert!(s.window >= 600, "every scroll step drew a frame ({} frames)", s.window);
    if !cfg!(debug_assertions) {
        assert!(s.p95_ms < 4.0, "frame p95 budget is 4 ms (got {:.3})", s.p95_ms);
    }
}
