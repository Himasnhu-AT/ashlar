//! Behaviour tests for the controls kit: keyboard, pointer, automation and AccessKit.

use ashlar_command::shortcut::{Chord, Platform};
use egui::accesskit::{Role as AkRole, Toggled};
use egui::{Id, Key, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use crate::automation::{self, Role};
use crate::wake::{Reason, Scheduler};

#[derive(Default)]
struct Kit {
    clicks: usize,
    enabled: bool,
    loading: bool,
    label: String,
    on: bool,
    switch_changes: usize,
    segment: usize,
    radio: usize,
    choice_changes: usize,
    horizontal: bool,
    fraction: Option<f32>,
    reduced: bool,
    hidden: bool,
    before: Option<Id>,
    after: Option<Id>,
    button: Option<Id>,
    segmented: Option<Id>,
    radios: Option<Id>,
    switch: Option<Id>,
}

fn kit() -> Harness<'static, Kit> {
    let state = Kit { enabled: true, label: "Export".into(), segment: 1, fraction: Some(0.4), ..Default::default() };
    let mut h = Harness::builder().with_size(vec2(600.0, 400.0)).build_ui_state(
        |ui, s: &mut Kit| {
            s.before = Some(Button::new("kit.before", "Before").show(ui).id);
            let r = Button::new("kit.export", &s.label)
                .variant(Variant::Primary)
                .enabled(s.enabled)
                .loading(s.loading)
                .reduced_motion(s.reduced)
                .show(ui);
            s.button = Some(r.id);
            s.clicks += usize::from(r.clicked());
            let r = Switch::new("kit.sync", "Sync", &mut s.on).show(ui);
            s.switch = Some(r.id);
            s.switch_changes += usize::from(r.changed());
            let r = Segmented::new("kit.view", "View", &["List", "Grid", "Columns"], &mut s.segment).show(ui);
            s.segmented = Some(r.id);
            s.choice_changes += usize::from(r.changed());
            let r = RadioGroup::new("kit.quality", "Quality", &["Draft", "Good", "Best"], &mut s.radio).horizontal(s.horizontal).show(ui);
            s.radios = Some(r.id);
            s.choice_changes += usize::from(r.changed());
            s.after = Some(Button::new("kit.after", "After").show(ui).id);
            Badge::new("kit.badge", "3").tone(Tone::Accent).show(ui);
            Keycap::chord("kit.keys", Chord::parse("Mod+Shift+P").unwrap(), Platform::Other).show(ui);
            Progress::bar("kit.progress", "Exporting", s.fraction).reduced_motion(s.reduced).show(ui);
            Progress::circle("kit.circle", "Uploading", s.fraction.map(|f| f / 2.0)).reduced_motion(s.reduced).show(ui);
            if s.hidden {
                // scrolled out of view: clipped away entirely
                let clip = egui::Rect::from_min_size(egui::pos2(-100.0, -100.0), vec2(1.0, 1.0));
                ui.scope(|ui| {
                    ui.set_clip_rect(clip);
                    Spinner::new("kit.spinner", "Loading").reduced_motion(s.reduced).show(ui);
                });
            } else {
                Spinner::new("kit.spinner", "Loading").reduced_motion(s.reduced).show(ui);
            }
        },
        state,
    );
    automation::install(&h.ctx);
    h.step();
    h.step();
    h
}

fn focus(h: &mut Harness<'_, Kit>, id: Option<Id>) {
    let id = id.unwrap();
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.step();
    h.step();
}

fn focused(h: &Harness<'_, Kit>) -> Option<Id> {
    h.ctx.memory(|m| m.focused())
}

fn press(h: &mut Harness<'_, Kit>, key: Key) {
    h.key_press(key);
    h.step();
    h.step();
}

fn click_id(h: &mut Harness<'_, Kit>, id: &str) {
    let p = automation::find(&h.ctx, id).unwrap_or_else(|| panic!("{id} not registered")).center();
    crate::test_support::click(h, p);
    h.step();
}

