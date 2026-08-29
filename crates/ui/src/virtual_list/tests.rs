use std::cell::Cell;
use std::time::Instant;

use super::*;
use crate::test_support::{click, run};
use egui_kittest::Harness;

const SCREEN: egui::Vec2 = egui::vec2(500.0, 400.0);

fn input() -> egui::RawInput {
    egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, SCREEN)), ..Default::default() }
}

#[test]
fn layout_math() {
    let u = Layout::Uniform { rows: 10, height: 20.0 };
    assert_eq!(u.total(), 200.0);
    assert_eq!(u.row_at(39.9), 1);
    assert_eq!(u.row_at(-5.0), 0);
    assert_eq!(u.row_at(1e9), 9);
    assert_eq!(u.visible(30.0, 70.0, 0), 1..4);
    assert_eq!(u.visible(30.0, 70.0, 2), 0..6);
    let mut o = Offsets::default();
    let heights = |i: usize| if i == 2 { f32::NAN } else { 10.0 + i as f32 };
    assert!(o.update(5, 1, &heights));
    assert!(!o.update(5, 1, &heights), "same revision and count: kept");
    assert_eq!(o.offsets, [0.0, 10.0, 21.0, 21.0, 34.0, 48.0]);
    let v = Layout::Variable(&o.offsets);
    assert_eq!(v.row_at(20.9), 1);
    assert_eq!(v.row_at(21.0), 3, "zero-height row 2 is skipped");
    assert_eq!(v.span(4), (34.0, 14.0));
    assert_eq!(v.visible(0.0, 0.0, 0), 0..0);
    assert_eq!(Layout::Variable(&[0.0]).visible(0.0, 100.0, 3), 0..0);
}

/// Frames a 100k-row list and counts row-painter calls per frame.
fn rows_painted(rows: usize, heights: RowHeights<'_>, frames: usize, scroll_to: &[usize]) -> Vec<usize> {
    let ctx = egui::Context::default();
    let mut counts = Vec::new();
    for f in 0..frames {
        let mut n = 0;
        let target = scroll_to.get(f).copied();
        run(&ctx, input(), |ui| {
            VirtualList::new("big", rows, heights).scroll_to(target).show(ui, |ui, row| {
                n += 1;
                let _ = (ui, row);
            });
        });
        counts.push(n);
    }
    counts
}

#[test]
fn hundred_thousand_uniform_rows_paint_only_the_visible_ones() {
    let counts = rows_painted(100_000, RowHeights::Uniform(20.0), 4, &[0, 50_000, 99_999, 10]);
    let max_visible = (SCREEN.y / 20.0) as usize + 2 * 2 + 2;
    for c in counts {
        assert!(c > 0 && c <= max_visible, "{c} rows painted, at most {max_visible} visible");
    }
}

#[test]
fn variable_heights_are_summed_once_per_revision() {
    let calls = Cell::new(0usize);
    let height = |i: usize| {
        calls.set(calls.get() + 1);
        16.0 + (i % 7) as f32 * 4.0
    };
    let ctx = egui::Context::default();
    let mut painted = Vec::new();
    for (frame, revision) in [(0, 1u64), (1, 1), (2, 1), (3, 2)] {
        calls.set(0);
        let mut n = 0;
        run(&ctx, input(), |ui| {
            VirtualList::new("var", 100_000, RowHeights::Variable { revision, height: &height })
                .scroll_to(Some(frame * 30_000))
                .show(ui, |_, _| n += 1);
        });
        painted.push(n);
        match frame {
            0 | 3 => assert_eq!(calls.get(), 100_000, "frame {frame}: table built"),
            _ => assert_eq!(calls.get(), 0, "frame {frame}: no per-row work"),
        }
    }
    assert!(painted.iter().all(|n| *n > 0 && *n <= (SCREEN.y / 16.0) as usize + 6), "{painted:?}");
}

/// Per-frame cost must not grow with the row count: a 100k-row list costs about what a 1k-row
/// list costs once its offsets exist.
#[test]
fn frame_cost_is_independent_of_row_count() {
    fn median_frame(rows: usize, variable: bool) -> f64 {
        let ctx = egui::Context::default();
        let height = |i: usize| 18.0 + (i % 3) as f32;
        let heights = if variable { RowHeights::Variable { revision: 1, height: &height } } else { RowHeights::Uniform(20.0) };
        let mut times = Vec::new();
        for f in 0..40usize {
            let target = Some((f * 7919) % rows);
            let start = Instant::now();
            run(&ctx, input(), |ui| {
                VirtualList::new("perf", rows, heights).scroll_to(target).stripes(true).show(ui, |ui, row| {
                    ui.painter().rect_filled(row.rect.shrink(2.0), 0, egui::Color32::GRAY);
                });
            });
            if f >= 5 {
                times.push(start.elapsed().as_secs_f64());
            }
        }
        times.sort_by(f64::total_cmp);
        times[times.len() / 2]
    }
    for variable in [false, true] {
        let small = median_frame(1_000, variable);
        let big = median_frame(100_000, variable);
        eprintln!("variable={variable}: 1k rows {:.3} ms, 100k rows {:.3} ms per frame", small * 1e3, big * 1e3);
        assert!(big < small * 3.0 + 0.002, "variable={variable}: 100k rows {big:.6}s vs 1k rows {small:.6}s");
    }
}

