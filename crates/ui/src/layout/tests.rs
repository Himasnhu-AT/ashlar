use egui::accesskit::{Action, ActionData, ActionRequest, Orientation, Role, TreeId};
use egui::{Color32, Event, Id, Key, Rect, Shape, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

#[test]
fn sizes_are_shared_out_as_requested() {
    let r = rect(10.0, 20.0, 400.0, 200.0);
    for (size, first, second) in
        [(SplitSize::Ratio(0.25), 99.0, 297.0), (SplitSize::FixedFirst(150.0), 150.0, 246.0), (SplitSize::FixedSecond(150.0), 246.0, 150.0)]
    {
        let s = split_rect(r, SplitAxis::Horizontal, size, 4.0, 20.0).unwrap();
        assert_eq!((s.first.width(), s.divider.width(), s.second.width()), (first, 4.0, second), "{size:?}");
        assert_eq!(s.first.max.x, s.divider.min.x);
        assert_eq!(s.divider.max.x, s.second.min.x);
        assert_eq!((s.first.min, s.second.max), (r.min, r.max));
        assert_eq!((s.first.height(), s.second.height()), (200.0, 200.0));
        assert_eq!((s.first_size(), s.available()), (first, 396.0));
    }
    let v = split_rect(rect(0.0, 0.0, 300.0, 200.0), SplitAxis::Vertical, SplitSize::Ratio(0.5), 2.0, 0.0).unwrap();
    assert_eq!((v.first.height(), v.second.height(), v.divider.height()), (99.0, 99.0, 2.0));
    assert_eq!(v.first.width(), 300.0);
}

#[test]
fn panes_stay_inside_the_parent() {
    let sizes = [
        SplitSize::Ratio(0.0),
        SplitSize::Ratio(0.5),
        SplitSize::Ratio(10.0),
        SplitSize::Ratio(-10.0),
        SplitSize::FixedFirst(f32::MAX),
        SplitSize::FixedFirst(-f32::MAX),
        SplitSize::FixedSecond(f32::MAX),
        SplitSize::FixedSecond(-3.0),
    ];
    for span in (0..=1000).step_by(37) {
        let r = rect(-5.0, 7.0, span as f32, span as f32 * 0.5);
        for axis in [SplitAxis::Horizontal, SplitAxis::Vertical] {
            for gap in [0.0, 4.0, 2000.0] {
                for size in sizes {
                    let s = split_rect(r, axis, size, gap, 20.0).unwrap();
                    for part in [s.first, s.divider, s.second] {
                        assert!(part.is_finite() && part.width() >= 0.0 && part.height() >= 0.0, "{part:?}");
                        assert!(r.contains_rect(part), "{part:?} outside {r:?} ({axis:?} {gap} {size:?})");
                    }
                }
            }
        }
    }
}

#[test]
fn malformed_input_gives_none() {
    let r = rect(0.0, 0.0, 100.0, 50.0);
    let h = SplitAxis::Horizontal;
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for size in [SplitSize::Ratio(bad), SplitSize::FixedFirst(bad), SplitSize::FixedSecond(bad)] {
            assert_eq!(split_rect(r, h, size, 4.0, 0.0), None);
        }
        assert_eq!(split_rect(r, h, SplitSize::Ratio(0.5), bad, 0.0), None);
        assert_eq!(split_rect(r, h, SplitSize::Ratio(0.5), 4.0, bad), None);
        assert_eq!(split_rect(Rect::from_min_max(pos2(bad, 0.0), pos2(10.0, 10.0)), h, SplitSize::Ratio(0.5), 4.0, 0.0), None);
    }
    assert_eq!(split_rect(r, h, SplitSize::Ratio(0.5), -1.0, 0.0), None);
    assert_eq!(split_rect(r, h, SplitSize::Ratio(0.5), 1.0, -1.0), None);
    assert_eq!(split_rect(Rect::from_min_max(pos2(10.0, 0.0), pos2(0.0, 10.0)), h, SplitSize::Ratio(0.5), 1.0, 0.0), None);
    let infinite = Rect::from_min_max(pos2(-f32::MAX, 0.0), pos2(f32::MAX, 10.0));
    assert_eq!(split_rect(infinite, h, SplitSize::Ratio(0.5), 1.0, 0.0), None);

    let huge = Rect::from_min_max(pos2(0.0, 0.0), pos2(f32::MAX, 10.0));
    for size in [SplitSize::Ratio(f32::MAX), SplitSize::FixedFirst(f32::MAX), SplitSize::FixedSecond(-f32::MAX)] {
        let s = split_rect(huge, h, size, 4.0, 0.0).unwrap();
        for part in [s.first, s.divider, s.second] {
            assert!(part.is_finite() && huge.contains_rect(part), "{part:?}");
        }
    }
    assert!(split_rect(Rect::from_min_size(pos2(3.0, 3.0), vec2(0.0, 0.0)), h, SplitSize::Ratio(0.5), 4.0, 10.0).is_some());
}