#[test]
fn button_clicks_by_pointer_and_keyboard_and_reports_itself() {
    let mut h = kit();
    click_id(&mut h, "kit.export");
    assert_eq!(h.state().clicks, 1);
    let target = h.state().button;
    focus(&mut h, target);
    for key in [Key::Enter, Key::Space] {
        press(&mut h, key);
    }
    assert_eq!(h.state().clicks, 3);
    let e = automation::find(&h.ctx, "kit.export").unwrap();
    assert_eq!((e.role, e.label.as_str(), e.enabled), (Role::Button, "Export", true));
    let node = h.get_by_label("Export");
    assert_eq!(node.accesskit_node().role(), AkRole::Button);
    assert!(!node.accesskit_node().is_disabled());
    // a new label keeps the identity (and the focus)
    let id = h.state().button;
    h.state_mut().label = "Exportieren".into();
    h.step();
    assert_eq!(h.state().button, id);
    assert_eq!(focused(&h), id);
    assert_eq!(h.get_by_label("Exportieren").accesskit_node().role(), AkRole::Button);
}

#[test]
fn disabled_and_loading_buttons_ignore_input() {
    for (enabled, loading) in [(false, false), (true, true)] {
        let mut h = kit();
        h.state_mut().enabled = enabled;
        h.state_mut().loading = loading;
        h.state_mut().reduced = true;
        h.step();
        click_id(&mut h, "kit.export");
        let target = h.state().button;
        focus(&mut h, target);
        press(&mut h, Key::Enter);
        assert_eq!(h.state().clicks, 0, "enabled {enabled} loading {loading}");
        assert!(!automation::find(&h.ctx, "kit.export").unwrap().enabled);
        let node = h.get_by_label("Export");
        assert_eq!(node.accesskit_node().is_disabled(), !enabled);
        assert_eq!(node.accesskit_node().is_busy(), loading);
    }
}

#[test]
fn switch_toggles_by_click_space_and_enter() {
    let mut h = kit();
    click_id(&mut h, "kit.sync");
    assert!(h.state().on);
    let target = h.state().switch;
    focus(&mut h, target);
    press(&mut h, Key::Space);
    assert!(!h.state().on);
    press(&mut h, Key::Enter);
    assert!(h.state().on);
    assert_eq!(h.state().switch_changes, 3);
    let e = automation::find(&h.ctx, "kit.sync").unwrap();
    assert_eq!((e.role, e.selected, e.value.as_str()), (Role::Switch, Some(true), "on"));
    let node = h.get_by_label("Sync");
    assert_eq!(node.accesskit_node().role(), AkRole::Switch);
    assert_eq!(node.accesskit_node().toggled(), Some(Toggled::True));
}

#[test]
fn a_disabled_switch_does_not_toggle() {
    let mut h = Harness::builder().with_size(vec2(200.0, 60.0)).build_ui_state(
        |ui, on: &mut bool| {
            ui.add_enabled_ui(false, |ui| Switch::new("off.sync", "Sync", on).show(ui));
        },
        false,
    );
    automation::install(&h.ctx);
    h.step();
    h.step();
    let e = automation::find(&h.ctx, "off.sync").unwrap();
    assert!(!e.enabled);
    crate::test_support::click(&mut h, e.center());
    assert!(!*h.state());
    assert!(h.get_by_label("Sync").accesskit_node().is_disabled());
}

