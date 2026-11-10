#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The motion gate: seeded faulty curves give each finding once, real motion passes, and a view
//! that asks for frames forever fails the idle check.

use ashlar_motion::{Class, Spring, tokens};
use ashlar_ui::automation::{self, Details, Role};
use ashlar_ui::motion;
use ashlar_ui::motion_gate::{Allow, Frame, MotionRecorder, MotionReport, MotionRules, Rule, Sample};
use ashlar_ui::wake::{Reason, wake};
use egui::{Id, Rect, pos2, vec2};

const DT: f64 = 1.0 / 120.0;

fn id() -> Id {
    Id::new("gate.value")
}

/// The one finding of `report`, which must be of `rule`.
fn only(report: &MotionReport, rule: Rule) {
    assert_eq!(report.findings.len(), 1, "{report}");
    assert_eq!(report.findings[0].rule, rule, "{report}");
}

/// Samples of `values` toward `target`, with velocities consistent with the steps.
fn track(rec: &MotionRecorder, class: Class, bounce: f32, target: f32, values: &[f32]) {
    let mut v = 0.0_f32;
    let mut prev: Option<f32> = None;
    for (i, &x) in values.iter().enumerate() {
        if let Some(p) = prev {
            v = 2.0 * (x - p) / DT as f32 - v;
        }
        rec.push(Sample::new(id(), i as f64 * DT, x, target, class).velocity(v).bounce(bounce));
        prev = Some(x);
    }
}

fn rect_frames(rec: &MotionRecorder, xs: &[Option<f32>]) {
    for (i, x) in xs.iter().enumerate() {
        let rects = x.map(|x| vec![("box".to_owned(), [x, 10.0, 50.0, 20.0])]).unwrap_or_default();
        rec.push_frame(Frame { time: i as f64 * DT, repaint_after: None, reasons: Vec::new(), rects });
    }
}

#[test]
fn an_effect_that_overshoots_is_flagged_once() {
    let rec = MotionRecorder::new();
    track(&rec, Class::Opacity, 0.3, 1.0, &[0.0, 0.4, 0.8, 1.06, 1.08, 1.03, 1.0]);
    only(&rec.check(&MotionRules::default()), Rule::Overshoot);
}

#[test]
fn a_value_without_bounce_that_turns_back_is_flagged_once() {
    let rec = MotionRecorder::new();
    track(&rec, Class::Spatial, 0.0, 100.0, &[0.0, 30.0, 60.0, 80.0, 75.0, 90.0, 100.0]);
    only(&rec.check(&MotionRules::default()), Rule::Reversal);
    // the same curve with bounce is a spring doing its job
    let rec = MotionRecorder::new();
    track(&rec, Class::Spatial, 0.3, 100.0, &[0.0, 30.0, 60.0, 80.0, 75.0, 90.0, 100.0]);
    assert!(rec.check(&MotionRules::default()).is_clean());
}

#[test]
fn a_value_that_snaps_to_its_target_pops_once() {
    let rec = MotionRecorder::new();
    // 1 pt per frame, then the last 36 pt in one frame
    for (i, x) in [0.0, 1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
        rec.push(Sample::new(id(), i as f64 * DT, x, 40.0, Class::Spatial).velocity(120.0));
    }
    rec.push(Sample::new(id(), 5.0 * DT, 40.0, 40.0, Class::Spatial));
    only(&rec.check(&MotionRules::default()), Rule::Pop);
}

#[test]
fn a_moving_element_that_jumps_pops_once() {
    let rec = MotionRecorder::new();
    rect_frames(&rec, &[Some(0.0), Some(2.0), Some(4.0), Some(6.0), Some(100.0), Some(100.0), Some(100.0)]);
    only(&rec.check(&MotionRules::default()), Rule::Pop);
    // a still element moved by a layout change is not a pop
    let rec = MotionRecorder::new();
    rect_frames(&rec, &[Some(0.0), Some(0.0), Some(100.0), Some(100.0)]);
    assert!(rec.check(&MotionRules::default()).is_clean());
}

#[test]
fn flicker_is_flagged_once() {
    let rec = MotionRecorder::new();
    rect_frames(&rec, &[Some(0.0), Some(0.0), Some(0.0), None, Some(0.0), Some(0.0)]);
    only(&rec.check(&MotionRules::default()), Rule::Flicker);

    let rec = MotionRecorder::new();
    for (i, x) in [1.0, 1.0, 0.0, 1.0, 1.0].into_iter().enumerate() {
        rec.push(Sample::new(id(), i as f64 * 0.05, x, x, Class::Opacity));
    }
    only(&rec.check(&MotionRules::default()), Rule::Flicker);
}

