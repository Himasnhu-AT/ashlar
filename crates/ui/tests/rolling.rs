#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Rolling text on a virtual clock: it settles and then asks for nothing, passes the motion gate
//! (no reversal, no pop) even when a change lands mid-roll, same text is a no-op, and the
//! accessible value is the whole new text at once.

use ashlar_motion::MotionPrefs;
use ashlar_ui::automation;
use ashlar_ui::motion;
use ashlar_ui::motion_gate::{MotionRecorder, MotionRules};
use ashlar_ui::rolling::RollingText;
use egui::{Rect, pos2, vec2};

const DT: f64 = 1.0 / 120.0;

fn frame(ctx: &egui::Context, rec: &MotionRecorder, n: usize, text: &str, value: f64) {
    let input = egui::RawInput {
        time: Some(n as f64 * DT),
        predicted_dt: DT as f32,
        screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0))),
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        RollingText::new("t.cost", text).value(value).tabular(true).show(ui);
    });
    out.textures_delta.clear();
    rec.frame(ctx, &out);
}

fn setup(reduced: bool) -> (egui::Context, MotionRecorder) {
    let ctx = egui::Context::default();
    automation::install(&ctx);
    motion::set_prefs(&ctx, MotionPrefs { reduced, ..MotionPrefs::default() });
    let rec = MotionRecorder::install(&ctx);
    (ctx, rec)
}

fn width(ctx: &egui::Context) -> f32 {
    automation::find(ctx, "t.cost").expect("registered").rect[2]
}

#[test]
fn a_roll_settles_cleanly_and_then_asks_for_nothing() {
    let (ctx, rec) = setup(false);
    let script = |n: usize| match n {
        0..10 => ("$9.99", 9.99),
        10..16 => ("$10.00", 10.0), // interrupted mid-roll
        16..100 => ("$10.01", 10.01),
        _ => ("$9.50", 9.5), // and down again
    };
    let mut widths = Vec::new();
    for n in 0..260 {
        let (text, value) = script(n);
        frame(&ctx, &rec, n, text, value);
        let el = automation::find(&ctx, "t.cost").unwrap();
        assert_eq!(el.value, text, "the accessible value is the whole text at once (frame {n})");
        widths.push(width(&ctx));
    }
    MotionRecorder::uninstall(&ctx);
    let report = rec.check(&MotionRules::default());
    assert!(report.is_clean(), "{report}");
    assert!(report.samples > 0, "{report}");
    assert!(rec.frames().last().unwrap().repaint_after.is_none(), "settled frames ask for nothing");
    assert_eq!(motion::active(&ctx), 0);
    // the width grew smoothly from 5 to 6 clusters (more than one in-between width)
    let between: Vec<f32> = widths.get(10..40).unwrap().iter().copied().filter(|w| *w > widths[0] + 0.5 && *w < widths[45] - 0.5).collect();
    assert!(between.len() >= 3, "width animates: {widths:?}");
}

#[test]
fn the_same_text_is_a_no_op() {
    let (ctx, rec) = setup(false);
    for n in 0..20 {
        frame(&ctx, &rec, n, "42", 42.0);
    }
    let report = rec.check(&MotionRules::default());
    assert!(report.is_clean(), "{report}");
    assert!(rec.frames().iter().skip(2).all(|f| f.repaint_after.is_none()), "nothing animates: {:?}", rec.frames());
    assert_eq!(motion::active(&ctx), 0);
    // a changed value with the same text does not roll either
    frame(&ctx, &rec, 20, "42", 42.4);
    assert_eq!(motion::active(&ctx), 0);
}

#[test]
fn reduced_motion_crossfades_briefly_without_width_travel() {
    let (ctx, rec) = setup(true);
    frame(&ctx, &rec, 0, "9", 9.0);
    let w0 = width(&ctx);
    frame(&ctx, &rec, 1, "10", 10.0);
    // the width jumps (no spatial travel under reduced motion)
    let w1 = width(&ctx);
    assert!(w1 > w0);
    for n in 2..120 {
        frame(&ctx, &rec, n, "10", 10.0);
        assert_eq!(width(&ctx), w1);
    }
    MotionRecorder::uninstall(&ctx);
    let report = rec.check(&MotionRules::default());
    assert!(report.is_clean(), "{report}");
    // the crossfade is the short reduced fade: done well within half a second
    let last_moving = rec.frames().iter().filter(|f| f.repaint_after.is_some()).map(|f| f.time).fold(0.0, f64::max);
    assert!(last_moving > 0.0 && last_moving < 0.4, "{last_moving}");
}
