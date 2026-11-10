#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Gesture handoff on a virtual clock: a constant-velocity drag is estimated within 2%, a pause of
//! 40 ms or more before release gives no fling, reduced motion never flings, and a list fling
//! (and rubber band) settles cleanly and then asks for nothing.

use ashlar_motion::MotionPrefs;
use ashlar_motion::gesture::{decay_k, deceleration};
use ashlar_ui::automation;
use ashlar_ui::edit::{Edit, EditPhase};
use ashlar_ui::motion;
use ashlar_ui::motion_gate::{MotionRecorder, MotionRules, Rule};
use ashlar_ui::slider::Slider;
use ashlar_ui::value_field::{Decimal, ValueField};
use ashlar_ui::virtual_list::{RowHeights, VirtualList};
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};

const DT: f64 = 1.0 / 120.0;

/// A context driven frame by frame with pointer events.
struct Rig {
    ctx: egui::Context,
    rec: MotionRecorder,
    n: usize,
}

impl Rig {
    fn new(reduced: bool) -> Self {
        let ctx = egui::Context::default();
        automation::install(&ctx);
        motion::set_prefs(&ctx, MotionPrefs { reduced, ..MotionPrefs::default() });
        let rec = MotionRecorder::install(&ctx);
        Rig { ctx, rec, n: 0 }
    }

    fn frame(&mut self, events: Vec<Event>, ui: &mut dyn FnMut(&mut egui::Ui)) {
        let input = egui::RawInput {
            time: Some(self.n as f64 * DT),
            predicted_dt: DT as f32,
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0))),
            events,
            ..Default::default()
        };
        let mut out = self.ctx.run_ui(input, |root| ui(root));
        out.textures_delta.clear();
        self.rec.frame(&self.ctx, &out);
        self.n += 1;
    }

    fn center(&self, id: &str) -> Pos2 {
        automation::find(&self.ctx, id).expect("registered").center()
    }

    /// Press at `from`, move by `step` per frame for `frames` frames, hold still for `hold`
    /// frames, then release (moving one more step in the release frame unless holding).
    fn drag(&mut self, from: Pos2, step: egui::Vec2, frames: usize, hold: usize, ui: &mut dyn FnMut(&mut egui::Ui)) -> Pos2 {
        let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        self.frame(vec![Event::PointerMoved(from)], ui);
        self.frame(vec![press(from, true)], ui);
        let mut at = from;
        for _ in 0..frames {
            at += step;
            self.frame(vec![Event::PointerMoved(at)], ui);
        }
        for _ in 0..hold {
            self.frame(Vec::new(), ui);
        }
        if hold == 0 {
            at += step;
            self.frame(vec![Event::PointerMoved(at), press(at, false)], ui);
        } else {
            self.frame(vec![press(at, false)], ui);
        }
        at
    }

    fn settle(&mut self, frames: usize, ui: &mut dyn FnMut(&mut egui::Ui)) {
        for _ in 0..frames {
            self.frame(Vec::new(), ui);
        }
    }

    fn assert_settled_clean(&self) {
        MotionRecorder::uninstall(&self.ctx);
        let report = self.rec.check(&MotionRules::default());
        // egui asks for frames while a button is held (click timing): input, not motion, so the
        // idle rule is checked below on the frames after release instead
        assert!(report.findings.iter().all(|f| f.rule == Rule::Idle), "{report}");
        let frames = self.rec.frames();
        assert!(frames.iter().rev().take(10).all(|f| f.repaint_after.is_none()), "settled frames ask for nothing");
        assert_eq!(motion::active(&self.ctx), 0);
    }
}

/// A field with 0.5 units per point; returns its edits.
fn field_rig(reduced: bool) -> (Rig, std::rc::Rc<std::cell::RefCell<(f64, Vec<Edit>)>>, impl FnMut(&mut egui::Ui)) {
    let state = std::rc::Rc::new(std::cell::RefCell::new((0.0_f64, Vec::<Edit>::new())));
    let s = state.clone();
    let fmt = Decimal::default();
    let ui = move |ui: &mut egui::Ui| {
        let mut st = s.borrow_mut();
        let out = ValueField::new("t.field", "Field", &mut st.0, &fmt).speed(0.5).width(120.0).show(ui);
        st.1.extend(out.edit);
    };
    (Rig::new(reduced), state, ui)
}

#[test]
fn a_constant_velocity_drag_flings_by_its_velocity_within_2_percent() {
    let (mut rig, state, mut ui) = field_rig(false);
    rig.frame(Vec::new(), &mut ui);
    let c = rig.center("t.field");
    // 2 pt per 1/120 s frame: 240 pt/s, 120 units/s
    rig.drag(c, vec2(2.0, 0.0), 30, 0, &mut ui);
    let edits = state.borrow().1.clone();
    let commit = *edits.last().unwrap();
    assert_eq!(commit.phase, EditPhase::Commit);
    // where the hand let go (egui applies the release before the release frame's movement)
    let released = edits.iter().rev().find(|e| e.phase == EditPhase::Update).unwrap().value;
    let estimated = (commit.value - released) * f64::from(decay_k(deceleration::FAST));
    assert!((estimated / 120.0 - 1.0).abs() < 0.02, "estimated {estimated} units/s for 120");
    assert_eq!(edits.iter().filter(|e| e.phase == EditPhase::Commit).count(), 1, "one drag, one commit");
    // the field eases there, then rests on the committed value and asks for nothing
    rig.settle(120, &mut ui);
    assert_eq!(state.borrow().0, commit.value);
    assert_eq!(automation::find(&rig.ctx, "t.field").unwrap().value, format!("{:.2}", commit.value));
    rig.assert_settled_clean();
}

