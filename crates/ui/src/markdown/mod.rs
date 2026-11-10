//! Markdown for chat replies and short documents: headings, paragraphs, bullet, numbered and
//! task lists, block quotes, rules, tables and fenced code blocks (with a Copy button), and inline
//! code, bold, italic, strike-through and links.
//!
//! [`parse`] is forgiving and made for text that is still streaming in: an unclosed code fence
//! runs to the end, an unclosed `**` or `` ` `` is shown as typed, so a reply does not jump as it
//! grows. [`show`] draws the blocks with a [`MarkdownStyle`] the app fills from its theme.
//! Paragraphs are selectable; a link opens in the browser (`http`, `https` and `mailto` only).
//!
//! Automation ids (under the `id` passed to [`show`]): `<id>.code.<n>` for each code block and
//! `<id>.code.<n>.copy` for its Copy button, `<id>.link.<n>` for each link (role `Link`; its rect
//! is the link's first row), `<id>.task.<n>` for each task checkbox when [`Markdown::on_task`] is
//! set, and `<id>.table.<n>.more` for the footer of a table cut at [`MAX_TABLE_ROWS`] rows.
//!
//! Links take keyboard focus (Tab, then Enter or Space opens them) and answer the pointer on every
//! row they wrap onto. Tables size their columns to their content and, when that is too wide,
//! wrap columns down to their longest word. [`Markdown`] adds find highlighting, task checkbox
//! callbacks, streaming repair (an unclosed `**`, `` ` `` or link at the end is drawn as already
//! styled, so markers never flash), a caller-kept revision instead of hashing the text, and
//! [`MarkdownLabels`] for the words drawn.
//!
//! Long documents and streaming replies stay cheap: the parse is kept while the text is the same
//! (text appended to it is parsed again only from its last open block), each block's layout is
//! kept while the block, the width and the style are the same, and blocks outside the clip rect
//! only reserve their space. Automation ids exist for the blocks drawn.

use std::borrow::Cow;

use egui::text::TextFormat;
use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, Ui};

mod find;
mod parse;
mod view;

pub use find::{MAX_QUERY, find_ranges};
pub use parse::{inline, parse};

/// [`parse`] for text still streaming in: an unclosed marker in the last block is drawn as
/// already closed (see [`Markdown::streaming`]).
pub fn parse_streaming(md: &str) -> Vec<MdBlock> {
    parse::parse_doc(md, true).blocks
}

/// A run of text with one style.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    pub link: Option<String>,
}

/// A list item's marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Marker {
    Bullet,
    Number(u32),
    Task(bool),
}

/// One block of a document.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MdBlock {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    Item {
        depth: u8,
        marker: Marker,
        spans: Vec<Span>,
    },
    Quote(Vec<Span>),
    /// `closed`: the closing fence has arrived (false while a reply is still streaming).
    Code {
        lang: String,
        text: String,
        closed: bool,
    },
    Rule,
    Table {
        header: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
}

/// Longest document parsed (the rest is shown as one plain paragraph).
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

/// Rows a table draws; the rest are counted in a footer ([`MarkdownLabels::more_rows`]).
pub const MAX_TABLE_ROWS: usize = 500;

/// The words markdown draws, for the app to localize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownLabels {
    /// A code block's copy button.
    pub copy: Cow<'static, str>,
    /// The copy button for a moment after it was pressed.
    pub copied: Cow<'static, str>,
    /// The caption of a code block with no language.
    pub code: Cow<'static, str>,
    /// The footer of a table cut short; `{n}` is replaced by the number of rows not drawn.
    pub more_rows: Cow<'static, str>,
}

impl MarkdownLabels {
    /// English.
    pub const ENGLISH: MarkdownLabels = MarkdownLabels {
        copy: Cow::Borrowed("Copy"),
        copied: Cow::Borrowed("Copied"),
        code: Cow::Borrowed("code"),
        more_rows: Cow::Borrowed("{n} more rows"),
    };
}

static ENGLISH: MarkdownLabels = MarkdownLabels::ENGLISH;

impl Default for MarkdownLabels {
    fn default() -> Self {
        Self::ENGLISH
    }
}

