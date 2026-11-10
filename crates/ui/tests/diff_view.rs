#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `DiffView` behaviour: automation ids for visible rows only, expanding collapsed ranges,
//! folding files, hunk navigation, selection, the sticky header, AccessKit roles and hostile
//! input.

use ashlar_diff::{FileDiff, LineKind, parse_patch};
use ashlar_ui::automation;
use ashlar_ui::diff_view::{DiffMode, DiffView, DiffViewState, EXPAND_STEP, SelectedLine};
use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};

struct App {
    files: Vec<FileDiff>,
    mode: DiffMode,
    state: DiffViewState,
    selected: Vec<SelectedLine>,
    expanded: Option<(usize, usize)>,
    toggled: Option<usize>,
    hunk: Option<(usize, usize)>,
    top_file: Option<usize>,
}

fn harness(files: Vec<FileDiff>, mode: DiffMode, height: f32) -> Harness<'static, App> {
    let app = App { files, mode, state: DiffViewState::new(), selected: Vec::new(), expanded: None, toggled: None, hunk: None, top_file: None };
    Harness::builder().with_size(vec2(900.0, height)).build_ui_state(
        |ui, app: &mut App| {
            let r = DiffView::new("diff", &mut app.state, &app.files, 1).mode(app.mode).show(ui);
            app.selected = r.selected_lines;
            app.expanded = r.expanded.or(app.expanded);
            app.toggled = r.toggled_file.or(app.toggled);
            app.hunk = r.hunk.or(app.hunk);
            app.top_file = r.top_file;
        },
        app,
    )
}

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("let v{i} = {i};\n")).collect()
}

/// 60 lines with lines 20 and 40 changed: two hunks, three collapsed ranges.
fn two_hunks() -> FileDiff {
    let old = numbered(60);
    let new: String = (1..=60)
        .map(|i| match i {
            20 => "let v20 = 2000;\n".to_owned(),
            40 => "let changed = 40;\n".to_owned(),
            _ => format!("let v{i} = {i};\n"),
        })
        .collect();
    FileDiff::from_texts(&old, &new).with_paths("src/lib.rs", "src/lib.rs")
}

fn rect(h: &Harness<'_, App>, id: &str) -> egui::Rect {
    automation::find(&h.ctx, id).unwrap_or_else(|| panic!("{id} not registered")).egui_rect()
}

fn click(h: &mut Harness<'_, App>, at: Pos2, modifiers: Modifiers) {
    h.input_mut().events.push(Event::ModifiersChanged(modifiers));
    h.input_mut().events.push(Event::PointerMoved(at));
    h.input_mut().events.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers });
    h.run();
    h.input_mut().events.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers });
    h.run();
    h.input_mut().events.push(Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

fn key(h: &mut Harness<'_, App>, k: Key, modifiers: Modifiers) {
    h.input_mut().events.push(Event::ModifiersChanged(modifiers));
    h.input_mut().events.push(Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers });
    h.run();
    h.input_mut().events.push(Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers });
    h.input_mut().events.push(Event::ModifiersChanged(Modifiers::NONE));
    h.run();
}

#[test]
fn rows_register_automation_and_accesskit() {
    let mut h = harness(vec![two_hunks()], DiffMode::Unified, 600.0);
    h.run();
    assert_eq!(automation::find(&h.ctx, "diff.file.0").unwrap().label, "src/lib.rs");
    assert!(automation::find(&h.ctx, "diff.file.0.hunk.0").unwrap().label.starts_with("@@ -17,7 +17,7 @@"));
    assert!(automation::find(&h.ctx, "diff.file.0.hunk.1").is_some());
    assert!(automation::find(&h.ctx, "diff.file.0.gap.0").unwrap().label.contains("16 unchanged lines"));
    // line 3 of hunk 0 is the removed line 20
    let removed = automation::find(&h.ctx, "diff.file.0.hunk.0.line.3").unwrap();
    assert_eq!(removed.label, "removed 20: let v20 = 20;");
    assert_eq!(automation::find(&h.ctx, "diff.header").unwrap().label, "src/lib.rs");
    assert_eq!(automation::find(&h.ctx, "diff").unwrap().role, automation::Role::Document);
    let doc = h.get_by_role(Role::Document);
    assert!(doc.accesskit_node().is_read_only());
    assert_eq!(h.get_by_label("added 20: let v20 = 2000;").accesskit_node().role(), Role::Paragraph);
    assert!(h.query_all_by_role(Role::Heading).count() >= 3);
}

