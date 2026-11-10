#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The gallery headless: every page in light and dark renders its components (checked through
//! their automation ids), stays within a frame budget, and the main interactions work.
//! Set `ASHLAR_GALLERY_SNAPSHOTS=<dir>` to also write PNG snapshots (needs a GPU adapter).

use std::time::Instant;

use ashlar::ui::automation::{self, Role};
use ashlar::ui::tokens::{self, ThemeMode};
use ashlar_example_gallery::{Page, harness, snapshot};

fn expected(page: Page) -> &'static [&'static str] {
    match page {
        Page::Inputs => &[
            "inputs.opacity",
            "inputs.opacity.disabled",
            "inputs.duration",
            "inputs.gain",
            "inputs.gain.disabled",
            "inputs.pan",
            "inputs.knob.large",
            "inputs.swatches.0",
            "inputs.swatches.15",
            "inputs.search",
            "inputs.search.filled.clear",
        ],
        Page::Controls => &[
            "controls.button.primary",
            "controls.button.destructive",
            "controls.button.loading",
            "controls.switch.sync",
            "controls.switch.locked",
            "controls.view",
            "controls.view.option.2",
            "controls.quality.option.0",
            "controls.align.option.2",
            "controls.badge.danger",
            "controls.keys.palette",
            "controls.keys.mac",
            "controls.plan.option.2",
            "controls.skeleton",
            "controls.progress.bar",
            "controls.progress.indeterminate",
            "controls.progress.circle",
            "controls.spinner",
            "controls.icon.add",
        ],
        Page::Content => &[
            "content.banner.info",
            "content.banner.warning",
            "content.banner.warning.action",
            "content.banner.warning.dismiss",
            "content.banner.danger",
            "content.card",
            "content.card.plan",
            "content.advanced",
            "content.advanced.logs",
            "content.about",
            "content.empty",
            "content.empty.action",
        ],
        Page::Lists => &[
            "lists.list",
            "lists.list.row.0",
            "lists.table",
            "lists.table.header.2",
            "lists.table.row.1",
            "lists.tree",
            "lists.tree.node.1",
            "lists.tree.node.42",
        ],
        Page::Tabs => {
            &["tabs.documents", "tabs.documents.tab.0", "tabs.documents.tab.0.close", "tabs.many", "tabs.many.tab.30", "tabs.many.overflow"]
        }
        Page::Overlays => &["overlays.dialog", "overlays.dialog.button.2", "overlays.size", "overlays.font"],
        Page::Commands => &["palette", "palette.search", "palette.results.row.0", "keymap.search", "keymap.file.save.shortcut"],
        Page::Chat => &["chat.transcript", "chat.tool.read", "chat.tool.read.approve", "chat.tool.read.deny", "chat.input"],
        Page::Code => &["code.wrap", "code.numbers", "code.view", "code.view.line.1", "code.view.line.12"],
        Page::Settings => &["settings.search", "general", "general.launch", "general.updates", "appearance.theme", "appearance.icons"],
    }
}

const SHELL: &[&str] = &["menu.0", "gallery.status", "gallery.status.export", "gallery.page.lists", "gallery.page.code", "gallery.scale"];

#[test]
fn every_page_in_light_and_dark_shows_its_components() {
    for page in Page::ALL {
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let mut h = harness(page, mode, false);
            h.run_steps(4);
            assert_eq!(tokens::current(&h.ctx).dark, mode == ThemeMode::Dark, "{page:?} {mode:?}");
            let snap = automation::inspect(&h.ctx);
            for id in SHELL.iter().chain(expected(page)) {
                assert!(
                    snap.elements.iter().any(|e| &e.id == id),
                    "{page:?} {mode:?}: `{id}` missing; have {:?}",
                    snap.elements.iter().map(|e| &e.id).collect::<Vec<_>>()
                );
            }
            if page == Page::Overlays {
                let toasts = snap.elements.iter().filter(|e| e.role == Role::Toast).count();
                assert_eq!(toasts, 2);
            }
            // nothing is registered twice
            let mut ids: Vec<&str> = snap.elements.iter().map(|e| e.id.as_str()).collect();
            ids.sort_unstable();
            let before = ids.len();
            ids.dedup();
            assert_eq!(before, ids.len(), "{page:?}: duplicate automation ids");
        }
    }
}

