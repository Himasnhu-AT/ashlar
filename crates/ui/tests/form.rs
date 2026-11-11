#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The settings form pattern: filter, reset, automation ids, AccessKit and field errors.

use ashlar_ui::automation::{self, Role};
use ashlar_ui::controls::{Segmented, Switch};
use ashlar_ui::form::{self, Field, FormOutput};
use ashlar_ui::wake::Scheduler;
use egui::accesskit::{Invalid, Live, Role as AkRole};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

#[derive(Default)]
struct Prefs {
    query: String,
    launch: bool,
    updates: bool,
    theme: usize,
    name: String,
    error: Option<String>,
    out: FormOutput,
}

fn harness(prefs: Prefs) -> Harness<'static, Prefs> {
    let h = Harness::builder().with_size((520.0, 560.0)).build_ui_state(
        |ui, p: &mut Prefs| {
            let Prefs { query, launch, updates, theme, name, error, out } = p;
            *out = form::filter(ui, "settings", query, |f| {
                f.section("general", "General", |s| {
                    s.row("launch", "Launch at login")
                        .description("Starts with your session")
                        .keywords(&["startup", "boot"])
                        .bind(launch, false)
                        .control(|ui, id, on| Switch::new(id, "", on).show(ui));
                    s.row("updates", "Check for updates").bind(updates, true).control(|ui, id, on| Switch::new(id, "", on).show(ui));
                });
                f.section("appearance", "Appearance", |s| {
                    s.row("theme", "Theme")
                        .description("Light, dark or follow the system")
                        .bind(theme, 2)
                        .control(|ui, id, v| Segmented::new(id, "Theme", &["Light", "Dark", "System"], v).show(ui));
                });
            });
            Field::new("project.name", "Project name")
                .required()
                .description("Shown in the title bar")
                .error(error.as_deref())
                .show(ui, |ui, _id| ui.text_edit_singleline(name));
        },
        Prefs { updates: true, theme: 2, ..prefs },
    );
    automation::install(&h.ctx);
    let mut h = h;
    h.run();
    h
}

fn ids(h: &Harness<'_, Prefs>) -> Vec<String> {
    automation::inspect(&h.ctx).elements.into_iter().map(|e| e.id).collect()
}

fn set_query(h: &mut Harness<'_, Prefs>, q: &str) {
    h.state_mut().query = q.into();
    h.run();
}

fn click_id(h: &mut Harness<'_, Prefs>, id: &str) {
    let p = automation::find(&h.ctx, id).unwrap_or_else(|| panic!("{id} not registered")).center();
    h.hover_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    h.run();
}

#[test]
fn rows_register_section_dotted_ids() {
    let h = harness(Prefs::default());
    let ids = ids(&h);
    for id in ["general", "general.launch", "general.updates", "appearance", "appearance.theme"] {
        assert!(ids.iter().any(|i| i == id), "{id} missing from {ids:?}");
    }
    let launch = automation::find(&h.ctx, "general.launch").unwrap();
    assert_eq!((launch.role, launch.value.as_str()), (Role::Switch, "off"));
    // nothing is modified, so there is no reset button
    assert!(!ids.iter().any(|i| i.ends_with(".reset")));
    assert_eq!((h.state().out.rows, h.state().out.visible), (3, 3));
}

#[test]
fn an_agent_can_set_a_preference_by_id() {
    let mut h = harness(Prefs::default());
    click_id(&mut h, "general.launch");
    assert!(h.state().launch);
    assert!(automation::find(&h.ctx, "general.launch.reset").is_some());
}

#[test]
fn filter_hides_rows_and_empty_sections() {
    let mut h = harness(Prefs::default());
    set_query(&mut h, "LOGIN");
    let ids = ids(&h);
    assert!(ids.iter().any(|i| i == "general.launch"));
    assert!(!ids.iter().any(|i| i == "general.updates" || i.starts_with("appearance")), "{ids:?}");
    assert_eq!((h.state().out.rows, h.state().out.visible), (3, 1));
    // keywords and descriptions match too
    set_query(&mut h, "boot");
    assert!(automation::find(&h.ctx, "general.launch").is_some());
    set_query(&mut h, "follow the");
    assert!(automation::find(&h.ctx, "appearance.theme").is_some());
    assert!(automation::find(&h.ctx, "general").is_none());
    // a matching section title shows all its rows
    set_query(&mut h, "general");
    assert!(automation::find(&h.ctx, "general.updates").is_some());
}

#[test]
fn filter_with_no_match_shows_the_empty_state() {
    let mut h = harness(Prefs::default());
    set_query(&mut h, "zzz");
    assert!(h.state().out.is_empty());
    let empty = automation::find(&h.ctx, "settings.empty").unwrap();
    assert_eq!(empty.label, "No settings match");
    assert!(!ids(&h).iter().any(|i| i.starts_with("general") || i.starts_with("appearance")));
    set_query(&mut h, "");
    assert!(automation::find(&h.ctx, "settings.empty").is_none());
}

#[test]
fn reset_restores_the_default() {
    let mut h = harness(Prefs { launch: true, ..Prefs::default() });
    h.state_mut().theme = 0;
    h.run();
    assert!(automation::find(&h.ctx, "appearance.theme.reset").is_some());
    click_id(&mut h, "general.launch.reset");
    assert!(!h.state().launch);
    assert!(automation::find(&h.ctx, "general.launch.reset").is_none());
    click_id(&mut h, "appearance.theme.reset");
    assert_eq!(h.state().theme, 2);
}

#[test]
fn control_is_named_and_described_by_its_row() {
    let h = harness(Prefs::default());
    let node = h.get_by_role_and_label(AkRole::Switch, "Launch at login");
    assert_eq!(node.accesskit_node().description().as_deref(), Some("Starts with your session"));
    let heading = h.get_all_by_role(AkRole::Heading).map(|n| n.accesskit_node().label().unwrap_or_default()).collect::<Vec<_>>();
    assert_eq!(heading, ["General", "Appearance"]);
}

#[test]
fn required_field_error_is_described_and_announced() {
    let mut h = harness(Prefs::default());
    let input = h.get_by_role_and_label(AkRole::TextInput, "Project name");
    let node = input.accesskit_node();
    assert!(node.is_required());
    assert_eq!(node.data().invalid(), None);
    assert_eq!(node.description().as_deref(), Some("Shown in the title bar"));
    assert!(automation::find(&h.ctx, "project.name.error").is_none());

    h.state_mut().error = Some("A name is required".into());
    h.run();
    let input = h.get_by_role_and_label(AkRole::TextInput, "Project name");
    assert_eq!(input.accesskit_node().data().invalid(), Some(Invalid::True));
    assert_eq!(input.accesskit_node().description().as_deref(), Some("A name is required"));
    let error = h.get_by_label("A name is required");
    assert_eq!(error.accesskit_node().live(), Live::Polite);
    let e = automation::find(&h.ctx, "project.name.error").unwrap();
    assert_eq!((e.role, e.value.as_str()), (Role::Alert, "A name is required"));
}

#[test]
fn settled_form_requests_no_frames() {
    let mut h = harness(Prefs::default());
    set_query(&mut h, "theme");
    let before = Scheduler::of(&h.ctx).stats().requests();
    for _ in 0..5 {
        h.step();
    }
    assert_eq!(Scheduler::of(&h.ctx).stats().requests(), before);
    assert!(!h.ctx.has_requested_repaint());
}
