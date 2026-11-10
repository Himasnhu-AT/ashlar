#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `DiffView` perf on a headless `egui::Context`: the CPU cost of one egui pass over a diff of
//! about 50k rows, unified and split.
//!
//! `cargo test -p ashlar-ui --release --test perf_diff_view -- --ignored --nocapture`

use std::time::{Duration, Instant};

use ashlar_diff::{DiffOptions, FileDiff};
use ashlar_ui::diff_view::{DiffMode, DiffView, DiffViewState};
use egui::{Event, Rect, pos2, vec2};

const SCREEN: Rect = Rect { min: pos2(0.0, 0.0), max: pos2(1200.0, 800.0) };

fn frame(ctx: &egui::Context, t: f64, events: Vec<Event>, ui: impl FnMut(&mut egui::Ui)) -> Duration {
    let input = egui::RawInput { screen_rect: Some(SCREEN), time: Some(t), events, ..Default::default() };
    let t0 = Instant::now();
    let mut out = ctx.run_ui(input, ui);
    let dt = t0.elapsed();
    out.textures_delta.clear();
    dt
}

fn median_ms(v: &mut [Duration]) -> f64 {
    v.sort_unstable();
    v[v.len() / 2].as_secs_f64() * 1000.0
}

fn wheel(dy: f32) -> Vec<Event> {
    vec![
        Event::PointerMoved(pos2(500.0, 400.0)),
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, dy),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        },
    ]
}

/// Five Rust files of 10k lines, each with a change every 12 lines: about 50k diff rows.
fn files() -> Vec<FileDiff> {
    (0..5)
        .map(|f| {
            let mut old = String::new();
            let mut new = String::new();
            for i in 0..10_000 {
                let line = format!("    let value_{i} = compute({i}, \"item {f}\"); // note\n");
                old.push_str(&line);
                if i % 12 == 5 {
                    new.push_str(&format!("    let value_{i} = compute({}, \"item {f}\"); // changed\n", i + 1));
                } else {
                    new.push_str(&line);
                }
            }
            FileDiff::from_texts_with(&old, &new, DiffOptions { context: 6, ..Default::default() })
                .with_paths(&format!("src/f{f}.rs"), &format!("src/f{f}.rs"))
        })
        .collect()
}

#[test]
#[ignore]
fn perf_diff_view_50k_lines() {
    let files = files();
    let lines: usize = files.iter().flat_map(|f| &f.hunks).map(|h| h.lines.len() + 1).sum();
    assert!(lines >= 50_000, "{lines}");
    for mode in [DiffMode::Unified, DiffMode::Split] {
        let ctx = egui::Context::default();
        let mut state = DiffViewState::new();
        let visible = std::cell::RefCell::new(0..0);
        let mut draw = |ctx: &egui::Context, t: f64, events: Vec<Event>| {
            frame(ctx, t, events, |ui| {
                let r = DiffView::new("perf.diff", &mut state, &files, 1).mode(mode).show(ui);
                *visible.borrow_mut() = r.visible;
            })
        };
        let first = draw(&ctx, 0.0, Vec::new());
        for i in 0..5 {
            draw(&ctx, 0.01 * f64::from(i), Vec::new());
        }
        let mut scroll = Vec::new();
        for i in 0..60 {
            scroll.push(draw(&ctx, 0.1 + f64::from(i) / 60.0, wheel(-120.0)));
        }
        let shown = visible.borrow().clone();
        assert!(shown.start > 100 && shown.len() > 20, "the view did not scroll: {shown:?}");
        let mut idle = Vec::new();
        for i in 0..30 {
            idle.push(draw(&ctx, 2.0 + f64::from(i) / 60.0, Vec::new()));
        }
        let suffix = if mode == DiffMode::Split { "_split" } else { "" };
        println!("bench: diff_50k_lines_first_frame{suffix} {:.3} ms", first.as_secs_f64() * 1000.0);
        println!("bench: diff_50k_lines_frame{suffix} {:.3} ms", median_ms(&mut scroll));
        println!("bench: diff_50k_lines_idle_frame{suffix} {:.3} ms", median_ms(&mut idle));
    }
}