#[test]
fn frames_stay_within_budget() {
    for page in Page::ALL {
        let mut h = harness(page, ThemeMode::Dark, false);
        h.run_steps(4);
        let mut times = Vec::new();
        for _ in 0..20 {
            let start = Instant::now();
            h.step();
            times.push(start.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(f64::total_cmp);
        let median = times[times.len() / 2];
        eprintln!("{page:?}: median frame {median:.2} ms (debug build, no GPU)");
        // generous for unoptimized test builds; the release target is 4 ms p95
        assert!(median < 60.0, "{page:?}: {median} ms");
    }
}

#[test]
fn drag_a_value_then_run_commands_from_palette_and_shortcut() {
    let mut h = harness(Page::Inputs, ThemeMode::Light, false);
    h.run_steps(3);
    let field = automation::find(&h.ctx, "inputs.opacity").unwrap();
    let start = h.state().opacity;
    h.hover_at(field.center());
    h.step();
    h.drag_at(field.center());
    h.step();
    h.hover_at(field.center() + egui::vec2(30.0, 0.0));
    h.step();
    h.drop_at(field.center() + egui::vec2(30.0, 0.0));
    h.step();
    assert_ne!(h.state().opacity, start);
    assert!(h.state().edit_count() >= 2, "begin and commit at least");

    // Mod+Shift+P opens the palette; typing and Enter runs a command
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::P);
    h.run_steps(3);
    assert!(h.state().palette.is_open());
    h.event(egui::Event::Text("pdf".into()));
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().ran.last().map(String::as_str), Some("file.export.pdf"));
    assert!(!h.state().palette.is_open());

    // the menu bar runs commands too, and switches the theme
    let view = automation::query(&h.ctx, "menu.").into_iter().find(|e| e.label == "View").unwrap();
    h.hover_at(view.center());
    h.step();
    h.drag_at(view.center());
    h.step();
    h.drop_at(view.center());
    h.run_steps(2);
    let grid = automation::find(&h.ctx, "menu.item.view.grid").unwrap();
    assert_eq!(grid.selected, Some(true));
    h.hover_at(grid.center());
    h.step();
    h.drag_at(grid.center());
    h.step();
    h.drop_at(grid.center());
    h.run_steps(2);
    assert!(!h.state().show_grid);
}

#[test]
fn snapshots_when_requested() {
    let Some(dir) = std::env::var_os("ASHLAR_GALLERY_SNAPSHOTS") else {
        eprintln!("SKIPPED: set ASHLAR_GALLERY_SNAPSHOTS=<dir> to render PNG snapshots");
        return;
    };
    let files = snapshot(std::path::Path::new(&dir)).unwrap();
    assert_eq!(files.len(), Page::ALL.len() * 2);
    for f in files {
        assert!(std::fs::metadata(&f).unwrap().len() > 10_000, "{}", f.display());
    }
}

#[test]
fn controls_page_switches_choices_and_motion() {
    use ashlar::ui::wake::{Reason, Scheduler};
    let mut h = harness(Page::Controls, ThemeMode::Dark, false);
    h.run_steps(3);
    let sync = automation::find(&h.ctx, "controls.switch.sync").unwrap();
    assert_eq!(sync.selected, Some(true));
    h.hover_at(sync.center());
    h.step();
    h.drag_at(sync.center());
    h.step();
    h.drop_at(sync.center());
    h.run_steps(2);
    assert!(!h.state().sync);
    let grid = automation::find(&h.ctx, "controls.view.option.2").unwrap();
    h.hover_at(grid.center());
    h.step();
    h.drag_at(grid.center());
    h.step();
    h.drop_at(grid.center());
    h.run_steps(2);
    assert_eq!(h.state().view, 2);
    assert_eq!(automation::find(&h.ctx, "controls.view").unwrap().value, "Columns");

    // spinners and indeterminate bars animate; with reduced motion the page asks for no frames
    let requests = |h: &mut egui_kittest::Harness<'_, ashlar_example_gallery::Gallery>| {
        let before = Scheduler::of(&h.ctx).stats().requests_for(Reason::ANIMATION);
        h.run_steps(4);
        Scheduler::of(&h.ctx).stats().requests_for(Reason::ANIMATION) - before
    };
    assert!(requests(&mut h) > 0);
    h.state_mut().reduce_motion = true;
    h.step();
    assert_eq!(requests(&mut h), 0);
}

