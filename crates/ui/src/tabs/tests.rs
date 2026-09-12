use egui::accesskit::Role;
use egui::{Color32, Key, Shape, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;

fn total(fit: &TabFit) -> f32 {
    fit.shown.iter().map(|&(_, w)| w).sum::<f32>() + fit.overflow_width
}

fn shown(fit: &TabFit) -> Vec<usize> {
    fit.shown.iter().map(|&(i, _)| i).collect()
}

#[test]
fn natural_widths_when_they_fit_and_the_widest_shrinks_first() {
    let f = fit(&[60.0, 70.0, 80.0], 0, 400.0, 40.0, 22.0);
    assert_eq!(f.shown, vec![(0, 60.0), (1, 70.0), (2, 80.0)]);
    assert!(f.overflow.is_empty());

    let f = fit(&[50.0, 70.0, 120.0], 1, 200.0, 40.0, 22.0);
    assert!(f.overflow.is_empty());
    assert_eq!((f.shown[0], f.shown[1]), ((0, 50.0), (1, 70.0)));
    assert!((f.shown[2].1 - 80.0).abs() < 0.01, "{:?}", f.shown);
    assert!((total(&f) - 200.0).abs() < 0.01);
}

#[test]
fn the_selected_tab_is_always_shown() {
    let natural = [80.0, 90.0, 85.0, 95.0];
    for selected in [0, 1, 2, 3, 4, usize::MAX] {
        for available in (0..=500).step_by(5) {
            let available = available as f32;
            let f = fit(&natural, selected, available, 40.0, 22.0);
            let selected = selected.min(3);
            assert!(shown(&f).contains(&selected) && !f.overflow.contains(&selected), "{selected} {available} {f:?}");
            assert!(shown(&f).windows(2).all(|w| w[0] < w[1]) && f.overflow.windows(2).all(|w| w[0] < w[1]));
            let mut all = shown(&f);
            all.extend(&f.overflow);
            all.sort_unstable();
            assert_eq!(all, vec![0, 1, 2, 3]);
            assert!(total(&f) <= available + 0.01, "{available} {f:?}");
        }
    }
}

#[test]
fn a_single_tab_takes_whatever_there_is() {
    assert_eq!(fit(&[80.0], 99, 10.0, 40.0, 22.0), TabFit { shown: vec![(0, 10.0)], overflow: vec![], overflow_width: 0.0 });
}

#[test]
fn hostile_dimensions_stay_finite_and_bounded() {
    let hostile = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -5.0, 0.0, f32::MAX, 1e20, 120.0];
    let inputs: [&[f32]; 4] = [&[], &[60.0, f32::NAN, 70.0], &[f32::MAX, -5.0, f32::INFINITY, 30.0], &[50.0; 9]];
    for natural in inputs {
        for &available in &hostile {
            for &minimum in &hostile {
                for &overflow in &hostile {
                    let f = fit(natural, 1, available, minimum, overflow);
                    assert!(f.shown.iter().all(|&(_, w)| w.is_finite() && w >= 0.0), "{f:?}");
                    assert!(f.overflow_width.is_finite() && f.overflow_width >= 0.0);
                    assert!(total(&f) <= clean(available) + 0.125, "{natural:?} {available} {minimum} {overflow} {f:?}");
                    assert_eq!(f.shown.len() + f.overflow.len(), natural.len());
                }
            }
        }
    }
}

#[test]
fn ten_thousand_tabs_are_fast() {
    let natural = vec![60.0; 10_000];
    let start = std::time::Instant::now();
    let f = fit(&natural, 9_999, 240.0, 40.0, 22.0);
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    let last = f.shown.iter().find(|&&(i, _)| i == 9_999).unwrap();
    assert!(last.1 > 0.0);
    assert_eq!(f.shown.len() + f.overflow.len(), 10_000);
    assert!(total(&f) <= 240.0 + 0.01);
}

const FOCUS: Color32 = Color32::from_rgb(220, 38, 38);

#[derive(Default)]
struct Strip {
    labels: Vec<String>,
    selected: usize,
    enabled: bool,
    drag: bool,
    clicks: Vec<usize>,
    drag_started: bool,
    dragged: bool,
    drag_stopped: bool,
    ids: Vec<Id>,
    rects: Vec<Rect>,
}

