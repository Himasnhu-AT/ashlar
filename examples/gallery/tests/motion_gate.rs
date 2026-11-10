#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The motion gate over the gallery's real interactions at 120 Hz: open and close the dialog,
//! show and dismiss a toast, switch the theme with its crossfade, switch tabs and pages. Each
//! one must move without overshoot, reversal, pops or flicker, and go idle once settled.

use ashlar::ui::automation;
use ashlar::ui::motion_gate::{MotionRecorder, MotionReport, MotionRules};
use ashlar::ui::tokens::{self, ThemeMode};
use ashlar_example_gallery::{Gallery, Page};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

const DT: f32 = 1.0 / 120.0;

fn harness(page: Page) -> Harness<'static, Gallery> {
    let mut g = Gallery::new();
    g.page = page;
    let mut h = Harness::builder()
        .with_size(egui::vec2(1180.0, 780.0))
        .with_theme(egui::Theme::Dark)
        .with_step_dt(DT)
        .build_ui_state(|ui, g: &mut Gallery| g.ui(ui), g);
    // let startup settle before recording
    h.run_steps(30);
    h
}

/// Step `secs` of 120 Hz frames, recording each.
fn wait(h: &mut Harness<'_, Gallery>, rec: &MotionRecorder, secs: f32) {
    for _ in 0..(secs / DT).ceil() as usize {
        h.step();
        rec.frame(&h.ctx, h.output());
    }
}

/// Click the automation element `id` (hover, press, release over a few recorded frames).
fn click(h: &mut Harness<'_, Gallery>, rec: &MotionRecorder, id: &str) {
    let p = automation::find(&h.ctx, id).unwrap_or_else(|| panic!("no element {id}")).center();
    h.hover_at(p);
    wait(h, rec, DT);
    h.drag_at(p);
    wait(h, rec, DT);
    h.drop_at(p);
    wait(h, rec, DT);
}

fn gate(h: &mut Harness<'_, Gallery>, run: impl FnOnce(&mut Harness<'_, Gallery>, &MotionRecorder)) -> MotionReport {
    let rec = MotionRecorder::install(&h.ctx);
    run(h, &rec);
    MotionRecorder::uninstall(&h.ctx);
    let report = rec.check(&MotionRules::default());
    assert!(report.frames > 0, "{report}");
    report
}

#[test]
fn dialog_opens_and_closes_cleanly() {
    let mut h = harness(Page::Overlays);
    let report = gate(&mut h, |h, rec| {
        click(h, rec, "overlays.open");
        wait(h, rec, 1.0);
        assert!(automation::find(&h.ctx, "overlays.dialog").is_some(), "the dialog opened");
        h.key_press(egui::Key::Escape);
        wait(h, rec, 1.0);
        assert!(automation::find(&h.ctx, "overlays.dialog").is_none(), "the dialog closed");
    });
    assert!(report.samples > 0, "nothing animated: {report}");
    assert!(report.is_clean(), "{report}");
}

#[test]
fn toast_shows_and_dismisses_cleanly() {
    let mut h = harness(Page::Overlays);
    let report = gate(&mut h, |h, rec| {
        h.get_by_label("Sticky").click();
        wait(h, rec, 1.0);
        let close = automation::query(&h.ctx, "toast.").into_iter().find(|e| e.id.ends_with(".close")).expect("a toast with a close button");
        click(h, rec, &close.id);
        wait(h, rec, 1.0);
        assert!(automation::query(&h.ctx, "toast.").is_empty(), "the toast left");
    });
    assert!(report.samples > 0, "nothing animated: {report}");
    assert!(report.is_clean(), "{report}");
}

#[test]
fn theme_crossfade_is_clean() {
    let mut h = harness(Page::Overlays);
    let report = gate(&mut h, |h, rec| {
        tokens::set_mode_animated(&h.ctx, ThemeMode::Light);
        wait(h, rec, 1.0);
        tokens::set_mode_animated(&h.ctx, ThemeMode::Dark);
        wait(h, rec, 1.0);
    });
    assert!(report.samples > 0, "nothing animated: {report}");
    assert!(report.is_clean(), "{report}");
}

#[test]
fn tab_and_page_switches_are_clean() {
    let mut h = harness(Page::Tabs);
    let report = gate(&mut h, |h, rec| {
        click(h, rec, "tabs.documents.tab.2");
        wait(h, rec, 1.0);
        click(h, rec, "tabs.documents.tab.0");
        wait(h, rec, 1.0);
        h.state_mut().page = Page::Overlays;
        wait(h, rec, 1.0);
    });
    assert!(report.is_clean(), "{report}");
}
