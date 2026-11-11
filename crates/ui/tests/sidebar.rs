#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `Sidebar` behaviour: one Tab stop, keyboard navigation and folding, type-ahead, pointer
//! selection, activation and context clicks, rename in place, automation, AccessKit, labels,
//! virtualization and idling.

use ashlar_ui::automation::{self, Role};
use ashlar_ui::sidebar::{ItemRef, Sidebar, SidebarItem, SidebarSection, SidebarState};
use egui::accesskit::Role as AkRole;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};

struct App {
    state: SidebarState,
    places: Vec<String>,
    tags: Vec<String>,
    log: Vec<String>,
    before: Option<egui::Id>,
    after: Option<egui::Id>,
    list: Option<egui::Id>,
}

fn harness(tags: usize) -> Harness<'static, App> {
    let app = App {
        state: SidebarState::new(),
        places: ["Inbox", "Drafts", "Sent", "Archive"].map(String::from).to_vec(),
        tags: (0..tags).map(|i| format!("Tag {i}")).collect(),
        log: Vec::new(),
        before: None,
        after: None,
        list: None,
    };
    let mut h = Harness::builder().with_size(vec2(320.0, 400.0)).build_ui_state(
        |ui, app: &mut App| {
            app.before = Some(ui.button("Before").id);
            let star = |p: &egui::Painter, r: egui::Rect, c: egui::Color32| {
                p.circle_filled(r.center(), r.width() * 0.3, c);
            };
            let places: Vec<SidebarItem> = app
                .places
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    let item = SidebarItem::new(n).icon(&star);
                    if i == 0 { item.badge("3").detail("today") } else { item }
                })
                .collect();
            let tags: Vec<SidebarItem> = app.tags.iter().map(|n| SidebarItem::new(n)).collect();
            let sections = [SidebarSection::new("Mailboxes", &places).count(3), SidebarSection::new("Tags", &tags)];
            let out = Sidebar::new("nav", "Navigation", &mut app.state, &sections).max_height(300.0).show(ui);
            app.list = Some(out.response.id);
            if let Some(r) = out.select {
                app.log.push(format!("select {}.{}", r.section, r.item));
            }
            if let Some(r) = out.activated {
                app.log.push(format!("activate {}.{}", r.section, r.item));
            }
            if let Some(r) = out.context {
                app.log.push(format!("context {}.{}", r.section, r.item));
            }
            if let Some(s) = out.toggled {
                app.log.push(format!("toggle {s}"));
            }
            if let Some((r, name)) = out.renamed {
                app.log.push(format!("rename {}.{} {name}", r.section, r.item));
                if r.section == 0 {
                    app.places[r.item] = name;
                }
            }
            app.after = Some(ui.button("After").id);
        },
        app,
    );
    automation::install(&h.ctx);
    h.run();
    h
}

fn key(h: &mut Harness<'_, App>, k: Key) {
    h.key_press(k);
    h.run();
}

fn focused(h: &Harness<'_, App>) -> Option<egui::Id> {
    h.ctx.memory(|m| m.focused())
}

fn center(h: &Harness<'_, App>, id: &str) -> Pos2 {
    automation::find(&h.ctx, id).unwrap_or_else(|| panic!("{id} not registered")).center()
}

fn click(h: &mut Harness<'_, App>, at: Pos2, button: PointerButton, count: usize) {
    // one frame for all the presses (kittest frames are 0.25 s apart, longer than a double click)
    h.input_mut().events.push(Event::PointerMoved(at));
    for _ in 0..count {
        h.input_mut().events.push(Event::PointerButton { pos: at, button, pressed: true, modifiers: Modifiers::NONE });
        h.input_mut().events.push(Event::PointerButton { pos: at, button, pressed: false, modifiers: Modifiers::NONE });
    }
    h.run();
}

fn log(h: &mut Harness<'_, App>) -> Vec<String> {
    std::mem::take(&mut h.state_mut().log)
}

