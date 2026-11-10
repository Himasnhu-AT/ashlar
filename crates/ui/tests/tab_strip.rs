#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The standalone tab strip: one Tab stop, arrows, close, overflow, reorder, automation, AccessKit
//! and the selection indicator.

use ashlar_ui::automation;
use ashlar_ui::tabs::{TabStrip, reorder};
use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

#[derive(Default)]
struct State {
    tabs: Vec<String>,
    selected: usize,
    closable: bool,
    reorderable: bool,
    activated: Vec<usize>,
    closed: Vec<usize>,
    moved: Vec<(usize, usize)>,
    overflow_open: bool,
    after: usize,
}

fn harness(labels: &[&str], width: f32) -> Harness<'static, State> {
    let state = State { tabs: labels.iter().map(|s| s.to_string()).collect(), ..Default::default() };
    Harness::builder().with_size(vec2(width, 120.0)).build_ui_state(
        |ui, s: &mut State| {
            let out = TabStrip::new("t", &s.tabs, &mut s.selected).label("Documents").closable(s.closable).reorderable(s.reorderable).show(ui);
            s.activated.extend(out.selected);
            s.closed.extend(out.closed);
            s.overflow_open = out.overflow_open;
            if let Some(m) = out.moved {
                s.moved.push(m);
                reorder(&mut s.tabs, &mut s.selected, m);
            }
            if ui.button("After").clicked() {
                s.after += 1;
            }
        },
        state,
    )
}

fn point(h: &Harness<'_, State>, id: &str) -> Pos2 {
    automation::click_point(&h.ctx, id).unwrap_or_else(|| panic!("`{id}` not registered"))
}

fn focused_label(h: &Harness<'_, State>) -> Option<String> {
    h.ctx.memory(|m| m.focused())?;
    h.get_all_by_role(Role::Tab).find(|n| n.accesskit_node().is_focused()).and_then(|n| n.accesskit_node().label())
}

fn press(h: &mut Harness<'_, State>, button: PointerButton, at: Pos2, pressed: bool) {
    h.event(Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE });
    h.step();
}

#[test]
fn accesskit_tab_list_with_positioned_tabs() {
    let mut h = harness(&["Alpha", "Beta", "Gamma"], 400.0);
    h.state_mut().selected = 1;
    h.run_steps(2);
    let list = h.get_by_role(Role::TabList);
    assert_eq!(list.accesskit_node().label().as_deref(), Some("Documents"));
    let tabs: Vec<_> = list.get_all_by_role(Role::Tab).collect();
    assert_eq!(tabs.len(), 3);
    let info: Vec<_> =
        tabs.iter().map(|n| (n.accesskit_node().is_selected(), n.accesskit_node().position_in_set(), n.accesskit_node().size_of_set())).collect();
    assert_eq!(info, vec![(Some(false), Some(1), Some(3)), (Some(true), Some(2), Some(3)), (Some(false), Some(3), Some(3))]);

    let snap = automation::inspect(&h.ctx);
    let tab = |n: usize| snap.elements.iter().find(|e| e.id == format!("t.tab.{n}")).unwrap();
    assert_eq!((tab(0).label.as_str(), tab(0).role, tab(0).selected), ("Alpha", automation::Role::Tab, Some(false)));
    assert_eq!(tab(1).selected, Some(true));
    assert!(snap.elements.iter().any(|e| e.id == "t"));
}

#[test]
fn one_tab_stop_and_arrows_rove() {
    let mut h = harness(&["Alpha", "Beta", "Gamma", "Delta"], 400.0);
    h.state_mut().selected = 1;
    h.run_steps(2);
    // Tab lands on the selected tab, the next Tab leaves the strip
    h.key_press(Key::Tab);
    h.run_steps(2);
    assert_eq!(focused_label(&h).as_deref(), Some("Beta"));
    h.key_press(Key::Tab);
    h.run_steps(2);
    assert_eq!(focused_label(&h), None, "a second Tab stop inside the strip");

    h.key_press_modifiers(Modifiers::SHIFT, Key::Tab);
    h.run_steps(2);
    assert_eq!(focused_label(&h).as_deref(), Some("Beta"));
    for (key, selected, label) in
        [(Key::ArrowRight, 2, "Gamma"), (Key::End, 3, "Delta"), (Key::ArrowRight, 0, "Alpha"), (Key::ArrowLeft, 3, "Delta"), (Key::Home, 0, "Alpha")]
    {
        h.key_press(key);
        h.run_steps(2);
        assert_eq!(h.state().selected, selected, "{key:?}");
        assert_eq!(focused_label(&h).as_deref(), Some(label), "{key:?}");
    }
    assert_eq!(h.state().activated, vec![2, 3, 0, 3, 0]);
}

#[test]
fn click_selects() {
    let mut h = harness(&["Alpha", "Beta"], 400.0);
    h.run_steps(2);
    h.get_by_label("Beta").click();
    h.run_steps(2);
    assert_eq!(h.state().selected, 1);
}