fn strip(labels: &[&str]) -> Harness<'static, Strip> {
    let state = Strip { labels: labels.iter().map(|s| s.to_string()).collect(), enabled: true, ..Default::default() };
    Harness::builder().with_size(vec2(400.0, 120.0)).build_ui_state(
        |ui, s: &mut Strip| {
            s.ids.clear();
            s.rects.clear();
            let enabled = s.enabled;
            ui.add_enabled_ui(enabled, |ui| {
                for (i, label) in s.labels.clone().iter().enumerate() {
                    let rect = Rect::from_min_size(pos2(10.0 + 80.0 * i as f32, 10.0), vec2(76.0, 26.0));
                    let mut tab = Tab::new(Id::new(("tab", i)), label, i == s.selected).focus_stroke(Stroke::new(1.0, FOCUS));
                    if s.drag {
                        tab = tab.sense(Sense::click_and_drag());
                    }
                    let response = tab.show_at(ui, rect, |ui, r| {
                        ui.painter().rect_filled(r.rect, 0.0, Color32::from_gray(if r.hovered() { 90 } else { 60 }));
                    });
                    if response.clicked() {
                        s.clicks.push(i);
                    }
                    s.drag_started |= response.drag_started();
                    s.dragged |= response.dragged();
                    s.drag_stopped |= response.drag_stopped();
                    s.ids.push(response.id);
                    s.rects.push(rect);
                }
            });
        },
        state,
    )
}

#[test]
fn ids_come_from_the_caller_and_nodes_are_tabs() {
    let mut h = strip(&["Layers", "Layers"]);
    h.step();
    let ids = h.state().ids.clone();
    assert_ne!(ids[0], ids[1]);
    h.ctx.memory_mut(|m| m.request_focus(ids[0]));
    h.step();
    h.state_mut().labels[0] = "Ebenen".into();
    h.step();
    assert_eq!(h.state().ids[0], ids[0]);
    assert!(h.ctx.memory(|m| m.has_focus(ids[0])));
    let nodes: Vec<_> = h.get_all_by_role(Role::Tab).collect();
    assert_eq!(nodes.len(), 2);
    let selected: Vec<Option<bool>> = nodes.iter().map(|n| n.accesskit_node().is_selected()).collect();
    assert_eq!(selected, vec![Some(true), Some(false)]);
    assert!(nodes.iter().all(|n| n.accesskit_node().toggled().is_none()));
    assert_eq!(h.get_by_label("Ebenen").accesskit_node().role(), Role::Tab);
}

#[test]
fn keyboard_and_pointer_activate_once() {
    let mut h = strip(&["One", "Two"]);
    h.step();
    let id = h.state().ids[1];
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.step();
    for key in [Key::Enter, Key::Space] {
        h.state_mut().clicks.clear();
        h.key_press(key);
        h.step();
        h.step();
        assert_eq!(h.state().clicks, vec![1], "{key:?}");
    }
    h.state_mut().clicks.clear();
    let c = h.state().rects[0].center();
    crate::test_support::click(&mut h, c);
    h.step();
    assert_eq!(h.state().clicks, vec![0]);
    assert_eq!(h.state().selected, 0);
}

#[test]
fn disabled_tabs_ignore_input() {
    let mut h = strip(&["One", "Two"]);
    h.state_mut().enabled = false;
    h.step();
    let id = h.state().ids[1];
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.step();
    h.key_press(Key::Enter);
    h.step();
    let c = h.state().rects[0].center();
    crate::test_support::click(&mut h, c);
    h.step();
    assert!(h.state().clicks.is_empty());
    assert!(h.get_all_by_role(Role::Tab).all(|n| n.accesskit_node().is_disabled()));
}

#[test]
fn focus_outline_and_drags() {
    let mut h = strip(&["One", "Two"]);
    h.state_mut().drag = true;
    h.step();
    let id = h.state().ids[0];
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.step();
    let inner = h.state().rects[0].shrink(2.0);
    let outlined = h.output().shapes.iter().any(|c| matches!(&c.shape, Shape::Rect(r) if r.rect == inner && r.stroke.color == FOCUS));
    assert!(outlined, "no focus outline at rect.shrink(2)");

    let c = h.state().rects[1].center();
    h.hover_at(c);
    h.step();
    h.drag_at(c);
    h.step();
    for dx in [10.0, 30.0, 60.0] {
        h.hover_at(c + vec2(dx, 0.0));
        h.step();
    }
    h.drop_at(c + vec2(60.0, 0.0));
    h.step();
    let s = h.state();
    assert!(s.drag_started && s.dragged && s.drag_stopped);
    assert!(s.clicks.is_empty());
    assert_eq!(s.selected, 0);
}

#[test]
fn invalid_rects_are_inert() {
    let bad = [
        Rect::NOTHING,
        Rect::EVERYTHING,
        Rect::from_min_size(pos2(0.0, 0.0), vec2(f32::NAN, 10.0)),
        Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 10.0)),
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1e20, 10.0)),
        Rect::from_min_max(pos2(50.0, 50.0), pos2(10.0, 10.0)),
    ];
    for rect in bad {
        let mut painted = false;
        let mut senses = false;
        let mut finite = true;
        let mut h = Harness::builder().with_size(vec2(200.0, 100.0)).build_ui(|ui| {
            let r = Tab::new(Id::new("bad"), "Bad", false).show_at(ui, rect, |_, _| painted = true);
            senses |= r.sense.senses_click();
            finite &= r.rect.is_finite();
        });
        h.step();
        crate::test_support::click(&mut h, pos2(5.0, 5.0));
        drop(h);
        assert!(!painted && !senses && finite, "{rect:?}");
    }
}