#[test]
fn frames_after_settle_are_flagged_once_and_can_be_allowed() {
    let rec = MotionRecorder::new();
    for i in 0..10 {
        rec.push_frame(Frame { time: i as f64 * DT, repaint_after: Some(0.0), reasons: vec!["spinner".into()], rects: Vec::new() });
    }
    let report = rec.check(&MotionRules::default());
    only(&report, Rule::Idle);
    assert_eq!(report.findings[0].count, 10);
    let report = rec.check(&MotionRules::default().allowing(Allow::reason("spinner", "an indeterminate spinner runs until done")));
    assert!(report.is_clean());
    assert!(report.to_string().contains("an indeterminate spinner runs until done"), "{report}");
}

/// Run one 120 Hz frame and record it.
fn frame(ctx: &egui::Context, rec: &MotionRecorder, n: usize, mut ui: impl FnMut(&mut egui::Ui)) {
    let input = egui::RawInput {
        time: Some(n as f64 * DT),
        predicted_dt: DT as f32,
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 600.0))),
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |root| ui(root));
    out.textures_delta.clear();
    rec.frame(ctx, &out);
}

#[test]
fn real_motion_passes_and_is_recorded() {
    let ctx = egui::Context::default();
    automation::install(&ctx);
    let rec = MotionRecorder::install(&ctx);
    let (slide, fade) = (Id::new("gate.slide"), Id::new("gate.fade"));
    rec.name(slide, "slide");
    rec.name(fade, "fade");
    for n in 0..200 {
        let open = n >= 10;
        frame(&ctx, &rec, n, |ui| {
            let ctx = ui.ctx().clone();
            let x = motion::value(&ctx, slide, if open { 300.0 } else { 0.0 }, Class::Spatial, tokens::SNAP.pick(open));
            let alpha = motion::opacity(&ctx, fade, open);
            let w = motion::value(&ctx, Id::new("gate.width"), if open { 200.0 } else { 80.0 }, Class::Spatial, Spring::new(0.3, 0.0));
            if alpha > 0.0 {
                let rect = Rect::from_min_size(pos2(x, 40.0), vec2(w, 30.0));
                automation::register_fmt(ui, rect, format_args!("gate.panel"), "Panel", Role::Group, true, Details::default());
            }
        });
    }
    MotionRecorder::uninstall(&ctx);
    let report = rec.check(&MotionRules::default());
    assert!(report.is_clean(), "{report}");
    assert!(report.samples >= 3 * 200, "{report}");
    assert!(rec.frames().iter().any(|f| f.rects.iter().any(|(id, _)| id == "gate.panel")));
    // settled frames asked for nothing
    assert!(rec.frames().last().unwrap().repaint_after.is_none());
}

#[test]
fn recorded_springs_carry_their_bounce() {
    // a bouncy spring swings past its target: fine with its bounce, a reversal without
    let ctx = egui::Context::default();
    let rec = MotionRecorder::install(&ctx);
    for n in 0..120 {
        frame(&ctx, &rec, n, |ui| {
            let target = if n >= 2 { 100.0 } else { 0.0 };
            motion::value(ui.ctx(), id(), target, Class::Spatial, Spring::new(0.4, 0.5));
        });
    }
    MotionRecorder::uninstall(&ctx);
    assert!(rec.check(&MotionRules::default()).is_clean(), "{}", rec.check(&MotionRules::default()));
    let samples = rec.samples();
    assert!(samples.iter().any(|s| s.value[0] > 100.5 && s.bounce > 0.0));
    let flat = MotionRecorder::new();
    for s in samples {
        flat.push(Sample { bounce: 0.0, ..s });
    }
    let report = flat.check(&MotionRules::default());
    assert_eq!(report.of(Rule::Reversal).count(), 1, "{report}");
}

#[test]
fn a_view_that_never_stops_asking_for_frames_fails_the_idle_check() {
    let ctx = egui::Context::default();
    let rec = MotionRecorder::install(&ctx);
    for n in 0..30 {
        frame(&ctx, &rec, n, |ui| wake(ui.ctx(), Reason::ANIMATION));
    }
    let report = rec.check(&MotionRules::default());
    only(&report, Rule::Idle);
    assert!(report.to_string().contains("animation"), "{report}");

    // a view that settles passes
    let ctx = egui::Context::default();
    let rec = MotionRecorder::install(&ctx);
    for n in 0..30 {
        frame(&ctx, &rec, n, |ui| {
            if n < 2 {
                ui.ctx().request_repaint();
            }
        });
    }
    let report = rec.check(&MotionRules::default());
    assert!(report.is_clean(), "{report}");
}