#[test]
fn sections_rows_automation_and_accesskit() {
    let h = harness(5);
    let header = automation::find(&h.ctx, "nav.section.0").unwrap();
    assert_eq!((header.role, header.label.as_str(), header.value.as_str(), header.selected), (Role::Disclosure, "Mailboxes", "3", Some(true)));
    assert_eq!(automation::find(&h.ctx, "nav.section.1").unwrap().value, "5", "the count defaults to the rows");
    let inbox = automation::find(&h.ctx, "nav.section.0.item.0").unwrap();
    assert_eq!((inbox.role, inbox.label.as_str(), inbox.value.as_str()), (Role::Row, "Inbox", "today"));
    assert_eq!(automation::find(&h.ctx, "nav").unwrap().role, Role::List);
    let node = h.get_by_label("Mailboxes");
    assert_eq!(node.accesskit_node().role(), AkRole::Button);
    assert_eq!(node.accesskit_node().data().is_expanded(), Some(true));
    assert_eq!(node.accesskit_node().description().as_deref(), Some("3 items"));
    assert_eq!(h.get_by_label("Drafts").accesskit_node().role(), AkRole::ListItem);
    assert_eq!(h.get_by_label("Navigation").accesskit_node().role(), AkRole::List);
}

#[test]
fn one_tab_stop_then_arrows_move_and_fold() {
    let mut h = harness(5);
    let before = h.state().before;
    h.ctx.memory_mut(|m| m.request_focus(before.unwrap()));
    h.run();
    key(&mut h, Key::Tab);
    assert_eq!(focused(&h), h.state().list, "Tab reaches the sidebar");
    key(&mut h, Key::Tab);
    assert_eq!(focused(&h), h.state().after, "one Tab leaves it again");
    key(&mut h, Key::Tab);
    h.ctx.memory_mut(|m| m.request_focus(h.state().list.unwrap()));
    h.run();
    log(&mut h);
    // Down: the first header, then the first row (selected)
    key(&mut h, Key::ArrowDown);
    key(&mut h, Key::ArrowDown);
    assert_eq!(log(&mut h), ["select 0.0"]);
    key(&mut h, Key::ArrowDown);
    assert_eq!(h.state().state.selected, Some(ItemRef { section: 0, item: 1 }));
    // Left: to the header, Left again folds it
    key(&mut h, Key::ArrowLeft);
    key(&mut h, Key::ArrowLeft);
    assert_eq!(log(&mut h), ["select 0.1", "toggle 0"]);
    assert!(h.state().state.is_collapsed(0));
    assert!(automation::find(&h.ctx, "nav.section.0.item.0").is_none());
    assert_eq!(automation::find(&h.ctx, "nav.section.0").unwrap().selected, Some(false));
    // Down from the folded header lands on the next section's header; End on the last row
    key(&mut h, Key::ArrowDown);
    key(&mut h, Key::End);
    assert_eq!(log(&mut h), ["select 1.4"]);
    // Home, then Right unfolds
    key(&mut h, Key::Home);
    key(&mut h, Key::ArrowRight);
    assert_eq!(log(&mut h), ["toggle 0"]);
    assert!(automation::find(&h.ctx, "nav.section.0.item.3").is_some());
    // Enter on a row activates it
    key(&mut h, Key::ArrowRight);
    key(&mut h, Key::Enter);
    assert_eq!(log(&mut h), ["select 0.0", "activate 0.0"]);
    assert_eq!(focused(&h), h.state().list, "the focus stayed on the sidebar");
}

#[test]
fn typing_jumps_to_a_matching_row() {
    let mut h = harness(5);
    h.ctx.memory_mut(|m| m.request_focus(h.state().list.unwrap()));
    h.run();
    h.input_mut().events.push(Event::Text("ta".into()));
    h.run();
    assert_eq!(log(&mut h), ["select 1.0"]);
    h.input_mut().events.push(Event::Text("g 3".into()));
    h.run();
    assert_eq!(log(&mut h), ["select 1.3"]);
}

