#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The plain-list scroll benchmark: the one workload measured the same way as the reference
//! framework's published number.
//!
//! 1,000 rows of 32 pt (a text label, a hover fill, clickable) in a 760×800 pt viewport at 2 pixels
//! per point; 120 warm-up frames, then 480 wheel steps of 12 pt at 120 Hz with the pointer inside,
//! reversing direction halfway. Each frame is timed in thread CPU time: the egui pass plus
//! `tessellate`. Variants: a plain `ScrollArea` holding 1k and 10k selectable labels (and 1k
//! hand-painted rows, the floor, and 1k `ashlar_ui::list::Row`s), and `VirtualList` with 1k and
//! 100k rows. `perf_plain_scroll_breakdown` splits a selectable-label row into its parts.
//!
//! `cargo test -p ashlar-example-notes --release --test scroll_bench -- --ignored --nocapture --test-threads=1`

use ashlar::egui::{self, Color32, Event, MouseWheelUnit, Pos2, RawInput, Rect, Sense, TouchPhase, ViewportId, pos2, vec2};
use ashlar::ui::list::Row;
use ashlar::ui::virtual_list::{RowHeights, VirtualList};
use cpu_time::ThreadTime;

const ROW: f32 = 32.0;
const WIDTH: f32 = 760.0;
const HEIGHT: f32 = 800.0;
const PPP: f32 = 2.0;
const WARMUP: usize = 120;
const STEPS: usize = 480;
const STEP_PT: f32 = 12.0;
const DT: f64 = 1.0 / 120.0;

/// Runs the workload over `ui_fn` and returns per-frame thread CPU times in ms, sorted. `ui_fn`
/// returns how far down the list is scrolled, in points; the run checks it really scrolled.
fn run(mut ui_fn: impl FnMut(&mut egui::Ui) -> f32) -> Vec<f64> {
    let ctx = egui::Context::default();
    let pointer = pos2(WIDTH / 2.0, HEIGHT / 2.0);
    let mut time = 0.0;
    let mut samples = Vec::with_capacity(STEPS);
    let mut deepest = 0.0_f32;
    let mut last = 0.0_f32;
    for i in 0..WARMUP + STEPS {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(WIDTH, HEIGHT))),
            time: Some(time),
            focused: true,
            ..Default::default()
        };
        raw.viewports.entry(ViewportId::ROOT).or_default().native_pixels_per_point = Some(PPP);
        raw.events.push(Event::PointerMoved(pointer));
        if i >= WARMUP {
            let step = i - WARMUP;
            // down for the first half, back up for the second
            let dy = if step < STEPS / 2 { -STEP_PT } else { STEP_PT };
            raw.events.push(Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta: vec2(0.0, dy),
                phase: TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
        }
        let t = ThreadTime::now();
        let out = ctx.run_ui(raw, |ui| last = ui_fn(ui));
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        std::hint::black_box(prims);
        assert!((out.pixels_per_point - PPP).abs() < f32::EPSILON, "ran at {} pixels per point", out.pixels_per_point);
        if i >= WARMUP {
            samples.push(ms);
        }
        deepest = deepest.max(last);
        time += DT;
    }
    // 240 steps of 12 pt down is 2,880 pt; smooth scrolling may lag a little behind
    assert!(deepest > 2_000.0, "the list scrolled down (deepest offset {deepest} pt)");
    assert!(last < deepest / 2.0, "and back up (final offset {last} pt)");
    samples.sort_by(f64::total_cmp);
    samples
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    let i = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted.get(i).copied().unwrap_or(f64::NAN)
}

fn labels(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("Row {i}: a line of text in a plain list")).collect()
}

/// A plain `ScrollArea` that builds every row each frame, as an app would write it: one
/// full-width selectable label per row (text, hover fill, clickable).
fn plain(n: usize) -> Vec<f64> {
    let labels = labels(n);
    run(|ui| {
        let out = egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let size = vec2(ui.available_width(), ROW);
            for (i, label) in labels.iter().enumerate() {
                if ui.add_sized(size, egui::Button::selectable(false, label.as_str())).clicked() {
                    std::hint::black_box(i);
                }
            }
        });
        out.state.offset.y
    })
}