struct State {
    selected: Option<usize>,
    activated: Option<usize>,
    visible: Range<usize>,
}

fn list_harness(rows: usize) -> Harness<'static, State> {
    let mut h = Harness::builder().with_size(SCREEN).with_step_dt(0.02).build_ui_state(
        move |ui, s: &mut State| {
            let label = |_: usize| "item";
            let out = VirtualList::new("files", rows, RowHeights::Uniform(20.0)).selected(s.selected).row_label(&label).show(ui, |ui, row| {
                ui.label(if row.selected { "selected" } else { "row" });
            });
            if let Some(i) = out.select {
                s.selected = Some(i);
            }
            if out.activated.is_some() {
                s.activated = out.activated;
            }
            s.visible = out.visible;
        },
        State { selected: None, activated: None, visible: 0..0 },
    );
    crate::automation::install(&h.ctx);
    h.step();
    h.step();
    h
}

#[test]
fn click_selects_keyboard_moves_and_scrolls_enter_activates() {
    let mut h = list_harness(1000);
    let row3 = crate::automation::find(&h.ctx, "files.row.3").unwrap();
    assert_eq!(row3.label, "item");
    click(&mut h, row3.center());
    assert_eq!(h.state().selected, Some(3));
    h.key_press(Key::ArrowDown);
    h.step();
    assert_eq!(h.state().selected, Some(4));
    h.key_press(Key::End);
    h.step();
    h.step();
    assert_eq!(h.state().selected, Some(999));
    assert!(h.state().visible.contains(&999), "scrolled to the end: {:?}", h.state().visible);
    assert_eq!(crate::automation::find(&h.ctx, "files.row.999").and_then(|e| e.selected), Some(true));
    h.key_press(Key::PageUp);
    h.step();
    let after_page = h.state().selected.unwrap();
    assert!(after_page < 999 && after_page > 950, "{after_page}");
    h.key_press(Key::Enter);
    h.step();
    assert_eq!(h.state().activated, Some(after_page));
}

#[test]
fn empty_list_handles_keys() {
    let mut h = list_harness(0);
    let list = crate::automation::find(&h.ctx, "files").unwrap();
    click(&mut h, list.center());
    h.key_press(Key::ArrowDown);
    h.key_press(Key::End);
    h.step();
    assert_eq!(h.state().selected, None);
}

#[test]
fn table_cells_are_virtualized_headers_click_and_columns_resize() {
    struct T {
        cells: usize,
        header: Option<usize>,
        widths: Vec<f32>,
    }
    let cols = [Column::new("Name", 160.0), Column::new("Size", 80.0), Column { resizable: false, ..Column::new("Kind", 100.0) }];
    let mut h = Harness::builder().with_size(SCREEN).build_ui_state(
        move |ui, s: &mut T| {
            s.cells = 0;
            let out = VirtualTable::new("table", &cols, 100_000, RowHeights::Uniform(20.0)).sort(Some(Sort { column: 0, ascending: true })).show(
                ui,
                |ui, row, col| {
                    s.cells += 1;
                    ui.label(if col == 0 { "name" } else { "x" });
                    let _ = row;
                },
            );
            s.header = s.header.or(out.header_clicked);
            s.widths = out.widths;
        },
        T { cells: 0, header: None, widths: Vec::new() },
    );
    crate::automation::install(&h.ctx);
    h.step();
    h.step();
    let max_rows = (SCREEN.y / 20.0) as usize + 6;
    assert!(h.state().cells > 0 && h.state().cells <= max_rows * 3, "{} cells", h.state().cells);
    let size = crate::automation::find(&h.ctx, "table.header.1").unwrap();
    assert_eq!(crate::automation::find(&h.ctx, "table.header.0").and_then(|e| e.selected), Some(true));
    click(&mut h, size.center());
    assert_eq!(h.state().header, Some(1));
    // drag the Name column's right edge 40 points right
    let edge = egui::pos2(size.egui_rect().left(), size.center().y);
    h.hover_at(edge);
    h.step();
    h.drag_at(edge);
    h.step();
    for dx in [10.0, 25.0, 40.0] {
        h.hover_at(edge + egui::vec2(dx, 0.0));
        h.step();
    }
    h.drop_at(edge + egui::vec2(40.0, 0.0));
    h.step();
    let w = h.state().widths[0];
    assert!(w > 180.0 && w <= 200.5, "{w}");
    assert_eq!(h.state().widths[1], 80.0);
}
