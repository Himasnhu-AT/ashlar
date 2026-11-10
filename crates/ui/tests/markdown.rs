#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Markdown behaviour: focusable links, tables, task checkboxes, find, streaming repair and the
//! caller-kept revision.

use ashlar_ui::automation::{self, Role};
use ashlar_ui::markdown::{self, Markdown, MarkdownLabels, MarkdownResponse, MarkdownStyle, MdBlock, Span};
use egui::accesskit::Role as AkRole;
use egui::epaint::Shape;
use egui::{Key, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

#[derive(Default)]
struct S {
    text: String,
    query: String,
    streaming: bool,
    revision: Option<u64>,
    labels: Option<MarkdownLabels>,
    tasks: Vec<(usize, bool)>,
    opened: Vec<String>,
    last: MarkdownResponse,
}

fn style() -> MarkdownStyle {
    MarkdownStyle::from_visuals(&egui::Visuals::light(), 14.0)
}

fn harness(width: f32, height: f32, s: S) -> Harness<'static, S> {
    let mut h = Harness::builder().with_size((width, height)).build_ui_state(
        |ui, s: &mut S| {
            let style = style();
            let mut toggled = Vec::new();
            let mut md = Markdown::new("md", &s.text, &style).find(&s.query).streaming(s.streaming).on_task(|i, c| toggled.push((i, c)));
            if let Some(r) = s.revision {
                md = md.revision(r);
            }
            if let Some(l) = &s.labels {
                md = md.labels(l);
            }
            let r = md.show(ui);
            s.tasks.extend(toggled);
            if let Some(u) = &r.opened {
                s.opened.push(u.clone());
            }
            s.last = r;
        },
        s,
    );
    automation::install(&h.ctx);
    h.run();
    h
}

fn text(md: &str) -> S {
    S { text: md.into(), ..S::default() }
}

fn click(h: &mut Harness<'_, S>, p: Pos2) {
    h.hover_at(p);
    h.step();
    h.drag_at(p);
    h.step();
    h.drop_at(p);
    h.run();
}