#[test]
fn content_page_banner_and_sections() {
    let mut h = harness(Page::Content, ThemeMode::Light, false);
    h.run_steps(3);
    let click = |h: &mut egui_kittest::Harness<'_, ashlar_example_gallery::Gallery>, id: &str| {
        let p = automation::find(&h.ctx, id).unwrap_or_else(|| panic!("{id}")).center();
        h.hover_at(p);
        h.step();
        h.drag_at(p);
        h.step();
        h.drop_at(p);
        h.run_steps(2);
    };
    click(&mut h, "content.banner.warning.action");
    assert_eq!(h.state().retries, 1);
    click(&mut h, "content.banner.warning.dismiss");
    assert!(h.state().banner_dismissed);
    assert!(automation::find(&h.ctx, "content.banner.warning").is_none());
    click(&mut h, "content.advanced");
    assert!(!h.state().advanced_open);
    assert!(automation::find(&h.ctx, "content.advanced.logs").is_none());
}

#[test]
fn settings_page_filter_and_reset() {
    let mut h = harness(Page::Settings, ThemeMode::Light, false);
    h.run_steps(3);
    h.state_mut().settings_query = "dark".into();
    h.run_steps(2);
    assert!(automation::find(&h.ctx, "appearance.theme").is_some());
    assert!(automation::find(&h.ctx, "general").is_none());
    h.state_mut().settings_query = "nothing like this".into();
    h.run_steps(2);
    assert!(automation::find(&h.ctx, "settings.empty").is_some());
    h.state_mut().settings_query.clear();
    h.state_mut().launch_at_login = true;
    h.run_steps(2);
    let p = automation::find(&h.ctx, "general.launch.reset").unwrap().center();
    h.hover_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    h.run_steps(2);
    assert!(!h.state().launch_at_login);
    h.state_mut().project_name.clear();
    h.run_steps(2);
    assert!(automation::find(&h.ctx, "settings.project.error").is_some());
}

#[test]
fn overlays_page_toasts_run_their_actions_and_progress() {
    let mut h = harness(Page::Overlays, ThemeMode::Light, false);
    h.state_mut().dialog_open = false;
    h.state_mut().toasts.clear();
    h.run_steps(3);
    let click = |h: &mut egui_kittest::Harness<'_, ashlar_example_gallery::Gallery>, id: &str| {
        let p = automation::find(&h.ctx, id).unwrap_or_else(|| panic!("{id}")).center();
        h.hover_at(p);
        h.step();
        h.drag_at(p);
        h.step();
        h.drop_at(p);
        h.run_steps(3);
    };
    click(&mut h, "overlays.toast.action");
    assert_eq!(h.state().toasts.len(), 1);
    let grid = h.state().show_grid;
    let action = automation::query(&h.ctx, "toast.").into_iter().find(|n| n.id.ends_with(".action.0")).map(|n| n.id).unwrap();
    click(&mut h, &action);
    assert_eq!(h.state().show_grid, !grid, "the toast's action ran its gallery command");
    assert!(h.state().toasts.is_empty());
    click(&mut h, "overlays.toast.progress");
    click(&mut h, "overlays.toast.progress");
    assert_eq!(h.state().toasts.len(), 1, "a progress toast updates in place");
}
