use egui::accesskit::{Role, Toggled};
use egui::epaint::{ClippedPrimitive, Primitive};
use egui::{Id, Key, Pos2, RawInput, Shape, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;

fn visuals(fill: (u8, u8, u8), icon: (u8, u8, u8)) -> ButtonVisuals {
    ButtonVisuals {
        fill: Color32::from_rgb(fill.0, fill.1, fill.2),
        stroke: Stroke::new(1.0, Color32::from_gray(120)),
        icon: Color32::from_rgb(icon.0, icon.1, icon.2),
    }
}

fn style(pressed: bool) -> IconButtonStyle {
    IconButtonStyle {
        normal: visuals((220, 220, 220), (10, 10, 10)),
        hovered: visuals((200, 220, 250), (20, 40, 200)),
        pressed: pressed.then(|| visuals((120, 160, 240), (200, 30, 30))),
        selected: visuals((30, 90, 220), (255, 255, 255)),
        corner_radius: CornerRadius::same(3),
        focus_stroke: Stroke::new(1.0, Color32::from_rgb(220, 38, 38)),
    }
}

struct Bar {
    style: IconButtonStyle,
    labels: [String; 2],
    selected: bool,
    enabled: bool,
    size: Vec2,
    icon: Option<Color32>,
    clicks: usize,
    ids: Vec<Id>,
    rects: Vec<Rect>,
}

fn bar(pressed: bool) -> Harness<'static, Bar> {
    let state = Bar {
        style: style(pressed),
        labels: ["Brush".into(), "Brush".into()],
        selected: false,
        enabled: true,
        size: vec2(28.0, 24.0),
        icon: None,
        clicks: 0,
        ids: Vec::new(),
        rects: Vec::new(),
    };
    Harness::builder().with_size(vec2(200.0, 80.0)).build_ui_state(
        |ui, s: &mut Bar| {
            s.ids.clear();
            s.rects.clear();
            let enabled = s.enabled;
            ui.add_enabled_ui(enabled, |ui| {
                ui.horizontal(|ui| {
                    for i in 0..2 {
                        let mut icon = None;
                        let response =
                            IconButton::new(&s.labels[i], s.size, &s.style).selected(i == 0 && s.selected).show(ui, |_, _, color| icon = Some(color));
                        if i == 0 {
                            s.icon = icon;
                            s.clicks += usize::from(response.clicked());
                        }
                        s.ids.push(response.id);
                        s.rects.push(response.rect);
                    }
                });
            });
        },
        state,
    )
}

fn center(h: &Harness<'_, Bar>) -> Pos2 {
    h.state().rects[0].center()
}

#[test]
fn hover_press_and_release_change_the_tint_and_click_once() {
    let mut h = bar(true);
    h.step();
    let c = center(&h);
    assert_eq!(h.state().icon, Some(h.state().style.normal.icon));
    h.hover_at(c);
    h.step();
    assert_eq!(h.state().icon, Some(h.state().style.hovered.icon));
    h.drag_at(c);
    h.step();
    assert_eq!(h.state().icon, h.state().style.pressed.map(|p| p.icon));
    h.drop_at(c);
    h.step();
    h.step();
    assert_eq!(h.state().clicks, 1);
    assert!(!h.state().selected);
}

#[test]
fn pressing_then_leaving_cancels() {
    for pressed in [true, false] {
        let mut h = bar(pressed);
        h.step();
        let c = center(&h);
        h.hover_at(c);
        h.step();
        h.drag_at(c);
        h.step();
        h.hover_at(c + vec2(120.0, 40.0));
        h.step();
        assert_eq!(h.state().icon, Some(h.state().style.normal.icon), "pressed style: {pressed}");
        h.drop_at(c + vec2(120.0, 40.0));
        h.step();
        assert_eq!(h.state().clicks, 0);
    }
}

#[test]
fn selection_wins_over_press_and_is_reported() {
    let mut h = bar(true);
    h.state_mut().selected = true;
    h.step();
    let c = center(&h);
    h.hover_at(c);
    h.step();
    h.drag_at(c);
    h.step();
    assert_eq!(h.state().icon, Some(h.state().style.selected.icon));
    h.drop_at(c);
    h.step();
    let nodes: Vec<_> = h.get_all_by_role(Role::Button).collect();
    let node = nodes[0].accesskit_node();
    assert_eq!(node.toggled(), Some(Toggled::True));
    assert!(!node.is_disabled());
}

#[test]
fn disabled_buttons_do_nothing() {
    let mut h = bar(true);
    h.state_mut().enabled = false;
    h.step();
    let id = h.state().ids[0];
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.step();
    h.key_press(Key::Enter);
    h.step();
    let c = center(&h);
    crate::test_support::click(&mut h, c);
    h.step();
    assert_eq!(h.state().clicks, 0);
    assert!(h.get_all_by_role(Role::Button).all(|n| n.accesskit_node().is_disabled()));
}