#[test]
fn command_w_middle_click_and_close_button_close() {
    let mut h = harness(&["Alpha", "Beta", "Gamma"], 400.0);
    h.state_mut().closable = true;
    h.run_steps(2);
    h.key_press(Key::Tab);
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::W);
    h.run_steps(2);
    assert_eq!(h.state().closed, vec![0]);

    let at = point(&h, "t.tab.2");
    press(&mut h, PointerButton::Middle, at, true);
    press(&mut h, PointerButton::Middle, at, false);
    h.run_steps(1);
    assert_eq!(h.state().closed, vec![0, 2]);

    let at = point(&h, "t.tab.1.close");
    h.hover_at(at);
    h.step();
    press(&mut h, PointerButton::Primary, at, true);
    press(&mut h, PointerButton::Primary, at, false);
    h.run_steps(1);
    assert_eq!(h.state().closed, vec![0, 2, 1]);
    assert_eq!(h.state().selected, 0, "closing never selects");
    assert!(h.state().tabs.len() == 3, "the strip only reports");
}

#[test]
fn overflow_menu_lists_hidden_tabs() {
    let labels: Vec<String> = (0..30).map(|i| format!("Document {i}")).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let mut h = harness(&refs, 400.0);
    h.state_mut().selected = 25;
    h.run_steps(2);
    let shown: Vec<_> = h.get_all_by_role(Role::Tab).filter_map(|n| n.accesskit_node().label()).collect();
    assert!(shown.len() < 30 && shown.contains(&"Document 25".to_owned()), "{shown:?}");
    assert!(!h.state().overflow_open);

    let at = point(&h, "t.overflow");
    h.hover_at(at);
    h.step();
    press(&mut h, PointerButton::Primary, at, true);
    press(&mut h, PointerButton::Primary, at, false);
    h.run_steps(2);
    assert!(h.state().overflow_open);
    let found = automation::query(&h.ctx, "t.overflow.");
    let rows: Vec<String> = found.iter().map(|e| e.label.clone()).collect();
    let hidden: Vec<String> = labels.iter().filter(|l| !shown.contains(l)).cloned().collect();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|r| hidden.contains(r)), "{rows:?} vs {hidden:?}");

    let pick = found.last().unwrap().clone();
    let index: usize = pick.id.rsplit('.').next().unwrap().parse().unwrap();
    assert!(index != 25);
    h.hover_at(pick.center());
    h.step();
    press(&mut h, PointerButton::Primary, pick.center(), true);
    press(&mut h, PointerButton::Primary, pick.center(), false);
    h.run_steps(3);
    assert_eq!(h.state().selected, index);
    assert!(!h.state().overflow_open);
    let label = format!("Document {index}");
    assert!(h.get_all_by_role(Role::Tab).any(|n| n.accesskit_node().label().as_deref() == Some(label.as_str())));
}

#[test]
fn drag_and_keys_reorder() {
    let mut h = harness(&["Alpha", "Beta", "Gamma"], 400.0);
    h.state_mut().reorderable = true;
    h.run_steps(2);
    let from = point(&h, "t.tab.0");
    let to = point(&h, "t.tab.2") + vec2(30.0, 0.0);
    h.hover_at(from);
    h.step();
    press(&mut h, PointerButton::Primary, from, true);
    for k in 1..=10 {
        h.hover_at(from + (to - from) * (k as f32 / 10.0));
        h.step();
    }
    press(&mut h, PointerButton::Primary, to, false);
    h.run_steps(2);
    assert_eq!(h.state().moved, vec![(0, 2)]);
    assert_eq!(h.state().tabs, ["Beta", "Gamma", "Alpha"]);
    assert_eq!(h.state().selected, 2, "the selection follows the moved tab");

    // keyboard: ⌘⇧← moves the focused tab and keeps focus on it
    h.get_by_label("Alpha").focus();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(h.state().moved, vec![(0, 2), (2, 1)]);
    assert_eq!(h.state().tabs, ["Beta", "Alpha", "Gamma"]);
    assert_eq!(focused_label(&h).as_deref(), Some("Alpha"));
}

#[test]
fn indicator_settles_and_snaps_under_reduced_motion() {
    let mut h = harness(&["Alpha", "Beta", "Gamma"], 400.0);
    h.run_steps(2);
    assert_eq!(ashlar_ui::motion::active(&h.ctx), 0, "first show is at rest");
    h.get_by_label("Gamma").click();
    h.run_steps(2);
    assert!(ashlar_ui::motion::active(&h.ctx) > 0, "the indicator slides");
    // it settles and then asks for no more frames
    let steps = h.run();
    assert!(steps < 16, "{steps}");
    assert_eq!(ashlar_ui::motion::active(&h.ctx), 0);
    h.run();
    assert_eq!(ashlar_ui::motion::active(&h.ctx), 0);

    ashlar_ui::motion::set_prefs(&h.ctx, ashlar_motion::MotionPrefs::REDUCED);
    h.run();
    h.get_by_label("Alpha").click();
    h.run_steps(2);
    assert_eq!(ashlar_ui::motion::active(&h.ctx), 0, "reduced motion jumps");
}

#[test]
fn hostile_inputs_stay_inert() {
    let mut h = harness(&[], 400.0);
    h.state_mut().selected = 7;
    h.run_steps(2);
    assert_eq!(h.query_all_by_role(Role::Tab).count(), 0);
    let mut h = harness(&["Only"], 0.0);
    h.state_mut().selected = usize::MAX;
    h.run_steps(2);
    assert_eq!(h.state().selected, 0);
}