/// The plain text of runs (what Copy puts on the clipboard for a paragraph).
pub fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Colours and fonts, from the app's theme.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownStyle {
    pub text: Color32,
    /// Bold text and headings.
    pub strong: Color32,
    /// Quotes, list markers, a code block's language.
    pub weak: Color32,
    pub link: Color32,
    pub code_text: Color32,
    pub code_bg: Color32,
    pub code_border: Color32,
    pub rule: Color32,
    /// The background of text matching [`Markdown::find`].
    pub find_bg: Color32,
    pub body: FontId,
    pub mono: FontId,
    /// A bold face, when the app has one (otherwise bold is the strong colour).
    pub bold_family: Option<FontFamily>,
    /// Heading sizes for `#`, `##` and `###` (deeper levels use the last).
    pub heading_sizes: [f32; 3],
    pub corner: CornerRadius,
}

impl MarkdownStyle {
    /// From egui's visuals (an app with its own tokens sets the fields itself).
    pub fn from_visuals(v: &egui::Visuals, body_size: f32) -> Self {
        Self {
            text: v.text_color(),
            strong: v.strong_text_color(),
            weak: v.weak_text_color(),
            link: v.hyperlink_color,
            code_text: v.text_color(),
            code_bg: v.extreme_bg_color,
            code_border: v.widgets.noninteractive.bg_stroke.color,
            rule: v.widgets.noninteractive.bg_stroke.color,
            find_bg: v.warn_fg_color.gamma_multiply(0.35),
            body: FontId::proportional(body_size),
            mono: FontId::monospace(body_size * 0.92),
            bold_family: None,
            heading_sizes: [body_size * 1.45, body_size * 1.25, body_size * 1.1],
            corner: CornerRadius::same(4),
        }
    }

    /// The format of a run; `bold` makes every run bold (headings, table headers).
    fn format(&self, s: &Span, bold: bool, font: &FontId, base: Color32) -> TextFormat {
        let mut f = TextFormat { font_id: font.clone(), color: base, ..Default::default() };
        if s.code {
            f.font_id = FontId::new(self.mono.size * font.size / self.body.size.max(1.0), self.mono.family.clone());
            f.color = self.code_text;
            f.background = self.code_bg;
        }
        if s.bold || bold {
            f.color = self.strong;
            if let Some(fam) = &self.bold_family {
                f.font_id.family = fam.clone();
            }
        }
        if s.italic {
            f.italics = true;
        }
        if s.strike {
            f.strikethrough = Stroke::new(1.0, f.color);
        }
        if s.link.is_some() {
            f.color = self.link;
            f.underline = Stroke::new(1.0, self.link);
        }
        f
    }
}

/// What the person did in [`show`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MarkdownResponse {
    /// The code block (counted from 0) whose Copy was pressed.
    pub copied: Option<usize>,
    /// The link that was opened.
    pub opened: Option<String>,
    /// The task checkbox (counted from 0 in document order) that was toggled, and its new state.
    pub task: Option<(usize, bool)>,
    /// How many times [`Markdown::find`]'s query occurs in the document.
    pub matches: usize,
}

fn safe_link(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    u.starts_with("https://") || u.starts_with("http://") || u.starts_with("mailto:")
}

/// Draw `md` at the full available width. `id` prefixes the automation ids and, with the parent
/// ui's id, names what is kept between frames.
pub fn show(ui: &mut Ui, id: &str, md: &str, style: &MarkdownStyle) -> MarkdownResponse {
    Markdown::new(id, md, style).show(ui)
}

/// [`show`] for a caller that keeps a revision of `md` (bumped whenever the text changes): while
/// the revision is the same the text is not even hashed, so a long document costs nothing to keep.
pub fn show_with_revision(ui: &mut Ui, id: &str, revision: u64, md: &str, style: &MarkdownStyle) -> MarkdownResponse {
    Markdown::new(id, md, style).revision(revision).show(ui)
}

/// [`show`] with blocks parsed already. [`show`] keeps its parse itself; this hashes `blocks` each
/// frame to find the ones that changed.
pub fn show_blocks(ui: &mut Ui, id: &str, blocks: &[MdBlock], style: &MarkdownStyle) -> MarkdownResponse {
    Markdown::blocks(id, blocks, style).show(ui)
}

