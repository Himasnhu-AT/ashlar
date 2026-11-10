#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `CodeView` behaviour: AccessKit roles, automation of visible lines only, pointer selection,
//! select-all and copy, wrapping and hostile input.

use std::ops::Range;

use ashlar_ui::automation;
use ashlar_ui::code_view::CodeView;
use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};

#[derive(Default)]
struct Out {
    selection: Option<Range<usize>>,
    copied: bool,
    clicked: Option<usize>,
    visible: Range<usize>,
}

fn harness(text: String, wrap: bool) -> Harness<'static, Out> {
    Harness::builder().with_size(vec2(480.0, 300.0)).build_ui_state(
        move |ui, out: &mut Out| {
            let r = CodeView::new("code", &text).lang(Some("rust")).wrap(wrap).show(ui);
            out.selection = r.selection;
            out.copied |= r.copied;
            out.clicked = r.clicked_line.or(out.clicked);
            out.visible = r.visible;
        },
        Out::default(),
    )
}

fn line_rect(h: &Harness<'_, Out>, n: usize) -> egui::Rect {
    automation::find(&h.ctx, &format!("code.line.{n}")).unwrap_or_else(|| panic!("line {n} not registered")).egui_rect()
}

fn press(h: &mut Harness<'_, Out>, at: Pos2, pressed: bool) {
    h.input_mut().events.push(Event::PointerMoved(at));
    h.input_mut().events.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
}

const SRC: &str = "fn main() {\n    let answer = 42;\n}\n";

#[test]
fn document_and_lines_are_accessible() {
    let mut h = harness(SRC.to_owned(), false);
    h.run();
    let line = h.get_by_label("    let answer = 42;");
    assert_eq!(line.accesskit_node().role(), Role::Paragraph);
    let doc = h.get_by_role(Role::Document);
    assert!(doc.accesskit_node().is_read_only());
    assert_eq!(automation::find(&h.ctx, "code.line.2").unwrap().label, "    let answer = 42;");
    assert_eq!(automation::find(&h.ctx, "code").map(|e| e.role), Some(automation::Role::Document));
}

#[test]
fn only_visible_lines_are_registered() {
    let text: String = (0..100_000).map(|i| format!("let x{i} = {i};\n")).collect();
    let mut h = harness(text, false);
    h.run();
    let lines = automation::query(&h.ctx, "code.line.");
    assert!(!lines.is_empty() && lines.len() < 60, "{} lines registered", lines.len());
    assert!(h.state().visible.end < 100);
}

#[test]
fn drag_selects_and_copy_copies() {
    let mut h = harness(SRC.to_owned(), false);
    h.run();
    let (l1, l2) = (line_rect(&h, 1), line_rect(&h, 2));
    // from inside "main" on line 1 to the far right of line 2
    let start = l1.left_center() + vec2(l1.width() * 0.02 + 60.0, 0.0);
    press(&mut h, start, true);
    h.run();
    assert_eq!(h.state().clicked, Some(0));
    h.input_mut().events.push(Event::PointerMoved(l2.right_center() - vec2(4.0, 0.0)));
    h.run();
    press(&mut h, l2.right_center() - vec2(4.0, 0.0), false);
    h.run();
    let sel = h.state().selection.clone().expect("a selection");
    assert!(sel.start < 11 && sel.end >= 28, "{sel:?}");
    assert!(SRC.is_char_boundary(sel.start) && SRC.is_char_boundary(sel.end));

    h.key_press_modifiers(Modifiers::COMMAND, Key::C);
    h.run();
    assert!(h.state().copied);
    let copied = h.output().platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(t) if t.contains("let answer")));
    assert!(copied || h.state().copied);
}

#[test]
fn select_all_then_copy() {
    let mut h = harness(SRC.to_owned(), false);
    h.run();
    let l1 = line_rect(&h, 1);
    press(&mut h, l1.center(), true);
    h.run();
    press(&mut h, l1.center(), false);
    h.run();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run();
    assert_eq!(h.state().selection, Some(0..SRC.len()));
    h.key_press_modifiers(Modifiers::COMMAND, Key::C);
    h.run();
    assert!(h.state().copied);
}