#[test]
fn segmented_is_one_tab_stop_moved_by_arrows() {
    let mut h = kit();
    let seg = h.state().segmented.unwrap();
    let target = h.state().before;
    focus(&mut h, target);
    press(&mut h, Key::Tab);
    // the next Tab stop after "Before" is the export button, then the switch, then the chosen segment
    press(&mut h, Key::Tab);
    press(&mut h, Key::Tab);
    assert_eq!(focused(&h), Some(seg.with(1)), "Tab lands on the chosen segment");
    press(&mut h, Key::ArrowRight);
    assert_eq!(h.state().segment, 2);
    assert_eq!(focused(&h), Some(seg.with(2)));
    press(&mut h, Key::ArrowRight);
    assert_eq!(h.state().segment, 0, "wraps around");
    press(&mut h, Key::End);
    assert_eq!(h.state().segment, 2);
    press(&mut h, Key::Home);
    assert_eq!(h.state().segment, 0);
    press(&mut h, Key::ArrowLeft);
    assert_eq!(h.state().segment, 2);
    assert_eq!(focused(&h), Some(seg.with(2)));
    // one more Tab leaves the group for the radio group's chosen option
    press(&mut h, Key::Tab);
    assert_eq!(focused(&h), Some(h.state().radios.unwrap().with(0)));
    assert_eq!(h.state().choice_changes, 5);

    let group = automation::find(&h.ctx, "kit.view").unwrap();
    assert_eq!((group.role, group.value.as_str()), (Role::RadioGroup, "Columns"));
    assert_eq!(automation::find(&h.ctx, "kit.view.option.2").and_then(|e| e.selected), Some(true));
    assert_eq!(automation::find(&h.ctx, "kit.view.option.0").map(|e| (e.role, e.selected)), Some((Role::Radio, Some(false))));
    let group = h.get_by_label("View");
    assert_eq!(group.accesskit_node().role(), AkRole::RadioGroup);
    let options: Vec<_> = group.children_recursive().filter(|n| n.accesskit_node().role() == AkRole::RadioButton).collect();
    assert_eq!(options.len(), 3, "options are children of the group node");
    let columns = h.get_by_label("Columns").accesskit_node();
    assert_eq!(columns.toggled(), Some(Toggled::True));
    assert_eq!(columns.position_in_set(), Some(3));
    assert_eq!(columns.size_of_set(), Some(3));
}

#[test]
fn clicks_pick_options() {
    let mut h = kit();
    click_id(&mut h, "kit.view.option.0");
    assert_eq!(h.state().segment, 0);
    click_id(&mut h, "kit.quality.option.2");
    assert_eq!(h.state().radio, 2);
    // clicking the chosen one again is not a change
    click_id(&mut h, "kit.quality.option.2");
    assert_eq!(h.state().choice_changes, 2);
}

#[test]
fn radio_group_moves_with_up_and_down_in_either_layout() {
    for horizontal in [false, true] {
        let mut h = kit();
        h.state_mut().horizontal = horizontal;
        h.step();
        let radios = h.state().radios.unwrap();
        focus(&mut h, Some(radios.with(0)));
        press(&mut h, Key::ArrowDown);
        assert_eq!(h.state().radio, 1);
        press(&mut h, Key::ArrowDown);
        press(&mut h, Key::ArrowDown);
        assert_eq!(h.state().radio, 0, "wraps");
        press(&mut h, Key::ArrowUp);
        assert_eq!(h.state().radio, 2);
        assert_eq!(focused(&h), Some(radios.with(2)));
        // Shift+Tab goes back to the segmented control's chosen option
        h.key_press_modifiers(egui::Modifiers::SHIFT, Key::Tab);
        h.step();
        h.step();
        assert_eq!(focused(&h), Some(h.state().segmented.unwrap().with(1)), "horizontal {horizontal}");
        let rects: Vec<_> = (0..3).map(|i| automation::find(&h.ctx, &format!("kit.quality.option.{i}")).unwrap().egui_rect()).collect();
        if horizontal {
            assert!(rects[0].right() < rects[1].left() && rects[0].top() == rects[1].top());
        } else {
            assert!(rects[0].bottom() <= rects[1].top());
        }
    }
}

#[test]
fn an_out_of_range_choice_shows_none_and_tabs_to_the_first() {
    let mut h = kit();
    h.state_mut().radio = 99;
    h.step();
    let radios = h.state().radios.unwrap();
    let target = h.state().segmented.map(|s| s.with(1));
    focus(&mut h, target);
    press(&mut h, Key::Tab);
    assert_eq!(focused(&h), Some(radios.with(0)));
    assert!((0..3).all(|i| automation::find(&h.ctx, &format!("kit.quality.option.{i}")).and_then(|e| e.selected) == Some(false)));
    press(&mut h, Key::ArrowDown);
    assert_eq!(h.state().radio, 1);
}