/// The galleys painted in the last frame.
fn galleys(h: &Harness<'_, S>) -> Vec<(Pos2, std::sync::Arc<egui::Galley>)> {
    fn walk(s: &Shape, out: &mut Vec<(Pos2, std::sync::Arc<egui::Galley>)>) {
        match s {
            Shape::Text(t) => out.push((t.pos, t.galley.clone())),
            Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for c in &h.output().shapes {
        walk(&c.shape, &mut out);
    }
    out
}

fn painted_text(h: &Harness<'_, S>) -> String {
    galleys(h).iter().map(|(_, g)| g.text().to_string()).collect::<Vec<_>>().join("|")
}

#[test]
fn links_take_focus_and_open_with_enter() {
    let mut h = harness(500.0, 300.0, text("See [one](https://one.example) and [two](https://two.example).\n\nEnd."));
    let one = automation::find(&h.ctx, "md.link.0").expect("first link registered");
    assert_eq!((one.role, one.value.as_str()), (Role::Link, "https://one.example"));
    assert!(automation::find(&h.ctx, "md.link.1").is_some());
    let node = h.get_by_role_and_label(AkRole::Link, "two");
    assert!(node.accesskit_node().bounding_box().is_some());

    h.key_press(Key::Tab);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().opened, vec!["https://one.example".to_string()]);
    h.key_press(Key::Tab);
    h.run();
    h.key_press(Key::Enter);
    h.run();
    assert_eq!(h.state().opened.last().map(String::as_str), Some("https://two.example"));
}

#[test]
fn a_wrapped_link_answers_on_every_row() {
    let md = "[a long link text that wraps onto several rows of a narrow view](https://wrap.example)";
    let mut h = harness(140.0, 300.0, text(md));
    let first = automation::find(&h.ctx, "md.link.0").expect("link registered").egui_rect();
    // the link is the whole paragraph: its second row starts at the paragraph's left edge
    let below = Pos2::new(first.left() + 4.0, first.bottom() + first.height() * 0.5);
    click(&mut h, below);
    assert_eq!(h.state().opened, vec!["https://wrap.example".to_string()]);
}

#[test]
fn unsafe_and_pending_links_never_open() {
    let mut h = harness(400.0, 200.0, text("[x](javascript:alert(1))"));
    let r = automation::find(&h.ctx, "md.link.0").expect("registered").egui_rect();
    click(&mut h, r.center());
    assert!(h.state().opened.is_empty());
    // a link whose URL has not streamed in is styled, but is not a link yet
    let h = harness(400.0, 200.0, S { text: "see [the do".into(), streaming: true, ..S::default() });
    assert!(automation::find(&h.ctx, "md.link.0").is_none());
    assert_eq!(painted_text(&h), "see the do");
}

#[test]
fn task_checkboxes_report_toggles() {
    let mut h = harness(400.0, 200.0, text("- [ ] open\n- [x] done\n"));
    let open = automation::find(&h.ctx, "md.task.0").expect("task 0 registered");
    assert_eq!((open.role, open.selected), (Role::Toggle, Some(false)));
    assert_eq!(automation::find(&h.ctx, "md.task.1").and_then(|e| e.selected), Some(true));
    let node = h.get_by_role_and_label(AkRole::CheckBox, "open");
    assert_eq!(node.accesskit_node().label().as_deref(), Some("open"));
    click(&mut h, open.center());
    let done = automation::find(&h.ctx, "md.task.1").unwrap();
    click(&mut h, done.center());
    assert_eq!(h.state().tasks, vec![(0, true), (1, false)]);
}

#[test]
fn find_highlights_and_counts_matches() {
    let md = "Needle in a haystack, another needle.\n\n```\nlet needle = 1;\n```\n\n| a | b |\n|---|---|\n| NEEDLE | x |\n";
    let mut h = harness(500.0, 400.0, S { text: md.into(), query: "needle".into(), ..S::default() });
    assert_eq!(h.state().last.matches, 4);
    let bg = style().find_bg;
    let marked: usize = galleys(&h)
        .iter()
        .flat_map(|(_, g)| g.job.sections.iter().filter(|s| s.format.background == bg).map(|s| s.byte_range.end.0 - s.byte_range.start.0))
        .sum();
    assert_eq!(marked, 4 * "needle".len(), "every match is highlighted");
    h.state_mut().query.clear();
    h.run();
    assert_eq!(h.state().last.matches, 0);
    assert!(galleys(&h).iter().all(|(_, g)| g.job.sections.iter().all(|s| s.format.background != bg)));
}

#[test]
fn tables_are_sized_to_content_and_shrink_to_their_longest_word() {
    let long = "words ".repeat(40);
    let md = format!("| name | note |\n|---|---|\n| a | {long}|\n| b | incomprehensibilities |\n");
    let h = harness(300.0, 600.0, text(&md));
    let gs = galleys(&h);
    let cell = gs.iter().find(|(_, g)| g.text().starts_with("words words")).expect("long cell painted");
    assert!(cell.1.rows.len() > 1, "the long cell wraps");
    assert!(cell.0.x + cell.1.rect.width() <= 300.0, "the table fits the width: {:?}", cell.1.rect);
    let word = gs.iter().find(|(_, g)| g.text() == "incomprehensibilities").expect("word painted");
    assert_eq!(word.1.rows.len(), 1, "a column is never narrower than its longest word");

    // a short table stays narrow
    let h = harness(600.0, 300.0, text("| a | b |\n|---|---|\n| 1 | 2 |\n"));
    let gs = galleys(&h);
    let a = gs.iter().find(|(_, g)| g.text() == "a").unwrap().0.x;
    let b = gs.iter().find(|(_, g)| g.text() == "b").unwrap().0.x;
    assert!(b - a < 40.0, "content-sized columns: {a} → {b}");
}

#[test]
fn tables_past_the_row_cap_say_how_many_rows_are_hidden() {
    let mut md = String::from("| n |\n|---|\n");
    for i in 0..(markdown::MAX_TABLE_ROWS + 100) {
        md.push_str(&format!("| {i} |\n"));
    }
    let h = harness(300.0, 20_000.0, text(&md));
    let more = automation::find(&h.ctx, "md.table.0.more").expect("footer registered");
    assert_eq!(more.label, "100 more rows");
    let labels = MarkdownLabels { more_rows: "+{n} rows hidden".into(), ..MarkdownLabels::default() };
    let h = harness(300.0, 20_000.0, S { text: md, labels: Some(labels), ..S::default() });
    assert_eq!(automation::find(&h.ctx, "md.table.0.more").unwrap().label, "+100 rows hidden");
}

#[test]
fn code_block_words_come_from_the_labels() {
    let labels = MarkdownLabels { copy: "Kopieren".into(), code: "Quelltext".into(), ..MarkdownLabels::default() };
    let h = harness(400.0, 200.0, S { text: "```\nx\n```\n".into(), labels: Some(labels), ..S::default() });
    let text = painted_text(&h);
    assert!(text.contains("Kopieren") && text.contains("Quelltext"), "{text}");
    assert_eq!(automation::find(&h.ctx, "md.code.0.copy").unwrap().label, "Kopieren");
}

fn styles(blocks: &[MdBlock]) -> Vec<(String, bool, bool, bool, bool)> {
    let spans: &[Span] = match blocks.last() {
        Some(MdBlock::Paragraph(s) | MdBlock::Heading { spans: s, .. } | MdBlock::Item { spans: s, .. } | MdBlock::Quote(s)) => s,
        other => panic!("not text: {other:?}"),
    };
    spans.iter().map(|s| (s.text.clone(), s.bold, s.italic, s.code, s.link.is_some())).collect()
}

#[test]
fn streaming_repair_styles_unclosed_markers_at_the_tail() {
    let t = |s: &str, b, i, c, l| (s.to_string(), b, i, c, l);
    assert_eq!(styles(&markdown::parse_streaming("say **hel")), vec![t("say ", false, false, false, false), t("hel", true, false, false, false)]);
    assert_eq!(styles(&markdown::parse_streaming("say **")), vec![t("say ", false, false, false, false)]);
    assert_eq!(
        styles(&markdown::parse_streaming("run `cargo te")),
        vec![t("run ", false, false, false, false), t("cargo te", false, false, true, false)]
    );
    assert_eq!(styles(&markdown::parse_streaming("an *ita")), vec![t("an ", false, false, false, false), t("ita", false, true, false, false)]);
    assert_eq!(styles(&markdown::parse_streaming("see [do")), vec![t("see ", false, false, false, false), t("do", false, false, false, true)]);
    assert_eq!(
        styles(&markdown::parse_streaming("see [docs](https://ex")),
        vec![t("see ", false, false, false, false), t("docs", false, false, false, true)]
    );
    assert_eq!(styles(&markdown::parse_streaming("- item **b")), vec![t("item ", false, false, false, false), t("b", true, false, false, false)]);
    // closed markers and earlier blocks are unchanged; without streaming nothing is repaired
    assert_eq!(markdown::parse_streaming("a **b** c\n\nd"), markdown::parse("a **b** c\n\nd"));
    assert_eq!(markdown::parse_streaming("x **y\n\nz"), markdown::parse("x **y\n\nz"));
    assert_eq!(styles(&markdown::parse("say **hel")), vec![t("say **hel", false, false, false, false)]);
    // every prefix of a reply parses without a marker showing at the tail
    let reply = "Use **bold**, `code`, *it* and [a link](https://example.com) here.";
    for end in (1..=reply.len()).filter(|e| reply.is_char_boundary(*e)) {
        let blocks = markdown::parse_streaming(reply.get(..end).unwrap());
        let plain: String = styles(&blocks).iter().map(|s| s.0.as_str()).collect();
        assert!(!plain.ends_with('*') && !plain.ends_with('`') && !plain.contains('['), "{end}: {plain:?}");
    }
}

#[test]
fn streaming_view_hides_markers() {
    let mut h = harness(400.0, 200.0, S { text: "say **hel".into(), streaming: true, ..S::default() });
    assert_eq!(painted_text(&h), "say hel");
    h.state_mut().text.push_str("lo** world");
    h.run();
    assert_eq!(painted_text(&h), "say hello world");
    h.state_mut().streaming = false;
    h.state_mut().text = "tail **".into();
    h.run();
    assert_eq!(painted_text(&h), "tail **");
}

#[test]
fn an_unchanged_revision_skips_the_text() {
    let mut h = harness(400.0, 200.0, S { text: "alpha".into(), revision: Some(1), ..S::default() });
    assert_eq!(painted_text(&h), "alpha");
    // same revision, same length: the caller promised nothing changed, so the text is not read
    h.state_mut().text = "gamma".into();
    h.run();
    assert_eq!(painted_text(&h), "alpha");
    h.state_mut().revision = Some(2);
    h.run();
    assert_eq!(painted_text(&h), "gamma");
}

#[test]
fn installed_ui_labels_reach_code_blocks() {
    let mut i18n = ashlar_i18n::I18n::new("en");
    ashlar_ui::labels::add_catalogs(&mut i18n);
    i18n.set_locale("de");
    let mut h = Harness::builder().with_size((400.0, 200.0)).build_ui(|ui| {
        let style = style();
        markdown::show(ui, "md", "```\nx\n```\n", &style);
    });
    ashlar_ui::labels::install(&h.ctx, ashlar_ui::labels::UiLabels::from_i18n(&i18n));
    automation::install(&h.ctx);
    h.run();
    assert_eq!(automation::find(&h.ctx, "md.code.0.copy").unwrap().label, "Kopieren");
    assert_eq!(ashlar_ui::labels::UiLabels::from_i18n(&i18n).markdown.copied, "Kopiert");
}