enum Source<'a> {
    Text(&'a str),
    Blocks(&'a [MdBlock]),
}

/// A markdown view with options; [`show`] is `Markdown::new(id, md, style).show(ui)`.
///
/// ```no_run
/// # fn f(ui: &mut egui::Ui, reply: &str, style: &ashlar_ui::markdown::MarkdownStyle, rev: u64) {
/// use ashlar_ui::markdown::Markdown;
/// let r = Markdown::new("reply.12", reply, style).revision(rev).streaming(true).find("needle").show(ui);
/// # }
/// ```
pub struct Markdown<'a> {
    id: &'a str,
    source: Source<'a>,
    style: &'a MarkdownStyle,
    labels: Option<&'a MarkdownLabels>,
    query: &'a str,
    on_task: Option<Box<dyn FnMut(usize, bool) + 'a>>,
    streaming: bool,
    revision: Option<u64>,
}

impl<'a> Markdown<'a> {
    /// Markdown text, parsed (and kept parsed) by the view.
    pub fn new(id: &'a str, md: &'a str, style: &'a MarkdownStyle) -> Self {
        Self::with(id, Source::Text(md), style)
    }

    /// Blocks parsed by the caller (hashed each frame to find the ones that changed).
    pub fn blocks(id: &'a str, blocks: &'a [MdBlock], style: &'a MarkdownStyle) -> Self {
        Self::with(id, Source::Blocks(blocks), style)
    }

    fn with(id: &'a str, source: Source<'a>, style: &'a MarkdownStyle) -> Self {
        Markdown { id, source, style, labels: None, query: "", on_task: None, streaming: false, revision: None }
    }

    /// The caller's revision of the text: while it is unchanged the text is not hashed.
    pub fn revision(mut self, revision: u64) -> Self {
        self.revision = Some(revision);
        self
    }

    /// The text is still streaming in: an unclosed `**`, `*`, `~~`, `` ` `` or link in the last
    /// block is drawn as already closed, and a marker at the very end is hidden.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }

    /// Highlight every occurrence of `query` (ignoring ASCII case); [`MarkdownResponse::matches`]
    /// counts them. An empty query highlights nothing.
    pub fn find(mut self, query: &'a str) -> Self {
        self.query = find::clamp(query);
        self
    }

    /// Make task checkboxes clickable: `f(index, checked)` gets the task (counted from 0 in
    /// document order) and its new state. The app changes the text.
    pub fn on_task(mut self, f: impl FnMut(usize, bool) + 'a) -> Self {
        self.on_task = Some(Box::new(f));
        self
    }

    /// The words drawn (English by default).
    pub fn labels(mut self, labels: &'a MarkdownLabels) -> Self {
        self.labels = Some(labels);
        self
    }