#[test]
fn resizing_keeps_the_variant_and_the_minimum() {
    let r = rect(0.0, 0.0, 400.0, 100.0);
    let h = SplitAxis::Horizontal;
    let s = split_rect(r, h, SplitSize::FixedFirst(100.0), 4.0, 40.0).unwrap();
    assert_eq!(s.available(), 396.0);
    assert_eq!(s.resized(SplitSize::Ratio(0.1), 20.0, 40.0), Some(SplitSize::Ratio((120.0f64 / 396.0) as f32)));
    assert_eq!(s.resized(SplitSize::FixedFirst(100.0), 20.0, 40.0), Some(SplitSize::FixedFirst(120.0)));
    assert_eq!(s.resized(SplitSize::FixedSecond(296.0), 20.0, 40.0), Some(SplitSize::FixedSecond(276.0)));
    assert_eq!(s.resized(SplitSize::FixedFirst(100.0), -1000.0, 40.0), Some(SplitSize::FixedFirst(40.0)));
    assert_eq!(s.resized(SplitSize::FixedFirst(100.0), 1000.0, 40.0), Some(SplitSize::FixedFirst(356.0)));
    assert_eq!(s.resized(SplitSize::FixedFirst(100.0), f32::NAN, 40.0), None);
    assert_eq!(s.resized(SplitSize::FixedFirst(f32::NAN), 1.0, 40.0), None);
    assert_eq!(s.resized(SplitSize::FixedFirst(100.0), 1.0, -1.0), None);

    let narrow = split_rect(rect(0.0, 0.0, 19.0, 10.0), h, SplitSize::FixedFirst(3.0), 4.0, 40.0).unwrap();
    assert_eq!(narrow.resized(SplitSize::FixedFirst(3.0), 5.0, 40.0), Some(SplitSize::FixedFirst(7.5)));
    assert_eq!(narrow.resized(SplitSize::FixedSecond(3.0), -5.0, 40.0), Some(SplitSize::FixedSecond(7.5)));
    let empty = split_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 0.0)), h, SplitSize::Ratio(0.5), 0.0, 0.0).unwrap();
    assert_eq!(empty.resized(SplitSize::Ratio(0.5), 1.0, 0.0), None);
}

const STYLE: ResizeHandleStyle =
    ResizeHandleStyle { hovered: Color32::from_rgb(10, 20, 30), dragged: Color32::from_rgb(40, 50, 60), focused: Color32::from_rgb(70, 80, 90) };

struct Handle {
    axis: SplitAxis,
    rect: Rect,
    label: String,
    enabled: bool,
    value: Option<f32>,
    /// Sum of the deltas reported since the last [`take`] (a step can run more than one pass).
    delta: f32,
    stopped: bool,
    last: Option<egui::Response>,
}

fn handle(axis: SplitAxis, rect: Rect) -> Harness<'static, Handle> {
    let state = Handle { axis, rect, label: "Resize panes".into(), enabled: true, value: None, delta: 0.0, stopped: false, last: None };
    Harness::builder().with_size(vec2(400.0, 300.0)).build_ui_state(
        |ui, s: &mut Handle| {
            let enabled = s.enabled;
            ui.add_enabled_ui(enabled, |ui| {
                let mut w = ResizeHandle::new(Id::new("divider"), s.axis, s.rect, &s.label, STYLE);
                if let Some(v) = s.value {
                    w = w.value(v, 0.0..=300.0);
                }
                let out = w.show(ui);
                s.delta += out.delta;
                s.stopped |= out.response.drag_stopped();
                s.last = Some(out.response);
            });
        },
        state,
    )
}

fn take(h: &mut Harness<'_, Handle>) -> f32 {
    std::mem::take(&mut h.state_mut().delta)
}

#[test]
fn pointer_drag_reports_per_frame_deltas() {
    let mut h = handle(SplitAxis::Horizontal, rect(100.0, 20.0, 4.0, 200.0));
    h.step();
    let c = pos2(102.0, 100.0);
    h.hover_at(c);
    h.step();
    h.drag_at(c);
    h.step();
    take(&mut h);
    h.hover_at(c + vec2(12.0, 3.0));
    h.step();
    assert_eq!(take(&mut h), 12.0);
    h.hover_at(c + vec2(16.0, -9.0));
    h.step();
    assert_eq!(take(&mut h), 4.0);
    h.drop_at(c + vec2(16.0, -9.0));
    h.step();
    assert!(h.state().stopped);
    assert_eq!(take(&mut h), 0.0);
    assert_eq!(h.state().rect, rect(100.0, 20.0, 4.0, 200.0));
}

