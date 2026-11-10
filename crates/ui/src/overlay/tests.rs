//! Behaviour tests for the overlays: keyboard, pointer, placement, IME, automation, AccessKit and
//! idle frames.

use ashlar_command::menu::{Item, MenuNode, MenuSpec, Submenu};
use egui::accesskit::Role as AkRole;
use egui::{Event, Id, ImeEvent, Key, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use crate::automation::{self, Role};
use crate::commands::menu;
use crate::test_support::click;
use crate::wake::{Reason, Scheduler};

const FONTS: [&str; 5] = ["Inter", "Iosevka", "JetBrains Mono", "Fira Code", "Source Serif"];

struct S {
    current: usize,
    combo: ComboboxState,
    chosen: Vec<usize>,
    /// Push the controls to the bottom of the window.
    bottom: bool,
    popover: bool,
    help_rect: Rect,
    context_rect: Rect,
    context: Vec<String>,
    anchor: Option<Id>,
}

impl Default for S {
    fn default() -> Self {
        Self {
            current: 0,
            combo: ComboboxState::new(),
            chosen: Vec::new(),
            bottom: false,
            popover: false,
            help_rect: Rect::NOTHING,
            context_rect: Rect::NOTHING,
            context: Vec::new(),
            anchor: None,
        }
    }
}

fn spec() -> MenuSpec {
    let item = |id: &str, label: &str| MenuNode::Item(Item::new(id, label));
    MenuSpec {
        menus: vec![Submenu {
            label: "Row".into(),
            enabled: true,
            role: None,
            children: vec![item("row.copy", "Copy"), item("row.delete", "Delete")],
        }],
    }
}

fn harness() -> Harness<'static, S> {
    let mut h = Harness::builder().with_size(vec2(500.0, 400.0)).build_ui_state(
        |ui, s: &mut S| {
            if s.bottom {
                ui.add_space(ui.available_height() - 70.0);
            }
            Select::new("size", &FONTS, &mut s.current).label("Font").show(ui);
            if let Some(i) = Combobox::new("font", &FONTS, &mut s.combo).label("Font family").show(ui).chosen {
                s.chosen.push(i);
            }
            let anchor = ui.button("Filters");
            if anchor.clicked() {
                s.popover = !s.popover;
            }
            s.anchor = Some(anchor.id);
            Popover::new("filters").show(&anchor, &mut s.popover, |ui| {
                let _ = ui.button("Only errors");
            });
            s.help_rect = help(ui.label("Sync"), "Upload local changes").rect;
            let row = ui.label("main.rs");
            s.context_rect = row.rect;
            if let Some(id) = menu::context(&row, &spec()) {
                s.context.push(id);
            }
        },
        S::default(),
    );
    automation::install(&h.ctx);
    h.ctx.enable_accesskit();
    h.run_steps(2);
    h
}

fn el(h: &Harness<'_, S>, id: &str) -> Option<automation::Element> {
    automation::find(&h.ctx, id)
}

fn focus(h: &mut Harness<'_, S>, id: Id) {
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.run_steps(2);
}

fn key(h: &mut Harness<'_, S>, k: Key) {
    h.key_press(k);
    h.run_steps(2);
}

fn select_id() -> Id {
    Id::new(("ashlar_ui::select", "size"))
}

fn combo_id() -> Id {
    Id::new(("ashlar_ui::combobox", "font"))
}

fn settle(h: &mut Harness<'_, S>) {
    h.run_steps(40);
}