#[test]
fn only_visible_rows_are_registered() {
    let old = numbered(20_000);
    let new = old.replace("let v1 = 1;\n", "let v1 = 0;\n").replace("let v19999 = 19999;\n", "x\n");
    let d = FileDiff::from_texts_with(&old, &new, ashlar_diff::DiffOptions { context: 10_000, ..Default::default() });
    let mut h = harness(vec![d], DiffMode::Unified, 400.0);
    h.run();
    assert!(automation::find(&h.ctx, "diff.file.0.hunk.0.line.5").is_some());
    assert!(automation::find(&h.ctx, "diff.file.0.hunk.0.line.15000").is_none());
}

#[test]
fn clicking_a_gap_expands_it() {
    let mut h = harness(vec![two_hunks()], DiffMode::Unified, 900.0);
    h.run();
    let middle = rect(&h, "diff.file.0.gap.1");
    assert!(automation::find(&h.ctx, "diff.file.0.gap.1").unwrap().label.contains("13 unchanged lines"));
    click(&mut h, middle.center(), Modifiers::NONE);
    assert_eq!(h.state().expanded, Some((0, 1)));
    // 13 ≤ EXPAND_STEP: the whole range shows and its gap row is gone
    const { assert!(13 <= EXPAND_STEP) };
    assert!(automation::find(&h.ctx, "diff.file.0.gap.1").is_none());
    assert_eq!(automation::find(&h.ctx, "diff.file.0.gap.1.line.24").unwrap().label, "24: let v24 = 24;");
    // the leading range expands EXPAND_STEP at a time; Alt shows all of it
    let lead = rect(&h, "diff.file.0.gap.0");
    click(&mut h, lead.center(), Modifiers::ALT);
    assert!(automation::find(&h.ctx, "diff.file.0.gap.0.line.1").is_some());
    assert!(automation::find(&h.ctx, "diff.file.0.gap.0").is_none());
}

#[test]
fn file_headers_fold_and_patch_gaps_do_not_expand() {
    let patch = include_str!("../../diff/tests/fixtures/git_mixed.patch");
    let files = parse_patch(patch).unwrap();
    let main = files.iter().position(|f| f.path() == "main.rs").unwrap();
    let moved = files.iter().position(|f| f.path() == "moved.txt").unwrap();
    let mut h = harness(files, DiffMode::Unified, 2000.0);
    h.run();
    assert_eq!(automation::find(&h.ctx, &format!("diff.file.{moved}")).unwrap().label, "move_me.txt → moved.txt");
    // a patch does not carry the hidden text
    let gap = automation::find(&h.ctx, &format!("diff.file.{moved}.gap.0")).unwrap();
    assert_eq!(gap.label, "11 unchanged lines");
    click(&mut h, gap.egui_rect().center(), Modifiers::NONE);
    assert_eq!(h.state().expanded, None);
    assert!(h.get_by_label("Binary file not shown").accesskit_node().role() == Role::Paragraph);

    let header = rect(&h, &format!("diff.file.{main}"));
    assert!(automation::find(&h.ctx, &format!("diff.file.{main}.hunk.0")).is_some());
    click(&mut h, header.center(), Modifiers::NONE);
    assert_eq!(h.state().toggled, Some(main));
    assert!(h.state().state.is_file_collapsed(main));
    assert!(automation::find(&h.ctx, &format!("diff.file.{main}.hunk.0")).is_none());
}