#[test]
fn keys_activate_and_focus_is_outlined() {
    let mut h = bar(true);
    h.step();
    let id = h.state().ids[0];
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.step();
    for key in [Key::Enter, Key::Space] {
        h.state_mut().clicks = 0;
        h.key_press(key);
        h.step();
        h.step();
        assert_eq!(h.state().clicks, 1, "{key:?}");
    }
    let inner = h.state().rects[0].shrink(2.0);
    let focus = h.state().style.focus_stroke;
    let outlined = h.output().shapes.iter().any(|c| matches!(&c.shape, Shape::Rect(r) if r.rect == inner && r.stroke == focus));
    assert!(outlined);
}

#[test]
fn labels_do_not_change_identity() {
    let mut h = bar(true);
    h.step();
    let ids = h.state().ids.clone();
    assert_ne!(ids[0], ids[1], "same label, different buttons");
    h.ctx.memory_mut(|m| m.request_focus(ids[0]));
    h.step();
    h.state_mut().labels[0] = "Pinsel".into();
    h.step();
    assert_eq!(h.state().ids, ids);
    assert!(h.ctx.memory(|m| m.has_focus(ids[0])));
    assert_eq!(h.get_by_label("Pinsel").accesskit_node().role(), Role::Button);
    h.key_press(Key::Tab);
    h.step();
    assert!(h.ctx.memory(|m| m.has_focus(ids[1])));
}

#[test]
fn malformed_sizes_are_inert() {
    for size in [vec2(f32::NAN, 10.0), vec2(f32::INFINITY, 10.0), vec2(-4.0, 10.0), vec2(0.0, 10.0), vec2(f32::MAX, 10.0), vec2(10.0, 5000.0)] {
        let mut h = bar(true);
        h.state_mut().size = size;
        h.step();
        let r = h.state().rects[0];
        assert_eq!(r.size(), Vec2::ZERO, "{size:?}");
        assert_eq!(h.state().icon, None);
        crate::test_support::click(&mut h, r.min);
        assert_eq!(h.state().clicks, 0);
    }
    let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(20.0, 20.0));
    for width in [f32::NAN, f32::INFINITY, -1.0, 0.0] {
        assert_eq!(bounded(Stroke::new(width, Color32::RED), rect), Stroke::NONE);
    }
    assert_eq!(bounded(Stroke::new(f32::MAX, Color32::RED), rect).width, 10.0);
}

#[test]
fn malformed_sizes_do_not_sense_clicks() {
    let ctx = egui::Context::default();
    let mut sense = None;
    let st = style(true);
    crate::test_support::run(&ctx, RawInput::default(), |ui| {
        sense = Some(IconButton::new("x", vec2(f32::NAN, 1.0), &st).show(ui, |_, _, _| {}).sense);
    });
    assert!(!sense.unwrap().senses_click());
}

/// The primitives of one frame that draws `paint` in a fixed 60×40 area.
fn tessellate(pixels_per_point: f32, enabled: bool, paint: impl FnOnce(&mut Ui)) -> Vec<ClippedPrimitive> {
    let ctx = egui::Context::default();
    ctx.set_pixels_per_point(pixels_per_point);
    let input = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(60.0, 40.0))), ..Default::default() };
    let mut paint = Some(paint);
    let output = crate::test_support::run(&ctx, input, |ui| {
        ui.add_enabled_ui(enabled, |ui| {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(pos2(8.0, 6.0), vec2(30.0, 24.0))));
            if let Some(paint) = paint.take() {
                paint(&mut child);
            }
        });
    });
    ctx.tessellate(output.shapes, output.pixels_per_point)
}

fn same(a: &[ClippedPrimitive], b: &[ClippedPrimitive]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(a, b)| {
            a.clip_rect == b.clip_rect
                && match (&a.primitive, &b.primitive) {
                    (Primitive::Mesh(a), Primitive::Mesh(b)) => a == b,
                    _ => false,
                }
        })
}

#[test]
fn single_shape_frames_tessellate_like_one_rect() {
    let st = style(true);
    let size = vec2(30.0, 24.0);
    let icon = |ui: &Ui, r: Rect, c: Color32| {
        ui.painter().line_segment([r.left_top() + vec2(8.0, 6.0), r.right_bottom() - vec2(8.0, 6.0)], Stroke::new(1.0, c));
    };
    for scale in [1.0, 1.5, 2.0] {
        for enabled in [true, false] {
            let component = tessellate(scale, enabled, |ui| {
                IconButton::new("Brush", size, &st).single_shape_frame().show(ui, icon);
            });
            let reference = tessellate(scale, enabled, |ui| {
                let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                let v = st.normal;
                ui.painter().rect(rect, st.corner_radius, v.fill, v.stroke, StrokeKind::Inside);
                icon(ui, rect, v.icon);
            });
            assert!(same(&component, &reference), "scale {scale}, enabled {enabled}");
            let split = tessellate(scale, enabled, |ui| {
                IconButton::new("Brush", size, &st).show(ui, icon);
            });
            assert!(!same(&split, &reference), "the default frame should be two shapes (scale {scale})");
        }
    }
}