#[test]
fn badges_keycaps_and_progress_report_their_text() {
    let h = kit();
    let badge = automation::find(&h.ctx, "kit.badge").unwrap();
    assert_eq!((badge.role, badge.value.as_str()), (Role::Label, "3"));
    let keys = automation::find(&h.ctx, "kit.keys").unwrap();
    assert_eq!(keys.label, "Ctrl+Shift+P");
    assert_eq!(h.get_by_label("Ctrl+Shift+P").accesskit_node().role(), AkRole::Label);
    let bar = automation::find(&h.ctx, "kit.progress").unwrap();
    assert_eq!((bar.role, bar.value.as_str()), (Role::Progress, "40%"));
    assert_eq!(automation::find(&h.ctx, "kit.circle").unwrap().value, "20%");
    let node = h.get_by_label("Exporting").accesskit_node();
    assert_eq!(node.role(), AkRole::ProgressIndicator);
    assert_eq!(node.numeric_value(), Some(40.0));
    assert!(!node.is_busy());
    assert!(h.get_by_label("Loading").accesskit_node().is_busy());
}

fn animation_requests(h: &mut Harness<'_, Kit>, frames: usize) -> u64 {
    let before = Scheduler::of(&h.ctx).stats().requests_for(Reason::ANIMATION);
    for _ in 0..frames {
        h.step();
    }
    Scheduler::of(&h.ctx).stats().requests_for(Reason::ANIMATION) - before
}

#[test]
fn animations_ask_for_frames_only_while_visible_and_moving() {
    // determinate bar and circle, a visible spinner: only the spinner animates
    let mut h = kit();
    assert!(animation_requests(&mut h, 5) >= 5);
    // reduced motion: a static pose, no frames
    h.state_mut().reduced = true;
    h.step();
    assert_eq!(animation_requests(&mut h, 5), 0);
    // indeterminate, but every animated control under reduced motion or off screen
    h.state_mut().fraction = None;
    h.state_mut().hidden = true;
    h.step();
    assert_eq!(animation_requests(&mut h, 5), 0);
    h.state_mut().reduced = false;
    h.step();
    // the bar and circle are on screen and indeterminate now; the spinner is clipped
    assert!(animation_requests(&mut h, 5) >= 5);
    // a determinate fraction again: only the hidden spinner remains, so nothing asks
    h.state_mut().fraction = Some(0.5);
    h.step();
    assert_eq!(animation_requests(&mut h, 5), 0);
    // a loading button animates its spinner
    h.state_mut().loading = true;
    h.step();
    assert!(animation_requests(&mut h, 5) >= 5);
}

#[test]
fn hostile_sizes_and_values_are_clamped() {
    let mut h = Harness::builder().with_size(vec2(300.0, 200.0)).build_ui(|ui| {
        Progress::bar("x.bar", "x", Some(f32::INFINITY)).width(f32::NAN).show(ui);
        Progress::bar("x.bar2", "x", Some(-3.0)).width(1e9).show(ui);
        Progress::circle("x.circle", "x", Some(2.0)).size(f32::NEG_INFINITY).reduced_motion(true).show(ui);
        Spinner::new("x.spin", "x").size(f32::NAN).reduced_motion(true).show(ui);
        Button::new("x.button", "x").min_width(f32::INFINITY).show(ui);
        let mut i = usize::MAX;
        Segmented::new("x.seg", "x", &[], &mut i).show(ui);
        RadioGroup::new("x.radio", "x", &[], &mut i).show(ui);
        Keycap::new("x.key", "").show(ui);
        Badge::new("x.badge", "").show(ui);
    });
    automation::install(&h.ctx);
    h.step();
    h.step();
    assert_eq!(automation::find(&h.ctx, "x.bar").unwrap().value, "");
    let bar2 = automation::find(&h.ctx, "x.bar2").unwrap();
    assert_eq!(bar2.value, "0%");
    assert!(bar2.rect[2] <= 4000.0);
    assert_eq!(automation::find(&h.ctx, "x.circle").unwrap().value, "100%");
}