/// The floor for a plain list: each row is an allocated rect with a click sense, a hover fill and
/// text painted straight onto the painter (no widget).
fn plain_painted(n: usize) -> Vec<f64> {
    let labels = labels(n);
    let hover = Color32::from_gray(60);
    run(|ui| {
        let out = egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let w = ui.available_width();
            for (i, label) in labels.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(vec2(w, ROW), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, 0.0, hover);
                }
                if resp.clicked() {
                    std::hint::black_box(i);
                }
                let color = ui.visuals().text_color();
                ui.painter().text(rect.left_center() + vec2(8.0, 0.0), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(14.0), color);
            }
        });
        out.state.offset.y
    })
}

/// `n` rows in a plain `ScrollArea`, each built by `row(ui, index, label, size)`.
fn rows(n: usize, mut row: impl FnMut(&mut egui::Ui, usize, &str, egui::Vec2)) -> Vec<f64> {
    let labels = labels(n);
    run(|ui| {
        let out = egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let size = vec2(ui.available_width(), ROW);
            for (i, label) in labels.iter().enumerate() {
                row(ui, i, label, size);
            }
        });
        out.state.offset.y
    })
}

/// The plain list with `ashlar_ui::list::Row`: one allocation, one interact, and text laid out
/// (from the galley cache) only for visible rows.
fn plain_rows_widget(n: usize) -> Vec<f64> {
    rows(n, |ui, i, label, _| {
        if ui.add(Row::new(i, label).height(ROW)).clicked() {
            std::hint::black_box(i);
        }
    })
}

/// The same rows through `VirtualList` (selection, hover and click are its own).
fn virtual_list(n: usize) -> Vec<f64> {
    let labels = labels(n);
    run(|ui| {
        let mut first = usize::MAX;
        VirtualList::new("bench.list", n, RowHeights::Uniform(ROW)).show(ui, |ui, row| {
            first = first.min(row.index);
            ui.label(labels.get(row.index).map_or("", String::as_str));
        });
        // the first row built (overscan included), as points
        if first == usize::MAX { 0.0 } else { first as f32 * ROW }
    })
}

fn report(name: &str, s: &[f64]) {
    println!("bench: {name}_p50 {:.3} ms", pct(s, 0.50));
    println!("bench: {name}_p95 {:.3} ms", pct(s, 0.95));
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_plain_scroll() {
    let s1k = plain(1_000);
    report("plain_scroll_1k", &s1k);
    let s10k = plain(10_000);
    report("plain_scroll_10k", &s10k);
    report("plain_scroll_1k_painted", &plain_painted(1_000));
    report("plain_scroll_1k_rows_widget", &plain_rows_widget(1_000));
    report("plain_scroll_10k_rows_widget", &plain_rows_widget(10_000));
    let v1k = virtual_list(1_000);
    report("virtual_scroll_1k", &v1k);
    let v100k = virtual_list(100_000);
    report("virtual_scroll_100k", &v100k);
    if !cfg!(debug_assertions) {
        // virtualized cost does not grow with the row count
        assert!(pct(&v100k, 0.5) < pct(&v1k, 0.5) * 3.0 + 0.05, "VirtualList is flat in rows");
    }
}

/// Where a selectable-label row's time goes (IMP-2.8): each variant drops one part of it.
#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_plain_scroll_breakdown() {
    let n = 1_000;
    // `Button::selectable` without `add_sized`'s child `Ui`
    let s = rows(n, |ui, _, label, size| {
        let _ = ui.add(egui::Button::selectable(false, label).min_size(size));
    });
    report("breakdown_selectable_no_child_ui", &s);
    // `add_sized`'s child `Ui` around a bare allocation
    let s = rows(n, |ui, _, _, size| {
        let _ = ui.add_sized(size, |ui: &mut egui::Ui| ui.allocate_exact_size(size, Sense::click()).1);
    });
    report("breakdown_child_ui_alloc_interact", &s);
    // one allocation and one interact, nothing painted
    let s = rows(n, |ui, _, _, size| {
        let _ = ui.allocate_exact_size(size, Sense::click());
    });
    report("breakdown_alloc_interact", &s);
    // the space only: no widget registered
    let s = rows(n, |ui, _, _, size| {
        let _ = ui.allocate_space(size);
    });
    report("breakdown_alloc_only", &s);
    // allocation, interact and text laid out for every row (through egui's own galley cache)
    let s = rows(n, |ui, _, label, size| {
        let _ = ui.allocate_exact_size(size, Sense::click());
        let galley = ui.painter().layout_no_wrap(label.to_owned(), egui::FontId::proportional(14.0), Color32::WHITE);
        std::hint::black_box(galley);
    });
    report("breakdown_alloc_interact_layout_all", &s);
}