#[test]
fn select_opens_navigates_chooses_and_escapes_with_the_keyboard() {
    let mut h = harness();
    focus(&mut h, select_id());
    assert!(el(&h, "size.option.0").is_none(), "closed at first");
    key(&mut h, Key::ArrowDown);
    settle(&mut h);
    assert_eq!(el(&h, "size").and_then(|e| e.selected), Some(true), "ArrowDown opens");
    assert!(el(&h, "size.option.4").is_some());
    key(&mut h, Key::ArrowDown);
    key(&mut h, Key::ArrowDown);
    key(&mut h, Key::Enter);
    assert_eq!(h.state().current, 2);
    assert_eq!(el(&h, "size").map(|e| e.value), Some("JetBrains Mono".into()));
    settle(&mut h);
    assert!(el(&h, "size.option.0").is_none(), "Enter closes");
    assert!(h.ctx.memory(|m| m.has_focus(select_id())), "the focus stays on the select");

    // Escape closes without choosing; End/Home move
    key(&mut h, Key::Space);
    key(&mut h, Key::End);
    key(&mut h, Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().current, 2);
    assert_eq!(el(&h, "size").and_then(|e| e.selected), Some(false));
    assert!(h.ctx.memory(|m| m.has_focus(select_id())), "Escape does not drop the focus");
}

#[test]
fn select_opens_and_chooses_with_the_pointer_and_closes_on_outside_clicks() {
    let mut h = harness();
    let Some(button) = automation::click_point(&h.ctx, "size") else { panic!("no select") };
    click(&mut h, button);
    settle(&mut h);
    let Some(option) = automation::click_point(&h.ctx, "size.option.3") else { panic!("no option 3") };
    click(&mut h, option);
    assert_eq!(h.state().current, 3);
    // open again, then click far away
    click(&mut h, button);
    settle(&mut h);
    assert!(el(&h, "size.list").is_some());
    click(&mut h, pos2(480.0, 380.0));
    settle(&mut h);
    assert!(el(&h, "size.list").is_none());
    assert_eq!(h.state().current, 3);
}

#[test]
fn the_list_flips_above_at_the_window_edge() {
    let mut h = harness();
    focus(&mut h, select_id());
    key(&mut h, Key::ArrowDown);
    settle(&mut h);
    let (Some(button), Some(list)) = (el(&h, "size"), el(&h, "size.list")) else { panic!("missing") };
    assert!(list.rect[1] >= button.rect[1] + button.rect[3], "below the button when there is room");
    key(&mut h, Key::Escape);
    settle(&mut h);

    h.state_mut().bottom = true;
    h.run_steps(2);
    focus(&mut h, select_id());
    key(&mut h, Key::ArrowDown);
    settle(&mut h);
    let (Some(button), Some(list)) = (el(&h, "size"), el(&h, "size.list")) else { panic!("missing") };
    assert!(list.rect[1] + list.rect[3] <= button.rect[1] + 0.5, "above the button at the bottom edge: {list:?} {button:?}");
    assert!(list.rect[1] >= 0.0);
}

#[test]
fn place_flips_clamps_and_survives_hostile_input() {
    let screen = Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0));
    let anchor = Rect::from_min_size(pos2(10.0, 20.0), vec2(100.0, 24.0));
    assert_eq!(place(anchor, vec2(120.0, 100.0), screen, 2.0), (pos2(10.0, 46.0), false));
    let low = anchor.translate(vec2(0.0, 250.0));
    let (p, flipped) = place(low, vec2(120.0, 100.0), screen, 2.0);
    assert!(flipped);
    assert_eq!(p.y, low.top() - 2.0 - 100.0);
    // near the right edge it shifts left to stay inside
    let right = anchor.translate(vec2(330.0, 0.0));
    assert_eq!(place(right, vec2(120.0, 100.0), screen, 2.0).0.x, 280.0);
    // too tall for either side: stays below, clamped to the top
    let (p, _) = place(anchor, vec2(50.0, 900.0), screen, 2.0);
    assert_eq!(p.y, 0.0);
    for size in [vec2(f32::NAN, f32::INFINITY), vec2(-5.0, -5.0)] {
        let (p, _) = place(anchor, size, screen, f32::NAN);
        assert!(p.x.is_finite() && p.y.is_finite());
    }
    assert_eq!(place(Rect::NOTHING, vec2(1.0, 1.0), screen, 0.0), (Pos2::ZERO, false));
}