#[test]
fn a_pause_of_40_ms_before_release_does_not_fling() {
    let (mut rig, state, mut ui) = field_rig(false);
    rig.frame(Vec::new(), &mut ui);
    let c = rig.center("t.field");
    // 5 frames at 120 Hz = 41.7 ms still
    rig.drag(c, vec2(2.0, 0.0), 30, 5, &mut ui);
    let edits = state.borrow().1.clone();
    let commit = *edits.last().unwrap();
    let last_update = edits.iter().rev().find(|e| e.phase == EditPhase::Update).unwrap().value;
    assert_eq!(commit.value, last_update);
    rig.settle(60, &mut ui);
    rig.assert_settled_clean();
}

#[test]
fn reduced_motion_never_flings() {
    let (mut rig, state, mut ui) = field_rig(true);
    rig.frame(Vec::new(), &mut ui);
    let c = rig.center("t.field");
    rig.drag(c, vec2(2.0, 0.0), 30, 0, &mut ui);
    let edits = state.borrow().1.clone();
    let last_update = edits.iter().rev().find(|e| e.phase == EditPhase::Update).unwrap().value;
    assert_eq!(edits.last().unwrap().value, last_update);
    rig.settle(60, &mut ui);
    rig.assert_settled_clean();
}

#[test]
fn a_slider_flings_and_stops_at_its_end() {
    let mut rig = Rig::new(false);
    let value = std::rc::Rc::new(std::cell::RefCell::new(0.0_f64));
    let v = value.clone();
    let mut ui = move |ui: &mut egui::Ui| {
        Slider::new("t.slider", "Slider", &mut v.borrow_mut(), 0.0..=100.0).width(300.0).show(ui);
    };
    rig.frame(Vec::new(), &mut ui);
    let r = automation::find(&rig.ctx, "t.slider").unwrap().egui_rect();
    // a quick flick toward the right end
    rig.drag(pos2(r.left() + 20.0, r.center().y), vec2(8.0, 0.0), 30, 0, &mut ui);
    assert_eq!(*value.borrow(), 100.0, "the fling ends at the slider's end");
    rig.settle(120, &mut ui);
    assert_eq!(*value.borrow(), 100.0);
    rig.assert_settled_clean();
}

const ROW_H: f32 = 20.0;

/// The list's scroll offset, from the first registered row.
fn list_offset(ctx: &egui::Context, top: f32) -> f32 {
    let rows = automation::query(ctx, "t.list.row.");
    let (i, el) = rows
        .iter()
        .filter_map(|e| e.id.rsplit('.').next().and_then(|n| n.parse::<usize>().ok()).map(|i| (i, e)))
        .min_by_key(|(i, _)| *i)
        .expect("rows");
    i as f32 * ROW_H - (el.rect[1] - top)
}

fn list_ui(ui: &mut egui::Ui) {
    ui.set_max_height(300.0);
    VirtualList::new("t.list", 1000, RowHeights::Uniform(ROW_H)).max_height(300.0).row_label(&|_| "row").fling(true).show(ui, |_, _| {});
}

#[test]
fn a_list_fling_keeps_going_then_settles_and_asks_for_nothing() {
    let mut rig = Rig::new(false);
    let mut ui = list_ui;
    rig.frame(Vec::new(), &mut ui);
    let list = automation::find(&rig.ctx, "t.list").unwrap().egui_rect();
    // flick up: 4 pt per frame, 480 pt/s
    rig.drag(pos2(list.center().x, list.bottom() - 20.0), vec2(0.0, -4.0), 20, 0, &mut ui);
    let released = list_offset(&rig.ctx, list.top());
    assert!((released - 80.0).abs() < 1.0, "the list followed the hand: {released}");
    rig.settle(200, &mut ui);
    let rest = list_offset(&rig.ctx, list.top());
    let expected = released + 480.0 / decay_k(deceleration::NORMAL);
    assert!(rest > released + 100.0, "the fling kept going: {released} → {rest}");
    assert!((rest - expected).abs() < 0.05 * (expected - released), "rest {rest}, expected about {expected}");
    rig.assert_settled_clean();
}

#[test]
fn a_list_pulled_past_its_top_stretches_and_springs_back() {
    let mut rig = Rig::new(false);
    let mut ui = list_ui;
    rig.frame(Vec::new(), &mut ui);
    let list = automation::find(&rig.ctx, "t.list").unwrap().egui_rect();
    let from = pos2(list.center().x, list.top() + 20.0);
    let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    rig.frame(vec![Event::PointerMoved(from)], &mut ui);
    rig.frame(vec![press(from, true)], &mut ui);
    let mut at = from;
    for _ in 0..25 {
        at += vec2(0.0, 4.0);
        rig.frame(vec![Event::PointerMoved(at)], &mut ui);
    }
    // pulled 100 pt: the rows moved down, but less than the hand (rubber band)
    let stretched = -list_offset(&rig.ctx, list.top());
    assert!(stretched > 10.0 && stretched < 95.0, "{stretched}");
    rig.frame(vec![press(at, false)], &mut ui);
    rig.settle(200, &mut ui);
    assert!(list_offset(&rig.ctx, list.top()).abs() < 0.01, "springs back to the top");
    rig.assert_settled_clean();
}