#[test]
fn disabled_options_cannot_be_picked_and_are_skipped_by_keys() {
    let mut h = Harness::builder().with_size(vec2(600.0, 300.0)).build_ui_state(
        |ui, (seg, radio): &mut (usize, usize)| {
            Segmented::new("d.view", "View", &["List", "Grid", "Columns", "Board"], seg).disabled(&[1, 3]).show(ui);
            RadioGroup::new("d.quality", "Quality", &["Draft", "Good", "Best"], radio).disabled(&[0]).show(ui);
        },
        (0usize, 0usize),
    );
    automation::install(&h.ctx);
    h.step();
    h.step();
    let grid = automation::find(&h.ctx, "d.view.option.1").unwrap();
    assert!(!grid.enabled);
    assert!(automation::find(&h.ctx, "d.view.option.2").unwrap().enabled);
    // a click on a disabled option does nothing
    crate::test_support::click(&mut h, grid.center());
    h.step();
    assert_eq!(h.state().0, 0);
    // Tab reaches the segmented control; the arrows skip the disabled options, wrapping
    h.key_press(Key::Tab);
    h.step();
    h.step();
    assert!(h.get_by_label("List").accesskit_node().is_focused());
    h.key_press(Key::ArrowRight);
    h.step();
    assert_eq!(h.state().0, 2, "Grid is skipped");
    h.key_press(Key::ArrowRight);
    h.step();
    assert_eq!(h.state().0, 0, "Board is skipped, wrapping to List");
    h.key_press(Key::End);
    h.step();
    assert_eq!(h.state().0, 2, "End lands on the last enabled option");
    assert!(h.get_by_label("Draft").accesskit_node().is_disabled());
    // the radio group's chosen option is disabled: its Tab stop is the first enabled one
    h.key_press(Key::Tab);
    h.step();
    h.step();
    assert!(h.get_by_label("Good").accesskit_node().is_focused());
    assert_eq!(h.state().1, 0, "focus alone does not pick");
}

#[test]
fn skeleton_rows_pulse_while_visible_and_hold_still_under_reduced_motion() {
    let mut h = Harness::builder().with_size(vec2(400.0, 300.0)).build_ui_state(
        |ui, reduced: &mut bool| {
            Skeleton::rows("s.list", 5).reduced_motion(*reduced).show(ui);
        },
        false,
    );
    automation::install(&h.ctx);
    h.step();
    let e = automation::find(&h.ctx, "s.list").unwrap();
    assert_eq!((e.role, e.label.as_str()), (Role::Progress, "Loading"));
    assert_eq!(e.rect[3], 5.0 * 24.0);
    assert!(h.get_by_label("Loading").accesskit_node().is_busy());
    let count = |h: &mut Harness<'_, bool>| {
        let before = Scheduler::of(&h.ctx).stats().requests_for(Reason::ANIMATION);
        for _ in 0..5 {
            h.step();
        }
        Scheduler::of(&h.ctx).stats().requests_for(Reason::ANIMATION) - before
    };
    assert!(count(&mut h) >= 5);
    *h.state_mut() = true;
    h.step();
    assert_eq!(count(&mut h), 0, "reduced motion asks for no frames");
    // the name follows the installed labels
    let mut l = crate::labels::UiLabels::default();
    l.controls.loading = "Wird geladen".into();
    crate::labels::install(&h.ctx, l);
    h.step();
    assert_eq!(automation::find(&h.ctx, "s.list").unwrap().label, "Wird geladen");
}

#[test]
fn the_shift_cap_is_drawn_as_a_shape() {
    let mut h = Harness::builder().with_size(vec2(300.0, 100.0)).build_ui(|ui| {
        Keycap::chord("k.mac", Chord::parse("Mod+Shift+P").unwrap(), Platform::Mac).show(ui);
    });
    h.step();
    let out = h.output();
    let lines = out.shapes.iter().filter(|s| matches!(&s.shape, egui::Shape::Path(p) if p.closed && p.points.len() == 7)).count();
    assert_eq!(lines, 1, "one outlined arrow for ⇧");
    let mut caps = 0;
    for s in &out.shapes {
        if let egui::Shape::Text(t) = &s.shape {
            caps += 1;
            assert!(!t.galley.job.text.contains('\u{21E7}'), "⇧ is never drawn as text");
        }
    }
    assert_eq!(caps, 2, "⌘ and P are text, ⇧ is not");
}