#[test]
fn focused_arrows_move_by_the_step_and_paint_the_focus_fill() {
    for (axis, back, forward, r) in [
        (SplitAxis::Horizontal, Key::ArrowLeft, Key::ArrowRight, rect(100.0, 20.0, 4.0, 200.0)),
        (SplitAxis::Vertical, Key::ArrowUp, Key::ArrowDown, rect(20.0, 100.0, 300.0, 4.0)),
    ] {
        let mut h = handle(axis, r);
        h.step();
        h.ctx.memory_mut(|m| m.request_focus(Id::new("divider")));
        h.step();
        take(&mut h);
        h.key_press(forward);
        h.step();
        assert_eq!(take(&mut h), 8.0, "{axis:?}");
        h.key_press(back);
        h.step();
        assert_eq!(take(&mut h), -8.0, "{axis:?}");
        let painted =
            h.output().shapes.iter().any(|clipped| matches!(&clipped.shape, Shape::Rect(shape) if shape.rect == r && shape.fill == STYLE.focused));
        assert!(painted, "{axis:?}: no focus fill at the handle rect");
    }
}

#[test]
fn unfocused_disabled_and_malformed_handles_report_nothing() {
    let mut h = handle(SplitAxis::Horizontal, rect(100.0, 20.0, 4.0, 200.0));
    h.step();
    take(&mut h);
    h.key_press(Key::ArrowRight);
    h.step();
    assert_eq!(take(&mut h), 0.0);

    h.state_mut().enabled = false;
    h.step();
    h.ctx.memory_mut(|m| m.request_focus(Id::new("divider")));
    let c = pos2(102.0, 100.0);
    h.hover_at(c);
    h.step();
    h.drag_at(c);
    h.step();
    h.hover_at(c + vec2(30.0, 0.0));
    h.key_press(Key::ArrowRight);
    h.step();
    h.drop_at(c + vec2(30.0, 0.0));
    h.step();
    assert_eq!(take(&mut h), 0.0);
    assert!(!h.state().last.as_ref().unwrap().enabled());
    assert!(h.get_by_label("Resize panes").accesskit_node().is_disabled());

    let mut h = handle(SplitAxis::Horizontal, Rect::from_min_size(pos2(f32::NAN, 0.0), vec2(4.0, 100.0)));
    h.step();
    h.key_press(Key::ArrowRight);
    h.step();
    assert_eq!(take(&mut h), 0.0);
}

fn request(h: &Harness<'_, Handle>, action: Action, data: Option<ActionData>) {
    let target_node = Id::new("divider").accesskit_id();
    h.event(Event::AccessKitActionRequest(ActionRequest { action, target_tree: TreeId::ROOT, target_node, data }));
}

#[test]
fn accessibility_node_and_actions() {
    let mut h = handle(SplitAxis::Horizontal, rect(100.0, 20.0, 4.0, 200.0));
    h.state_mut().value = Some(100.0);
    h.step();
    let id = h.state().last.as_ref().unwrap().id;
    for label in ["Größe ändern", "調整", "Resize panes"] {
        h.state_mut().label = label.into();
        h.step();
        assert_eq!(h.state().last.as_ref().unwrap().id, id);
        let node = h.get_by_label(label);
        let node = node.accesskit_node();
        assert_eq!(node.role(), Role::Splitter);
        assert_eq!(node.orientation(), Some(Orientation::Vertical));
        assert_eq!((node.numeric_value(), node.min_numeric_value(), node.max_numeric_value()), (Some(100.0), Some(0.0), Some(300.0)));
        for action in [Action::Increment, Action::Decrement, Action::SetValue] {
            assert!(node.data().supports_action(action), "{action:?}");
        }
    }
    take(&mut h);
    for (action, data, want) in [
        (Action::Increment, None, 8.0),
        (Action::Decrement, None, -8.0),
        (Action::SetValue, Some(ActionData::NumericValue(250.0)), 150.0),
        (Action::SetValue, Some(ActionData::NumericValue(f64::NAN)), 0.0),
    ] {
        request(&h, action, data);
        h.step();
        assert_eq!(take(&mut h), want, "{action:?}");
    }
}