#[test]
fn pointer_selects_activates_and_reports_context_clicks() {
    let mut h = harness(5);
    let p = center(&h, "nav.section.0.item.2");
    click(&mut h, p, PointerButton::Primary, 1);
    assert_eq!(log(&mut h), ["select 0.2"]);
    assert_eq!(focused(&h), h.state().list, "a click focuses the sidebar");
    let p = center(&h, "nav.section.0.item.1");
    click(&mut h, p, PointerButton::Primary, 2);
    let l = log(&mut h);
    assert!(l.contains(&"activate 0.1".to_string()), "{l:?}");
    let p = center(&h, "nav.section.1.item.0");
    click(&mut h, p, PointerButton::Secondary, 1);
    assert_eq!(log(&mut h), ["select 1.0", "context 1.0"]);
    let p = center(&h, "nav.section.1");
    click(&mut h, p, PointerButton::Primary, 1);
    assert_eq!(log(&mut h), ["toggle 1"]);
    assert!(automation::find(&h.ctx, "nav.section.1.item.0").is_none());
}

#[test]
fn f2_renames_in_place() {
    let mut h = harness(2);
    let p = center(&h, "nav.section.0.item.1");
    click(&mut h, p, PointerButton::Primary, 1);
    log(&mut h);
    key(&mut h, Key::F2);
    let field = automation::find(&h.ctx, "nav.rename").unwrap();
    assert_eq!((field.role, field.label.as_str(), field.value.as_str()), (Role::Field, "Rename", "Drafts"));
    // the name is selected: typing replaces it
    h.input_mut().events.push(Event::Text("Outbox".into()));
    h.run();
    key(&mut h, Key::Enter);
    assert_eq!(log(&mut h), ["rename 0.1 Outbox"]);
    assert_eq!(h.state().places[1], "Outbox");
    assert!(automation::find(&h.ctx, "nav.rename").is_none());
    assert_eq!(focused(&h), h.state().list, "the focus comes back to the sidebar");
    // Escape cancels
    key(&mut h, Key::F2);
    h.input_mut().events.push(Event::Text("Nope".into()));
    h.run();
    key(&mut h, Key::Escape);
    assert!(log(&mut h).is_empty());
    assert_eq!(h.state().places[1], "Outbox");
}

#[test]
fn a_huge_section_builds_only_visible_rows_and_idles() {
    let mut h = harness(200_000);
    let rows = automation::query(&h.ctx, "nav.section.1.item.").len();
    assert!(rows > 0 && rows < 40, "{rows} rows registered");
    h.ctx.memory_mut(|m| m.request_focus(h.state().list.unwrap()));
    h.run();
    key(&mut h, Key::End);
    assert_eq!(log(&mut h), ["select 1.199999"]);
    assert!(automation::find(&h.ctx, "nav.section.1.item.199999").is_some(), "the last row scrolled into view");
    // settled: no frame asked for
    for _ in 0..3 {
        h.step();
    }
    assert!(!h.ctx.has_requested_repaint(), "an idle sidebar asks for no frames");
}

#[test]
fn labels_follow_the_installed_locale() {
    let mut h = harness(2);
    let mut l = ashlar_ui::labels::UiLabels::default();
    l.sidebar.items = "{n} Einträge".into();
    ashlar_ui::labels::install(&h.ctx, l);
    h.run();
    assert_eq!(h.get_by_label("Tags").accesskit_node().description().as_deref(), Some("2 Einträge"));
}

#[test]
fn rows_stack_inside_a_horizontal_parent() {
    let mut h = Harness::builder().with_size(vec2(500.0, 300.0)).build_ui_state(
        |ui, state: &mut SidebarState| {
            let items = [SidebarItem::new("One"), SidebarItem::new("Two"), SidebarItem::new("Three")];
            let sections = [SidebarSection::new("Things", &items)];
            ui.horizontal_top(|ui| {
                Sidebar::new("side", "Side", state, &sections).show(ui);
            });
        },
        SidebarState::new(),
    );
    automation::install(&h.ctx);
    h.run();
    let one = automation::find(&h.ctx, "side.section.0.item.0").unwrap().rect;
    let three = automation::find(&h.ctx, "side.section.0.item.2").unwrap().rect;
    assert_eq!(one[0], three[0], "rows share a column");
    assert!(three[1] > one[1], "rows stack downwards");
}