#[test]
fn combobox_filters_fuzzily_and_chooses_with_the_keyboard() {
    let mut h = harness();
    focus(&mut h, combo_id());
    h.event(Event::Text("jbm".into()));
    settle(&mut h);
    assert_eq!(h.state().combo.matches(), &[2], "fuzzy: jbm matches JetBrains Mono");
    assert!(el(&h, "font.option.2").is_some());
    assert!(el(&h, "font.option.0").is_none());
    key(&mut h, Key::Enter);
    assert_eq!(h.state().chosen, vec![2]);
    assert_eq!(h.state().combo.text, "JetBrains Mono");
    assert!(!h.state().combo.is_open());
    // reopening lists everything again, Escape closes without choosing
    key(&mut h, Key::ArrowDown);
    settle(&mut h);
    assert_eq!(h.state().combo.matches().len(), FONTS.len());
    key(&mut h, Key::ArrowDown);
    key(&mut h, Key::Escape);
    assert!(!h.state().combo.is_open());
    assert_eq!(h.state().chosen, vec![2]);
    // nothing matches: the list says so and Enter chooses nothing
    h.event(Event::Text("zzzz".into()));
    settle(&mut h);
    assert!(h.state().combo.matches().is_empty());
    key(&mut h, Key::Enter);
    assert_eq!(h.state().chosen, vec![2]);
}