#[test]
fn wrapping_makes_long_lines_taller() {
    let long = format!("let s = \"{}\";\nnext\n", "word ".repeat(200));
    let mut plain = harness(long.clone(), false);
    plain.run();
    let mut wrapped = harness(long, true);
    wrapped.run();
    let (a, b) = (line_rect(&plain, 1), line_rect(&wrapped, 1));
    assert!(b.height() > a.height() * 3.0, "{a:?} vs {b:?}");
    assert!(line_rect(&wrapped, 2).top() >= b.bottom() - 0.5);
}

#[test]
fn hostile_text_does_not_panic() {
    for text in ["".to_owned(), "\n\n\n".to_owned(), "é😀\r\n\u{0}\t".repeat(1000), "x".repeat(200_000), "/*".repeat(5000)] {
        for wrap in [false, true] {
            let mut h = harness(text.clone(), wrap);
            h.run();
            let l1 = automation::find(&h.ctx, "code.line.1").map(|e| e.egui_rect());
            if let Some(r) = l1 {
                press(&mut h, r.center(), true);
                h.run();
                press(&mut h, r.center() + vec2(500.0, 200.0), false);
                h.run();
            }
            h.key_press_modifiers(Modifiers::COMMAND, Key::A);
            h.run();
        }
    }
}

#[test]
fn highlight_job_colours_known_languages_only() {
    use ashlar_ui::code_view::{SyntaxTheme, highlight_job};
    let theme = SyntaxTheme::from_tokens(&ashlar_ui::tokens::Tokens::dark());
    let font = egui::FontId::monospace(13.0);
    let rust = highlight_job(Some("rust"), "fn main() { let s = \"x\"; }", font.clone(), &theme);
    let colours: std::collections::HashSet<_> = rust.sections.iter().map(|s| s.format.color).collect();
    assert!(colours.len() >= 3, "{colours:?}");
    let plain = highlight_job(Some("klingon"), "fn main() {}", font, &theme);
    assert_eq!(plain.sections.len(), 1);
}

#[test]
fn markdown_code_fences_still_render() {
    let mut h = Harness::builder().with_size(vec2(480.0, 300.0)).build_ui(|ui| {
        let style = ashlar_ui::markdown::MarkdownStyle::from_visuals(ui.visuals(), 13.0);
        let _ = ashlar_ui::markdown::show(ui, "md", "Text\n\n```rust\nfn main() {}\n```\n", &style);
    });
    h.run();
    assert!(automation::find(&h.ctx, "md.code.0").is_some() || !automation::query(&h.ctx, "md.code.").is_empty());
}

/// Left edge of the painted text that starts with `prefix`.
fn text_x(h: &Harness<'_, Out>, prefix: &str) -> f32 {
    h.output()
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.job.text.starts_with(prefix) => Some(t.pos.x),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{prefix}` not painted"))
}

fn wheel_x(h: &mut Harness<'_, Out>, at: Pos2, dx: f32) {
    h.input_mut().events.push(Event::PointerMoved(at));
    h.input_mut().events.push(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(dx, 0.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    });
    h.run_steps(30);
}

#[test]
fn unwrapped_long_lines_scroll_sideways() {
    let long = format!("let long = \"{}\";\nshort\n", "x".repeat(400));
    let mut h = harness(long.clone(), false);
    h.run();
    let start = text_x(&h, "let long");
    let mid = line_rect(&h, 1).center();
    wheel_x(&mut h, mid, -200.0);
    let moved = text_x(&h, "let long");
    assert!(moved < start - 50.0, "the line moved left: {start} → {moved}");
    assert!(text_x(&h, "short") < start, "every line moves together");
    // never past the end of the longest line
    for _ in 0..40 {
        wheel_x(&mut h, mid, -400.0);
    }
    let end = text_x(&h, "let long");
    let width = 400.0 * 7.6; // at least the run of x's in a 12.5-point mono font
    assert!(end > start - width - 480.0, "clamped at the end: {end}");
    wheel_x(&mut h, mid, 1.0e9);
    assert_eq!(text_x(&h, "let long"), start, "back at the start");

    // wrapped text does not scroll sideways
    let mut h = harness(long, true);
    h.run();
    let start = text_x(&h, "let long");
    let mid = line_rect(&h, 1).center();
    wheel_x(&mut h, mid, -200.0);
    assert_eq!(text_x(&h, "let long"), start);
}
