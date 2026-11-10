#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Accessibility goldens: every gallery page renders headless, its AccessKit tree is written as a
//! readable outline and compared with `tests/a11y/<page>.txt`. `ASHLAR_BLESS=1` rewrites the files.
//!
//! Each line is one node: role, "name", then value, description and states. Node ids and rects are
//! left out so the outline only changes when what a screen reader hears changes. Unnamed generic
//! containers are folded into their parent and text runs left out to keep the outline short.
//!
//! A second check fails when a node's name is an automation id (or looks like one), the mistake
//! that put `notes.list` in front of screen-reader users.

use std::fmt::Write as _;
use std::path::PathBuf;

use ashlar::ui::automation;
use ashlar::ui::tokens::ThemeMode;
use ashlar_example_gallery::{Gallery, Page};
use egui_kittest::kittest::{AccessKitNode, NodeT};

/// Names that look like dotted ids but are what a reader should hear, with the reason.
const NAME_ALLOW: &[(&str, &str)] =
    &[("main.rs", "a document tab titled with its file name"), ("lib.rs", "a document tab titled with its file name")];

fn harness(page: Page) -> egui_kittest::Harness<'static, Gallery> {
    let mut g = Gallery::for_snapshot(page, ThemeMode::Light);
    g.reduce_motion = true;
    egui_kittest::Harness::builder()
        .with_size(egui::vec2(1180.0, 780.0))
        .with_pixels_per_point(1.0)
        .with_theme(egui::Theme::Light)
        .with_max_steps(16)
        .build_ui_state(|ui, g: &mut Gallery| g.ui(ui), g)
}

fn quote(s: &str) -> String {
    format!("{s:?}")
}

/// Round a number so float noise never shows up in the outline.
fn num(v: f64) -> String {
    let r = (v * 1000.0).round() / 1000.0;
    if r == r.trunc() { format!("{r:.0}") } else { format!("{r}") }
}

fn describe(node: &AccessKitNode<'_>) -> String {
    let data = node.data();
    let mut line = format!("{:?}", node.role());
    if let Some(label) = node.label().filter(|l| !l.is_empty()) {
        let _ = write!(line, " {}", quote(&label));
    }
    if let Some(value) = node.value().filter(|v| !v.is_empty()) {
        let _ = write!(line, " value={}", quote(&value));
    }
    if let Some(v) = node.numeric_value() {
        let _ = write!(line, " numeric={}", num(v));
    }
    if let (Some(lo), Some(hi)) = (data.min_numeric_value(), data.max_numeric_value()) {
        let _ = write!(line, " range={}..{}", num(lo), num(hi));
    }
    if let Some(d) = node.description().filter(|d| !d.is_empty()) {
        let _ = write!(line, " description={}", quote(&d));
    }
    if let Some(p) = data.placeholder().filter(|p| !p.is_empty()) {
        let _ = write!(line, " placeholder={}", quote(p));
    }
    let mut states: Vec<String> = Vec::new();
    if let Some(t) = node.toggled() {
        states.push(format!("toggled={t:?}").to_lowercase());
    }
    if node.is_selected() == Some(true) {
        states.push("selected".into());
    }
    match data.is_expanded() {
        Some(true) => states.push("expanded".into()),
        Some(false) => states.push("collapsed".into()),
        None => {}
    }
    if node.is_disabled() {
        states.push("disabled".into());
    }
    if node.is_hidden() {
        states.push("hidden".into());
    }
    if data.is_read_only() {
        states.push("read-only".into());
    }
    if data.is_required() {
        states.push("required".into());
    }
    if let Some(live) = data.live() {
        states.push(format!("live={live:?}").to_lowercase());
    }
    if let (Some(pos), Some(size)) = (data.position_in_set(), data.size_of_set()) {
        states.push(format!("{pos}/{size}"));
    }
    if !states.is_empty() {
        let _ = write!(line, " [{}]", states.join(", "));
    }
    line
}

