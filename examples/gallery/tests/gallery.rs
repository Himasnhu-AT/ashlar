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
        Page::Overlays => &["overlays.dialog", "overlays.dialog.button.2"],
        Page::Commands => &["palette", "palette.search", "palette.results.row.0", "keymap.search", "keymap.file.save.shortcut"],
    }
}

const SHELL: &[&str] = &["menu.0", "gallery.status", "gallery.status.export", "gallery.page.lists", "gallery.scale"];

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
