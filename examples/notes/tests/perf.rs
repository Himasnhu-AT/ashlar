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

#[path = "../../../crates/ui/tests/support/allocs.rs"]
mod allocs;

/// Counts allocations for the `*allocs*` metrics (this test binary only).
#[global_allocator]
static GLOBAL: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

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
    d.pixels_per_point(2.0); // a Retina display
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
    println!("bench: pass_p95_scroll_100k {:.3} ms", s.pass.p95_ms);
    println!("bench: tessellate_p95_scroll_100k {:.3} ms", s.tessellate.p95_ms);
    println!("bench: frame_cpu_p95_scroll_100k_x2 {:.3} ms", s.frame_cpu.p95_ms);
    assert!(s.window >= 600, "every scroll step drew a frame ({} frames)", s.window);
    if !cfg!(debug_assertions) {
        assert!(s.p95_ms < 4.0, "frame p95 budget is 4 ms (got {:.3})", s.p95_ms);
        assert!(s.frame_cpu.p95_ms < 8.3, "the whole frame at 2x fits 120 Hz: 8.3 ms (got {:.3})", s.frame_cpu.p95_ms);
    }
}

// ---------------------------------------------------------------------------------------------
// Interaction benches (IMP-1.5). Frames are timed in thread CPU time around the driver call, so
// they include egui's end of pass (not tessellation: the headless driver has no renderer).
// ---------------------------------------------------------------------------------------------

/// A runtime with `count` generated notes, settled (and the temp dir its files live in).
fn with_notes(count: u64) -> (Driver<Notes>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let rt: Runtime<Notes> = notes::app().dirs(AppDirs::under(dir.path())).autosave(None).runtime();
    let mut d = Driver::new(rt);
    d.settle(10);
    d.call("engine.execute", json!({"command": "note.generate", "params": {"count": count}})).unwrap();
    d.settle(20);
    (d, dir)
}

fn p95(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    let i = ((v.len() as f64 - 1.0) * 0.95).round() as usize;
    v.get(i).copied().unwrap_or(f64::NAN)
}

/// Thread CPU ms of `f`.
fn cpu_ms(f: impl FnOnce()) -> f64 {
    let t = cpu_time::ThreadTime::now();
    f();
    t.elapsed().as_secs_f64() * 1000.0
}

/// Type `text` one character per keystroke, running every frame each keystroke asks for; returns
/// the thread CPU ms of each keystroke.
fn type_chars(d: &mut Driver<Notes>, text: &str) -> Vec<f64> {
    text.chars()
        .map(|c| {
            d.push_event(egui::Event::Text(c.to_string()));
            cpu_ms(|| {
                d.settle(10);
            })
        })
        .collect()
}