#[test]
fn ime_composition_never_chooses() {
    let mut h = harness();
    focus(&mut h, combo_id());
    h.event(Event::Text("i".into()));
    settle(&mut h);
    assert!(h.state().combo.is_open());
    h.event(Event::Ime(ImeEvent::Preedit { text: "ｎ".into(), active_range_chars: None }));
    h.run_steps(2);
    assert!(h.state().combo.is_composing());
    key(&mut h, Key::Enter);
    key(&mut h, Key::ArrowDown);
    assert!(h.state().chosen.is_empty(), "Enter mid-composition chooses nothing");
    assert!(h.state().combo.is_open());
    // the order platforms send: the preedit is cleared, then the text committed
    h.event(Event::Ime(ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
    h.event(Event::Ime(ImeEvent::Commit("n".into())));
    h.run_steps(2);
    assert!(!h.state().combo.is_composing());
    assert!(h.state().chosen.is_empty(), "committing text is typing, not choosing");
    assert!(h.state().combo.text.ends_with('n'), "the committed text is in the field");
    // with the composition over, Enter is the combobox's again
    h.state_mut().combo.text = "inter".into();
    h.run_steps(2);
    key(&mut h, Key::Enter);
    assert_eq!(h.state().chosen.len(), 1, "Enter after the composition chooses");
}

#[test]
fn accesskit_roles_and_states() {
    let mut h = harness();
    focus(&mut h, select_id());
    let combos = h.query_all_by_role(AkRole::ComboBox).count();
    assert!(combos >= 2, "the select and the combobox are combo boxes");
    key(&mut h, Key::ArrowDown);
    settle(&mut h);
    assert!(h.get_by_role(AkRole::ListBox).accesskit_node().role() == AkRole::ListBox);
    assert_eq!(h.query_all_by_role(AkRole::ListBoxOption).count(), FONTS.len());
    let first = h.query_all_by_role(AkRole::ListBoxOption).next().map(|n| n.accesskit_node().is_selected());
    assert_eq!(first, Some(Some(true)), "the cursor's option is selected");
    let expanded = h.query_all_by_role(AkRole::ComboBox).any(|n| n.accesskit_node().data().is_expanded() == Some(true));
    assert!(expanded);
    let Some(e) = el(&h, "size.option.1") else { panic!("no option") };
    assert_eq!((e.role, e.label.as_str()), (Role::ListOption, "Iosevka"));
    assert_eq!(el(&h, "size").map(|e| e.role), Some(Role::ComboBox));
    assert_eq!(el(&h, "size.list").map(|e| e.role), Some(Role::ListBox));
}

#[test]
fn help_shows_a_tooltip_after_the_delay_and_describes_the_control() {
    let mut h = harness();
    let center = h.state().help_rect.center();
    h.hover_at(center);
    h.run_steps(2);
    assert!(el(&h, "tooltip").is_none(), "not before the delay");
    h.ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
    h.hover_at(center + vec2(1.0, 0.0));
    h.run_steps(3);
    let Some(tip) = el(&h, "tooltip") else { panic!("no tooltip") };
    assert_eq!((tip.role, tip.label.as_str()), (Role::Tooltip, "Upload local changes"));
    assert_eq!(h.get_by_role(AkRole::Tooltip).accesskit_node().label().as_deref(), Some("Upload local changes"));
    assert_eq!(h.get_by_label("Sync").accesskit_node().description().as_deref(), Some("Upload local changes"));
}

#[test]
fn the_tooltip_delay_comes_from_the_tokens() {
    let mut t = crate::tokens::Tokens::dark();
    t.motion.tooltip_delay = 1.25;
    let mut style = egui::Style::default();
    t.style_into(&mut style);
    assert_eq!(style.interaction.tooltip_delay, 1.25);
    t.motion.tooltip_delay = f32::NAN;
    t.style_into(&mut style);
    assert!(style.interaction.tooltip_delay.is_finite());
}

#[test]
fn context_menu_from_a_spec_returns_the_chosen_command() {
    let mut h = harness();
    let p = h.state().context_rect.center();
    h.hover_at(p);
    h.step();
    h.event(Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.event(Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(4);
    assert_eq!(el(&h, "menu.context").map(|e| e.role), Some(Role::Menu));
    let Some(delete) = automation::click_point(&h.ctx, "menu.item.row.delete") else { panic!("no item") };
    click(&mut h, delete);
    assert_eq!(h.state().context, vec!["row.delete".to_owned()]);
    h.run_steps(4);
    assert!(el(&h, "menu.context").is_none(), "choosing closes the menu");
}

#[test]
fn popover_takes_the_focus_and_gives_it_back_on_escape() {
    let mut h = harness();
    let Some(anchor) = h.state().anchor else { panic!("no anchor") };
    h.ctx.memory_mut(|m| m.request_focus(anchor));
    h.run_steps(2);
    key(&mut h, Key::Enter);
    settle(&mut h);
    assert!(h.state().popover);
    assert!(el(&h, "filters").is_some());
    assert!(!h.ctx.memory(|m| m.has_focus(anchor)), "the focus moved into the popover");
    assert_eq!(h.query_all_by_role(AkRole::Dialog).count(), 1);
    key(&mut h, Key::Escape);
    assert!(!h.state().popover);
    h.run_steps(2);
    assert!(h.ctx.memory(|m| m.has_focus(anchor)), "the focus returned to the anchor");
    settle(&mut h);
    assert!(el(&h, "filters").is_none());
}

fn animation_requests(h: &mut Harness<'_, S>, frames: usize) -> u64 {
    let before = Scheduler::of(&h.ctx).stats().requests_for(Reason::ANIMATION);
    for _ in 0..frames {
        h.step();
    }
    Scheduler::of(&h.ctx).stats().requests_for(Reason::ANIMATION) - before
}

#[test]
fn fades_ask_for_frames_only_while_moving() {
    let mut h = harness();
    assert_eq!(animation_requests(&mut h, 5), 0, "closed and idle");
    focus(&mut h, select_id());
    h.key_press(Key::ArrowDown);
    assert!(animation_requests(&mut h, 3) > 0, "opening fades in");
    settle(&mut h);
    assert_eq!(animation_requests(&mut h, 5), 0, "open and settled");
    h.key_press(Key::Escape);
    assert!(animation_requests(&mut h, 3) > 0, "closing fades out");
    settle(&mut h);
    assert_eq!(animation_requests(&mut h, 5), 0, "closed and settled");
}