/// A node that adds nothing a reader hears: an unnamed generic container without states.
/// (`line` is the described node, so a bare role name means no name, value or state.)
fn foldable(line: &str) -> bool {
    line == "GenericContainer"
}

fn walk(node: &AccessKitNode<'_>, depth: usize, out: &mut String, names: &mut Vec<String>) {
    let line = describe(node);
    let fold = foldable(&line) && depth > 0;
    let child_depth = if fold { depth } else { depth + 1 };
    if !fold {
        let _ = writeln!(out, "{}{line}", "  ".repeat(depth));
        if let Some(label) = node.label() {
            names.push(label);
        }
    }
    for child in node.children() {
        // text runs repeat the text of the label that holds them
        if format!("{:?}", child.role()) == "TextRun" {
            continue;
        }
        walk(&child, child_depth, out, names);
    }
}

/// `a.b` or `a.b.c`, lowercase letters, digits, `_` and `-`: how automation ids are written.
fn looks_like_id(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'))
        && parts.first().is_some_and(|p| p.starts_with(|c: char| c.is_ascii_lowercase()))
}

/// Lines around the first difference, `-` for the stored copy and `+` for this run.
fn diff(want: &str, got: &str) -> String {
    let a: Vec<&str> = want.lines().collect();
    let b: Vec<&str> = got.lines().collect();
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let mut out = String::new();
    let start = prefix.saturating_sub(3);
    for l in &a[start..prefix] {
        let _ = writeln!(out, "  {l}");
    }
    for l in &a[prefix..a.len() - suffix] {
        let _ = writeln!(out, "- {l}");
    }
    for l in &b[prefix..b.len() - suffix] {
        let _ = writeln!(out, "+ {l}");
    }
    for l in a[a.len() - suffix..].iter().take(3) {
        let _ = writeln!(out, "  {l}");
    }
    out
}

fn outline(page: Page) -> (String, Vec<String>, Vec<String>) {
    let mut h = harness(page);
    h.run_steps(6);
    let ids: Vec<String> = automation::inspect(&h.ctx).elements.into_iter().map(|e| e.id).collect();
    let root = h.root();
    let mut out = String::new();
    let mut names = Vec::new();
    walk(&root.accesskit_node(), 0, &mut out, &mut names);
    (out, names, ids)
}

#[test]
fn every_page_matches_its_accessibility_golden() {
    let bless = std::env::var_os("ASHLAR_BLESS").is_some_and(|v| v == "1");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/a11y");
    let mut failures = Vec::new();
    for page in Page::ALL {
        let (got, names, ids) = outline(page);
        // the same page rendered again gives the same outline
        assert_eq!(got, outline(page).0, "{page:?}: accessibility tree is not deterministic");

        for name in &names {
            let allowed = NAME_ALLOW.iter().any(|(n, _)| n == name);
            if !allowed && (ids.iter().any(|id| id == name) || looks_like_id(name)) {
                failures.push(format!("{page:?}: a node is named `{name}`, an automation id; give it a readable label"));
            }
        }

        let path = dir.join(format!("{}.txt", page.name().to_lowercase()));
        if bless {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, &got).unwrap();
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(want) if want == got => {}
            Ok(want) => failures.push(format!(
                "{page:?}: accessibility tree changed ({}); rerun with ASHLAR_BLESS=1 if intended\n{}",
                path.display(),
                diff(&want, &got)
            )),
            Err(e) => failures.push(format!("{page:?}: {}: {e}; run with ASHLAR_BLESS=1 to create it", path.display())),
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn id_pattern() {
    assert!(looks_like_id("notes.list"));
    assert!(looks_like_id("controls.view.option.2"));
    assert!(!looks_like_id("Notes"));
    assert!(!looks_like_id("Version 1.2"));
    assert!(!looks_like_id("e.g. a name"));
    assert!(!looks_like_id("."));
}