fn note_text(d: &mut Driver<Notes>, id: u64) -> (String, String) {
    let list = d.call("engine.execute", json!({"command": "note.list", "params": {"limit": 10_000}})).unwrap();
    let n = list["notes"].as_array().unwrap().iter().find(|n| n["id"] == json!(id)).cloned().unwrap_or_default();
    (n["title"].as_str().unwrap_or_default().to_string(), n["body"].as_str().unwrap_or_default().to_string())
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_select_last_of_100k() {
    let (mut d, _dir) = with_notes(100_000);
    d.call("engine.execute", json!({"command": "note.select", "params": {"id": 100_000}})).unwrap();
    d.settle(20);
    d.runtime.stats_mut().reset_window();
    let over_list = egui::pos2(140.0, 400.0);
    let mut cpu = Vec::with_capacity(600);
    for i in 0..600 {
        d.push_event(egui::Event::PointerMoved(over_list));
        let dy = if (i / 150) % 2 == 0 { 120.0 } else { -120.0 };
        d.push_event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        cpu.push(cpu_ms(|| {
            d.advance(1.0 / 60.0);
        }));
    }
    let s = d.runtime.stats().summary();
    println!("bench: frame_p95_scroll_sel_last {:.3} ms", s.p95_ms);
    println!("bench: frame_cpu_p95_scroll_sel_last {:.3} ms", p95(cpu));
    assert!(s.window >= 600, "every scroll step drew a frame ({} frames)", s.window);
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_typing_with_query_100k() {
    let (mut d, _dir) = with_notes(100_000);
    // a query that keeps about a tenth of the notes
    d.call("ui.click", json!({"id": "notes.search"})).unwrap();
    d.call("ui.type", json!({"text": "note 9"})).unwrap();
    d.call("engine.execute", json!({"command": "note.select", "params": {"id": 9}})).unwrap();
    d.settle(20);
    d.call("ui.click", json!({"id": "notes.body"})).unwrap();
    d.call("ui.key", json!({"key": "End"})).unwrap();
    d.settle(20);
    let typed = " and some more words typed";
    let ks = type_chars(&mut d, typed);
    println!("bench: keystroke_p95_100k_query {:.3} ms", p95(ks));
    let (_, body) = note_text(&mut d, 9);
    assert!(body.ends_with(typed), "the keystrokes reached the note ({body:?})");
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_type_50kb_note() {
    let (mut d, _dir) = with_notes(1_000);
    let line = "The quick brown fox jumps over the lazy dog, again and again.\n";
    let body = line.repeat(50 * 1024 / line.len());
    let id = d.call("engine.execute", json!({"command": "note.add", "params": {"title": "big", "body": body}})).unwrap()["id"].as_u64().unwrap();
    d.settle(20);
    d.call("ui.click", json!({"id": "notes.title"})).unwrap();
    d.call("ui.key", json!({"key": "End"})).unwrap();
    d.settle(20);
    let typed = " with a longer title";
    let ks = type_chars(&mut d, typed);
    println!("bench: type_50kb_frame_p95 {:.3} ms", p95(ks));
    let (title, b) = note_text(&mut d, id);
    assert_eq!(title, format!("big{typed}"));
    assert_eq!(b.len(), body.len());
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_live_resize() {
    let (mut d, _dir) = with_notes(100_000);
    d.call("engine.execute", json!({"command": "note.select", "params": {"id": 100_000}})).unwrap();
    d.set_screen(900.0, 700.0);
    d.settle(20);
    let widths = (600..=900).rev().chain(601..=900);
    let mut cpu = Vec::with_capacity(600);
    for w in widths {
        d.set_screen(w as f32, 700.0);
        cpu.push(cpu_ms(|| {
            d.advance(1.0 / 120.0);
        }));
    }
    println!("bench: resize_frame_p95 {:.3} ms", p95(cpu));
}

// ---------------------------------------------------------------------------------------------
// Allocation gate (IMP-1.2). The counts are process-wide, so run with --test-threads=1.
// ---------------------------------------------------------------------------------------------

/// Allocations a frame woken only by a timer (the caret blink of a focused field, nothing else
/// changing) may make. Measured at 550 on 2026-11-10 after IMP-2.16 (954 before; a bare egui frame
/// with a focused field makes 29; most of the rest is inside egui's own widgets: about 7 per list
/// row, 17 per button, the text field, panels and tessellation), plus 5%. Lower it as IMP-2.x
/// removes per-frame work.
const IDLE_WAKE_FRAME_ALLOCS: usize = 578;

/// Wheel `steps` times over the list, 120 pt per step, reversing every 150 steps.
fn scroll_list(d: &mut Driver<Notes>, steps: u32) {
    let over_list = egui::pos2(140.0, 400.0);
    for i in 0..steps {
        d.push_event(egui::Event::PointerMoved(over_list));
        let dy = if (i / 150) % 2 == 0 { -120.0 } else { 120.0 };
        d.push_event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        });
        d.advance(1.0 / 60.0);
    }
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_allocs_scroll_100k() {
    let (mut d, _dir) = with_notes(100_000);
    d.pixels_per_point(2.0);
    d.call("engine.execute", json!({"command": "note.select", "params": {"id": 50}})).unwrap();
    d.settle(20);
    scroll_list(&mut d, 300); // warm up: down and back
    let frames0 = d.frames();
    let ((), a) = allocs::count(|| scroll_list(&mut d, 600));
    let frames = usize::try_from(d.frames() - frames0).unwrap();
    assert!(frames >= 600, "every scroll step drew a frame ({frames} frames)");
    println!("bench: allocs_per_frame_scroll_100k {} n", a.count / frames);
    println!("bench: bytes_per_frame_scroll_100k {} B", a.bytes / frames);
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_allocs_idle_wake_frame() {
    let (mut d, _dir) = with_notes(100_000);
    // a focused field: its caret blink is the only thing that wakes the loop
    d.call("ui.click", json!({"id": "notes.search"})).unwrap();
    d.settle(20);
    assert!(d.ctx().egui_wants_keyboard_input(), "the search field has the keyboard");
    d.advance(2.0); // warm up
    let mut per_frame = Vec::new();
    for _ in 0..200 {
        let (frames, a) = allocs::count(|| d.advance(0.05));
        if frames == 1 {
            per_frame.push(a.count);
        }
    }
    assert!(per_frame.len() >= 5, "the caret woke the loop ({} single-frame steps in 10 s)", per_frame.len());
    let max = per_frame.iter().copied().max().unwrap_or(0);
    let mean = per_frame.iter().sum::<usize>() as f64 / per_frame.len() as f64;
    println!("bench: allocs_per_idle_wake_frame {max} n");
    println!("idle wake frames: {} measured, mean {mean:.1} allocs, max {max}", per_frame.len());
    assert!(max <= IDLE_WAKE_FRAME_ALLOCS, "a frame woken only by a timer allocated {max} times (gate {IDLE_WAKE_FRAME_ALLOCS})");
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_allocs_typing_with_query_100k() {
    let (mut d, _dir) = with_notes(100_000);
    d.call("ui.click", json!({"id": "notes.search"})).unwrap();
    d.call("ui.type", json!({"text": "note 9"})).unwrap();
    d.call("engine.execute", json!({"command": "note.select", "params": {"id": 9}})).unwrap();
    d.settle(20);
    d.call("ui.click", json!({"id": "notes.body"})).unwrap();
    d.call("ui.key", json!({"key": "End"})).unwrap();
    d.settle(20);
    let typed = " and some more words typed";
    let mut counts = Vec::new();
    let mut bytes = 0;
    for c in typed.chars() {
        d.push_event(egui::Event::Text(c.to_string()));
        let (_, a) = allocs::count(|| d.settle(10));
        counts.push(a.count);
        bytes += a.bytes;
    }
    let n = counts.len();
    counts.sort_unstable();
    println!("bench: keystroke_allocs {} n", counts.iter().sum::<usize>() / n);
    println!("bench: keystroke_alloc_bytes {} B", bytes / n);
    println!("keystroke allocs: min {} max {}", counts[0], counts[n - 1]);
    let (_, body) = note_text(&mut d, 9);
    assert!(body.ends_with(typed), "the keystrokes reached the note ({body:?})");
}
