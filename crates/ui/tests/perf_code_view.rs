#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `CodeView` perf on a headless `egui::Context`: the CPU cost of one egui pass.
//!
//! `cargo test -p ashlar-ui --release --test perf_code_view -- --ignored --nocapture`

use std::time::{Duration, Instant};

use ashlar_ui::code_view::CodeView;
use egui::{Event, Rect, pos2, vec2};

const SCREEN: Rect = Rect { min: pos2(0.0, 0.0), max: pos2(1000.0, 800.0) };

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
        Event::PointerMoved(pos2(400.0, 400.0)),
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, dy),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        },
    ]
}

fn source(lines: usize) -> String {
    const UNIT: &[&str] = &[
        "/// Documented item.",
        "#[derive(Debug, Clone)]",
        "pub struct Item<'a> { name: &'a str, count: u32 }",
        "impl<'a> Item<'a> {",
        "    pub fn bump(&mut self) -> u32 { self.count = self.count.saturating_add(1); self.count } // note",
        "    /* block */ fn raw() -> &'static str { r#\"raw \"text\"\"# }",
        "}",
        "",
    ];
    let mut s = String::new();
    for i in 0..lines {
        s.push_str(UNIT[i % UNIT.len()]);
        s.push('\n');
    }
    s
}

#[test]
#[ignore]
fn perf_code_view_100k_lines() {
    let text = source(100_000);
    for wrap in [false, true] {
        let ctx = egui::Context::default();
        let visible = std::cell::RefCell::new(0..0);
        let draw = |ctx: &egui::Context, t: f64, events: Vec<Event>| {
            frame(ctx, t, events, |ui| {
                let r = CodeView::new("perf.code", &text).lang(Some("rust")).revision(1).wrap(wrap).show(ui);
                *visible.borrow_mut() = r.visible;
            })
        };
        let first = draw(&ctx, 0.0, Vec::new());
        for i in 0..5 {
            draw(&ctx, 0.01 * f64::from(i), Vec::new());
        }
        // steady scrolling: new lines enter each frame, the rest are cache hits
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
        let suffix = if wrap { "_wrap" } else { "" };
        println!("bench: code_view_100k_lines_first_frame{suffix} {:.3} ms", first.as_secs_f64() * 1000.0);
        println!("bench: code_view_100k_lines_frame{suffix} {:.3} ms", median_ms(&mut scroll));
        println!("bench: code_view_100k_lines_idle_frame{suffix} {:.3} ms", median_ms(&mut idle));
    }
}