    pub fn show(self, ui: &mut Ui) -> MarkdownResponse {
        let Markdown { id, source, style, labels, query, mut on_task, streaming, revision } = self;
        let opts = view::Opts {
            id,
            style,
            labels: labels.unwrap_or(&ENGLISH),
            query,
            on_task: on_task.as_deref_mut().map(|f| f as &mut dyn FnMut(usize, bool)),
            streaming,
            revision,
        };
        match source {
            Source::Text(md) => view::show(ui, md, opts),
            Source::Blocks(blocks) => view::show_blocks(ui, blocks, opts),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(text: &str) -> Span {
        Span { text: text.into(), ..Span::default() }
    }

    #[test]
    fn blocks() {
        let md = "# Title\n\nSome *soft*\nwrapped text.\n\n- one\n  more\n- [x] done\n  1. sub\n\n```rust\nfn main() {}\n```\n> quoted\n> twice\n\n---\n| a | b |\n|---|:-:|\n| 1 | 2 |\n";
        let b = parse(md);
        assert_eq!(b.first(), Some(&MdBlock::Heading { level: 1, spans: vec![t("Title")] }));
        assert_eq!(
            b.get(1),
            Some(&MdBlock::Paragraph(vec![t("Some "), Span { text: "soft".into(), italic: true, ..Span::default() }, t(" wrapped text.")]))
        );
        assert_eq!(b.get(2), Some(&MdBlock::Item { depth: 0, marker: Marker::Bullet, spans: vec![t("one more")] }));
        assert_eq!(b.get(3), Some(&MdBlock::Item { depth: 0, marker: Marker::Task(true), spans: vec![t("done")] }));
        assert_eq!(b.get(4), Some(&MdBlock::Item { depth: 1, marker: Marker::Number(1), spans: vec![t("sub")] }));
        assert_eq!(b.get(5), Some(&MdBlock::Code { lang: "rust".into(), text: "fn main() {}".into(), closed: true }));
        assert_eq!(b.get(6), Some(&MdBlock::Quote(vec![t("quoted twice")])));
        assert_eq!(b.get(7), Some(&MdBlock::Rule));
        assert_eq!(b.get(8), Some(&MdBlock::Table { header: vec![vec![t("a")], vec![t("b")]], rows: vec![vec![vec![t("1")], vec![t("2")]]] }));
        assert_eq!(b.len(), 9);
    }

    #[test]
    fn streaming_text_never_jumps_back() {
        // an unclosed fence runs to the end; unclosed inline marks show as typed
        assert_eq!(parse("```py\nprint(1)"), vec![MdBlock::Code { lang: "py".into(), text: "print(1)".into(), closed: false }]);
        assert_eq!(inline("a **bol"), vec![t("a **bol")]);
        assert_eq!(inline("run `cargo"), vec![t("run `cargo")]);
        assert_eq!(inline("[half](page"), vec![t("[half](page")]);
    }

    #[test]
    fn inline_styles() {
        let s = inline("**bold** and `code` and ~~gone~~ and snake_case_name and \\*literal\\*");
        assert_eq!(s.first(), Some(&Span { text: "bold".into(), bold: true, ..Span::default() }));
        assert!(s.iter().any(|x| x.code && x.text == "code"));
        assert!(s.iter().any(|x| x.strike && x.text == "gone"));
        assert_eq!(plain(&s), "bold and code and gone and snake_case_name and *literal*");
        let l = inline("see [the docs](https://example.com/a) or https://example.com/b.");
        assert_eq!(l.get(1), Some(&Span { text: "the docs".into(), link: Some("https://example.com/a".into()), ..Span::default() }));
        assert_eq!(l.get(3).and_then(|s| s.link.as_deref()), Some("https://example.com/b"));
        assert_eq!(l.last(), Some(&t(".")));
        assert_eq!(inline("2 * 3 * 4"), vec![t("2 * 3 * 4")], "a lone star is not italic");
        assert!(!safe_link("javascript:alert(1)") && safe_link("https://a.b"));
    }

    #[test]
    fn hostile_input_does_not_panic() {
        for s in ["`", "``", "**", "*", "_", "[", "[]()", "](", "|", "|-|", "#", "####### x", "1.", "- ", "> ", "\\", "~~", "```", "\u{301}*a*"] {
            let _ = parse(s);
            let _ = inline(s);
        }
        let big = "*a ".repeat(50_000);
        assert!(!parse(&big).is_empty());
    }

    #[test]
    fn it_draws_and_copies_a_code_block() {
        let ctx = egui::Context::default();
        let style = MarkdownStyle::from_visuals(&egui::Visuals::dark(), 13.0);
        let md = "## Steps\n1. Open **Main**\n2. See [docs](https://example.com)\n\n```sh\necho hi\n```";
        let mut out = MarkdownResponse::default();
        for _ in 0..2 {
            let _ = crate::test_support::run(&ctx, egui::RawInput::default(), |ui| {
                out = show(ui, "md", md, &style);
            });
        }
        assert_eq!(out, MarkdownResponse::default());
        let ids: Vec<String> = crate::automation::query(&ctx, "md.").into_iter().map(|e| e.id).collect();
        assert!(ids.contains(&"md.code.0".to_string()), "{ids:?}");
        assert!(ids.contains(&"md.code.0.copy".to_string()));
        assert!(ids.contains(&"md.link.0".to_string()));
    }
}