#[test]
fn keyboard_moves_between_hunks_and_selects_lines() {
    let mut h = harness(vec![two_hunks(), two_hunks()], DiffMode::Unified, 400.0);
    h.run();
    // focus the view by clicking the first file header's next row (the leading gap is a row too)
    let line = rect(&h, "diff.file.0.hunk.0.line.0");
    click(&mut h, line.center(), Modifiers::NONE);
    assert_eq!(h.state().selected.len(), 1);
    assert_eq!(h.state().selected[0].new_no, Some(17));
    key(&mut h, Key::N, Modifiers::NONE);
    assert_eq!(h.state().hunk, Some((0, 1)));
    key(&mut h, Key::N, Modifiers::NONE);
    assert_eq!(h.state().hunk, Some((1, 0)));
    assert_eq!(h.state().top_file, Some(1), "the sticky header follows the scroll");
    key(&mut h, Key::F7, Modifiers::SHIFT);
    assert_eq!(h.state().hunk, Some((0, 1)));
    // Shift+Down extends the selection over the hunk header and its first lines
    key(&mut h, Key::ArrowDown, Modifiers::SHIFT);
    key(&mut h, Key::ArrowDown, Modifiers::SHIFT);
    let sel = &h.state().selected;
    assert_eq!(sel.len(), 2);
    assert_eq!((sel[0].kind, sel[0].new_no), (LineKind::Context, Some(37)));
}

#[test]
fn split_mode_pairs_lines() {
    let mut h = harness(vec![two_hunks()], DiffMode::Split, 600.0);
    h.run();
    let row = automation::find(&h.ctx, "diff.file.0.hunk.0.line.3").unwrap();
    assert_eq!(row.label, "removed 20: let v20 = 20; | added 20: let v20 = 2000;");
    click(&mut h, row.egui_rect().center(), Modifiers::NONE);
    let kinds: Vec<LineKind> = h.state().selected.iter().map(|l| l.kind).collect();
    assert_eq!(kinds, vec![LineKind::Removed, LineKind::Added]);
}

#[test]
fn hostile_input_does_not_panic() {
    let long = "x".repeat(200_000);
    let mut odd = FileDiff::from_texts(&format!("{long}\n\r\n\t\u{200b}é\n"), "é\n");
    odd.hunks.push(ashlar_diff::Hunk {
        old_start: u32::MAX,
        old_len: u32::MAX,
        new_start: u32::MAX,
        new_len: 0,
        section: String::new(),
        lines: vec![ashlar_diff::Line {
            kind: LineKind::Added,
            old_no: None,
            new_no: Some(u32::MAX),
            text: "é".into(),
            no_newline: true,
            #[allow(clippy::reversed_empty_ranges)]
            emphasis: vec![0..1, 7..3, 100..200],
        }],
    });
    for mode in [DiffMode::Unified, DiffMode::Split] {
        let mut h = harness(vec![FileDiff::default(), odd.clone(), FileDiff::from_texts("", "")], mode, 300.0);
        h.run();
        for k in [Key::N, Key::N, Key::N, Key::P, Key::End, Key::Home] {
            key(&mut h, k, Modifiers::NONE);
        }
        let mut empty = harness(Vec::new(), mode, 300.0);
        empty.run();
        key(&mut empty, Key::N, Modifiers::NONE);
    }
}

#[test]
fn long_lines_scroll_sideways_in_both_modes() {
    let long = "x".repeat(600);
    let patch = format!("--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n context\n-old {long}\n+new {long}\n");
    for mode in [DiffMode::Unified, DiffMode::Split] {
        let files = parse_patch(&patch).unwrap();
        let mut h = harness(files, mode, 300.0);
        h.run();
        let x = |h: &Harness<'_, App>, prefix: &str| {
            h.output()
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.job.text.starts_with(prefix) => Some(t.pos.x),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("`{prefix}` not painted"))
        };
        let (old0, new0) = (x(&h, "old "), x(&h, "new "));
        let at = rect(&h, "diff.file.0.hunk.0.line.1").center();
        h.input_mut().events.push(Event::PointerMoved(at));
        h.input_mut().events.push(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(-300.0, 0.0),
            modifiers: Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
        h.run_steps(30);
        assert!(x(&h, "old ") < old0 - 100.0, "{mode:?}: the old line moved");
        assert!(x(&h, "new ") < new0 - 100.0, "{mode:?}: the new line moved with it");
    }
}
