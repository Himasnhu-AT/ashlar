//! Behaviour tests for the content components: pointer, keyboard, automation and AccessKit.

use egui::accesskit::{Live, Role as AkRole};
use egui::{Id, Key, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use crate::automation::{self, Role};
use crate::controls::Tone;

#[derive(Default)]
struct Page {
    tone: Tone,
    banner_action: usize,
    dismissed: usize,
    open: bool,
    body_runs: usize,
    header: Option<Id>,
    toggles: usize,
    empty_action: usize,
    card_runs: usize,
}

fn page() -> Harness<'static, Page> {
    let mut h = Harness::builder().with_size(vec2(640.0, 600.0)).build_ui_state(
        |ui, s: &mut Page| {
            let out = Banner::new("sync.offline", s.tone, "Changes stay on this computer until the connection is back.")
                .title("Offline")
                .action("Retry")
                .dismissible("Close")
                .show(ui);
            s.banner_action += usize::from(out.action);
            s.dismissed += usize::from(out.dismissed);
            let out = Collapsible::new("settings.advanced", "Advanced", &mut s.open).show(ui, |ui| {
                ui.label("Hidden setting");
            });
            s.header = Some(out.header.id);
            s.toggles += usize::from(out.header.changed());
            s.body_runs += usize::from(out.body.is_some());
            Card::new("settings.account").title("Account").show(ui, |ui| {
                ui.label("Signed in");
                s.card_runs += 1;
            });
            let out = EmptyState::new("files.empty", "No files yet").body("Drop files here or create one.").action("New file").show(ui);
            s.empty_action += usize::from(out.action);
        },
        Page { tone: Tone::Warning, ..Default::default() },
    );
    automation::install(&h.ctx);
    h.step();
    h.step();
    h
}

fn click_id(h: &mut Harness<'_, Page>, id: &str) {
    let p = automation::find(&h.ctx, id).unwrap_or_else(|| panic!("{id} not registered")).center();
    crate::test_support::click(h, p);
    h.step();
}

fn press(h: &mut Harness<'_, Page>, key: Key) {
    h.key_press(key);
    h.step();
    h.step();
}

#[test]
fn banner_buttons_report_and_the_banner_is_a_live_region() {
    let mut h = page();
    click_id(&mut h, "sync.offline.action");
    assert_eq!(h.state().banner_action, 1);
    click_id(&mut h, "sync.offline.dismiss");
    assert_eq!(h.state().dismissed, 1);
    let e = automation::find(&h.ctx, "sync.offline").unwrap();
    assert_eq!((e.role, e.label.as_str()), (Role::Alert, "Offline"));
    assert!(e.value.starts_with("Changes stay"));
    let node = h.get_by_label("Offline");
    let node = node.accesskit_node();
    assert_eq!(node.role(), AkRole::Status);
    assert_eq!(node.live(), Live::Polite);
    assert!(node.description().is_some_and(|d| d.starts_with("Changes stay")));
    assert_eq!(h.get_by_label("Close").accesskit_node().role(), AkRole::Button);
    // errors interrupt
    h.state_mut().tone = Tone::Danger;
    h.step();
    let node = h.get_by_label("Offline");
    assert_eq!(node.accesskit_node().role(), AkRole::Alert);
    assert_eq!(node.accesskit_node().live(), Live::Assertive);
    // the buttons sit inside the banner
    let banner = e.egui_rect();
    assert!(banner.contains_rect(automation::find(&h.ctx, "sync.offline.action").unwrap().egui_rect()));
    assert!(banner.contains_rect(automation::find(&h.ctx, "sync.offline.dismiss").unwrap().egui_rect()));
}

#[test]
fn banner_buttons_work_from_the_keyboard() {
    let mut h = page();
    press(&mut h, Key::Tab);
    // the close button is laid out first (trailing edge), then the action
    press(&mut h, Key::Enter);
    assert_eq!(h.state().dismissed, 1);
    press(&mut h, Key::Tab);
    press(&mut h, Key::Space);
    assert_eq!(h.state().banner_action, 1);
}

#[test]
fn collapsible_toggles_by_pointer_keys_and_arrows() {
    let mut h = page();
    assert_eq!(h.state().body_runs, 0, "closed: the body does not run");
    click_id(&mut h, "settings.advanced");
    assert!(h.state().open);
    assert!(h.state().body_runs > 0);
    let header = h.state().header.unwrap();
    h.ctx.memory_mut(|m| m.request_focus(header));
    h.step();
    press(&mut h, Key::Space);
    assert!(!h.state().open);
    press(&mut h, Key::Enter);
    assert!(h.state().open);
    press(&mut h, Key::ArrowLeft);
    assert!(!h.state().open);
    press(&mut h, Key::ArrowLeft);
    assert!(!h.state().open, "left on a closed section does nothing");
    press(&mut h, Key::ArrowRight);
    assert!(h.state().open);
    assert_eq!(h.state().toggles, 5);
    let e = automation::find(&h.ctx, "settings.advanced").unwrap();
    assert_eq!((e.role, e.selected), (Role::Disclosure, Some(true)));
    let node = h.get_by_label("Advanced");
    assert_eq!(node.accesskit_node().role(), AkRole::Button);
    assert_eq!(node.accesskit_node().data().is_expanded(), Some(true));
    assert!(h.query_by_label("Hidden setting").is_some());
    press(&mut h, Key::ArrowLeft);
    assert_eq!(h.get_by_label("Advanced").accesskit_node().data().is_expanded(), Some(false));
    assert!(h.query_by_label("Hidden setting").is_none());
}

#[test]
fn card_and_empty_state_are_named_groups() {
    let mut h = page();
    assert!(h.state().card_runs > 0);
    let card = automation::find(&h.ctx, "settings.account").unwrap();
    assert_eq!((card.role, card.label.as_str()), (Role::Group, "Account"));
    assert!(h.get_all_by_role(AkRole::Group).any(|n| n.accesskit_node().label().as_deref() == Some("Account")));
    let empty = automation::find(&h.ctx, "files.empty").unwrap();
    assert_eq!((empty.role, empty.label.as_str(), empty.value.as_str()), (Role::Group, "No files yet", "Drop files here or create one."));
    assert!(h.get_all_by_role(AkRole::Group).any(|n| n.accesskit_node().label().as_deref() == Some("No files yet")));
    click_id(&mut h, "files.empty.action");
    assert_eq!(h.state().empty_action, 1);
}

#[test]
fn content_components_idle_without_frames() {
    use crate::wake::Scheduler;
    let mut h = page();
    let before = Scheduler::of(&h.ctx).stats().requests();
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(Scheduler::of(&h.ctx).stats().requests(), before);
}
