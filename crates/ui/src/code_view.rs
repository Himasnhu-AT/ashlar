//! A read-only code view: virtualized lines, a line-number gutter, optional wrapping, mouse
//! selection with copy, search marks, and syntax colours from [`ashlar_syntax`].
//!
//! A frame costs O(visible lines). The line index and the lexer state at the start of each line
//! are kept per `(id, revision)`; line states are computed lazily up to the lowest line shown.
//! Each line's galley is cached by `(id, line)` and rebuilt only when the revision, its start
//! state, the width, the font or the theme changes.
//!
//! ```
//! # use ashlar_ui::code_view::CodeView;
//! # fn ui(ui: &mut egui::Ui, source: &str, revision: u64) {
//! let out = CodeView::new("editor.preview", source).lang_for_path("src/main.rs").revision(revision).wrap(false).show(ui);
//! if out.copied {
//!     // the selection is on the clipboard
//! }
//! # }
//! ```
//!
//! Automation: the view registers as `<id>` (a group) and each visible line as `<id>.line.<n>`
//! (1-based, labelled with its text). AccessKit: the view is a read-only document, each visible
//! line a paragraph labelled with its text.

use std::ops::Range;
use std::sync::Arc;

use ashlar_syntax::{Builtin, Highlighter, Kind, State};
use egui::text::{ByteIndex, CCursor, LayoutJob, LayoutSection, TextWrapping};
use egui::text_selection::CCursorRange;
use egui::{Color32, Event, FontId, Galley, Key, Pos2, Rect, TextFormat, Ui, Visuals, pos2, vec2};

use crate::automation::{self, Details, Role};
use crate::cache::{self, hash_of};
use crate::tokens::{self, Tokens};
use crate::virtual_list::{self, MAX_ROWS, RowHeights, VirtualList};

/// Colours for each [`Kind`] of token.
#[derive(Clone, Debug, PartialEq)]
pub struct SyntaxTheme {
    colors: [Color32; Kind::ALL.len()],
}

fn kind_index(kind: Kind) -> usize {
    Kind::ALL.iter().position(|k| *k == kind).unwrap_or(0)
}

impl SyntaxTheme {
    /// A theme derived from the design tokens (accent, status and text colours), so it follows
    /// light and dark mode and any brand accent.
    pub fn from_tokens(t: &Tokens) -> SyntaxTheme {
        let text = t.text.primary;
        let weak = t.text.secondary;
        let mix = |a: Color32, b: Color32, f: f32| a.lerp_to_gamma(b, f);
        let mut theme = SyntaxTheme { colors: [text; Kind::ALL.len()] };
        let pairs = [
            (Kind::Plain, text),
            (Kind::Keyword, t.accent),
            (Kind::Type, mix(t.status.info, t.status.success, 0.5)),
            (Kind::Function, t.status.info),
            (Kind::String, t.status.success),
            (Kind::Comment, mix(weak, t.text.disabled, 0.3)),
            (Kind::Number, t.status.warning),
            (Kind::Constant, mix(t.status.warning, t.status.danger, 0.4)),
            (Kind::Punctuation, mix(text, weak, 0.6)),
            (Kind::Operator, mix(text, t.accent, 0.35)),
            (Kind::Attribute, mix(t.status.warning, t.accent, 0.4)),
            (Kind::Property, mix(t.status.info, text, 0.3)),
            (Kind::Variable, mix(t.status.danger, t.accent, 0.3)),
            (Kind::Tag, t.status.danger),
            (Kind::Heading, t.accent),
            (Kind::Emphasis, mix(text, t.accent, 0.2)),
            (Kind::Link, t.status.info),
            (Kind::Inserted, t.status.success),
            (Kind::Deleted, t.status.danger),
            (Kind::Meta, weak),
        ];
        for (kind, color) in pairs {
            theme.set(kind, color);
        }
        theme
    }

    /// The colour of `kind`.
    pub fn color(&self, kind: Kind) -> Color32 {
        self.colors.get(kind_index(kind)).copied().unwrap_or(Color32::PLACEHOLDER)
    }

    /// Overrides the colour of `kind`.
    pub fn set(&mut self, kind: Kind, color: Color32) {
        if let Some(c) = self.colors.get_mut(kind_index(kind)) {
            *c = color;
        }
    }

    /// A hash of the colours, for galley cache revisions.
    pub fn revision(&self) -> u64 {
        hash_of(&self.colors.map(|c| c.to_array()))
    }
}

/// A highlighted layout job for one block of code (used by Markdown code fences): each span
/// becomes a section in `font` coloured by `theme`. Unknown languages give one plain section.
pub fn highlight_job(lang: Option<&str>, text: &str, font: FontId, theme: &SyntaxTheme) -> LayoutJob {
    let spans = ashlar_syntax::highlight(lang, text);
    job_from_spans(text, &spans, font, theme)
}

fn job_from_spans(text: &str, spans: &[ashlar_syntax::Span], font: FontId, theme: &SyntaxTheme) -> LayoutJob {
    let sections = spans
        .iter()
        .map(|(range, kind)| LayoutSection {
            leading_space: 0.0,
            byte_range: ByteIndex(range.start)..ByteIndex(range.end),
            format: TextFormat { font_id: font.clone(), color: theme.color(*kind), ..Default::default() },
        })
        .collect();
    LayoutJob { text: text.to_owned(), sections, break_on_newline: true, ..Default::default() }
}

/// What the view reports this frame.
pub struct CodeViewResponse {
    /// The view's viewport.
    pub response: egui::Response,
    /// The selected byte range of the text (never empty).
    pub selection: Option<Range<usize>>,
    /// The selection was copied to the clipboard this frame (Cmd/Ctrl+C).
    pub copied: bool,
    /// Line (0-based) pressed with the pointer this frame.
    pub clicked_line: Option<usize>,
    /// Lines (0-based) laid out this frame.
    pub visible: Range<usize>,
}

/// A read-only, virtualized code view. See the [module docs](self).
pub struct CodeView<'a> {
    id: &'a str,
    text: &'a str,
    lang: Option<&'a str>,
    revision: u64,
    wrap: bool,
    line_numbers: bool,
    theme: Option<&'a SyntaxTheme>,
    highlighter: &'a dyn Highlighter,
    marks: &'a [Range<usize>],
    scroll_to_line: Option<usize>,
    max_height: Option<f32>,
}

impl<'a> CodeView<'a> {
    pub fn new(id: &'a str, text: &'a str) -> Self {
        CodeView {
            id,
            text,
            lang: None,
            revision: 0,
            wrap: false,
            line_numbers: true,
            theme: None,
            highlighter: &Builtin,
            marks: &[],
            scroll_to_line: None,
            max_height: None,
        }
    }

    /// The language (a name, alias or extension; see [`ashlar_syntax::Lang::from_name`]). Unknown
    /// or `None` shows plain text.
    pub fn lang(mut self, lang: Option<&'a str>) -> Self {
        self.lang = lang;
        self
    }

    /// The language for a file path (`"src/main.rs"`).
    pub fn lang_for_path(mut self, path: &str) -> Self {
        self.lang = ashlar_syntax::lang_for_path(path);
        self
    }

    /// Bump when the text changes. (A change of the text's address or length is noticed anyway;
    /// an edit that keeps both needs a new revision.)
    pub fn revision(mut self, revision: u64) -> Self {
        self.revision = revision;
        self
    }

    /// Wrap long lines at the view's width (off: long lines are clipped).
    pub fn wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }

    /// Show the line-number gutter (on by default).
    pub fn line_numbers(mut self, yes: bool) -> Self {
        self.line_numbers = yes;
        self
    }

    /// Token colours (default: [`SyntaxTheme::from_tokens`] of the current tokens).
    pub fn theme(mut self, theme: &'a SyntaxTheme) -> Self {
        self.theme = Some(theme);
        self
    }

    /// The highlighting engine (default: [`Builtin`]).
    pub fn highlighter(mut self, h: &'a dyn Highlighter) -> Self {
        self.highlighter = h;
        self
    }

    /// Byte ranges to mark (search hits), sorted by start and not overlapping.
    pub fn marks(mut self, marks: &'a [Range<usize>]) -> Self {
        self.marks = marks;
        self
    }

    /// Scroll so this line (0-based) is visible.
    pub fn scroll_to_line(mut self, line: Option<usize>) -> Self {
        self.scroll_to_line = line;
        self
    }

    /// Cap the view's height (default: the available height).
    pub fn max_height(mut self, h: f32) -> Self {
        self.max_height = Some(h);
        self
    }

    pub fn show(self, ui: &mut Ui) -> CodeViewResponse {
        show(self, ui)
    }
}

/// The line index and lexer states of one text revision.
#[derive(Clone, Default)]
struct Doc {
    key: u64,
    /// Byte offset of each line start, then `text.len()`.
    starts: Vec<usize>,
    /// The lexer state at the start of lines `0..states.len()`.
    states: Vec<State>,
}

impl Doc {
    fn build(key: u64, text: &str) -> Doc {
        let mut starts = Vec::with_capacity(text.len() / 32 + 2);
        starts.push(0);
        for (i, _) in text.match_indices('\n') {
            if starts.len() > MAX_ROWS {
                break;
            }
            starts.push(i + 1);
        }
        starts.push(text.len());
        Doc { key, starts, states: vec![State::START] }
    }
}

fn line_count(starts: &[usize]) -> usize {
    starts.len().saturating_sub(1).max(1)
}

/// The byte range of line `i` without its line ending.
fn line_range(starts: &[usize], text: &str, i: usize) -> Range<usize> {
    let s = starts.get(i).copied().unwrap_or(text.len()).min(text.len());
    let e = starts.get(i + 1).copied().unwrap_or(text.len()).clamp(s, text.len());
    let line = text.get(s..e).unwrap_or("");
    let body = line.strip_suffix('\n').unwrap_or(line);
    let body = body.strip_suffix('\r').unwrap_or(body);
    s..s + body.len()
}

/// Line starts, the states known so far, and what lexes the rest.
struct Lines<'d> {
    starts: &'d [usize],
    states: &'d mut Vec<State>,
    text: &'d str,
    lang: &'d str,
    hl: &'d dyn Highlighter,
}

impl Lines<'_> {
    /// The state line `i` starts in, lexing forward from the last known line.
    fn state(&mut self, i: usize, scratch: &mut Vec<ashlar_syntax::Span>) -> State {
        let target = i.min(line_count(self.starts));
        if self.states.is_empty() {
            self.states.push(State::START);
        }
        while self.states.len() <= target {
            let k = self.states.len() - 1;
            let prev = self.states.last().copied().unwrap_or_default();
            let r = line_range(self.starts, self.text, k);
            scratch.clear();
            let next = self.hl.highlight_line(self.lang, self.text.get(r).unwrap_or(""), prev, scratch);
            self.states.push(next);
        }
        self.states.get(target).copied().unwrap_or_default()
    }
}

/// Mouse selection, as byte offsets into the text.
#[derive(Clone, Copy, Default)]
struct Sel {
    anchor: usize,
    head: usize,
    dragging: bool,
}

impl Sel {
    fn range(&self) -> Option<Range<usize>> {
        let (a, b) = (self.anchor.min(self.head), self.anchor.max(self.head));
        (b > a).then_some(a..b)
    }
}

/// A line's text for labels: at most 256 bytes (a minified file can have megabyte lines).
fn short(line: &str) -> &str {
    let mut end = line.len().min(256);
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    line.get(..end).unwrap_or("")
}

/// The char index of byte offset `byte` in `line` (clamped).
fn char_index(line: &str, byte: usize) -> usize {
    let mut b = byte.min(line.len());
    while b > 0 && !line.is_char_boundary(b) {
        b -= 1;
    }
    line.get(..b).map_or(0, |s| s.chars().count())
}

/// The byte offset of char index `ci` in `line` (clamped).
fn byte_index(line: &str, ci: usize) -> usize {
    line.char_indices().nth(ci).map_or(line.len(), |(b, _)| b)
}

fn show(view: CodeView<'_>, ui: &mut Ui) -> CodeViewResponse {
    let t = tokens::current(ui.ctx());
    let base = ui.make_persistent_id(view.id);
    let text = view.text;
    let lang = view.lang.unwrap_or("");
    let font = t.mono();
    let (line_h, char_w) = ui.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, '0')));
    let (line_h, char_w) = (line_h.max(1.0), char_w.max(1.0));
    let own_theme;
    let theme = match view.theme {
        Some(th) => th,
        None => {
            own_theme = SyntaxTheme::from_tokens(&t);
            &own_theme
        }
    };

    let doc_id = base.with("doc");
    let key = hash_of(&(view.revision, text.as_ptr() as usize, text.len(), lang));
    let mut doc = ui.data_mut(|d| d.remove_temp::<Doc>(doc_id)).filter(|d| d.key == key).unwrap_or_else(|| Doc::build(key, text));
    let mut states = std::mem::take(&mut doc.states);
    let lines = line_count(&doc.starts);

    // geometry
    let pad = t.space.sm;
    let digits = lines.to_string().len().max(2) as f32;
    let gutter_w = if view.line_numbers { digits * char_w + 2.0 * pad } else { 0.0 };
    let text_w = (ui.available_width() - gutter_w - 3.0 * pad - ui.spacing().scroll.bar_width).max(char_w);
    let cols = ((text_w / char_w).floor() as usize).max(1);
    let wrap_w = if view.wrap { cols as f32 * char_w + 0.5 } else { f32::INFINITY };
    let style_rev = hash_of(&(theme.revision(), font.size.to_bits(), wrap_w.to_bits(), t.dark));

    // input, read once
    let sel_id = base.with("sel");
    let mut sel: Sel = ui.data(|d| d.get_temp(sel_id)).unwrap_or_default();
    let (pressed, down, pointer, shift) =
        ui.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.interact_pos(), i.modifiers.shift));
    let mut clicked_line = None;

    let height_fn = |i: usize| {
        let r = line_range(&doc.starts, text, i);
        let chars = text.get(r).map_or(0, |s| s.chars().count());
        chars.div_ceil(cols).max(1) as f32 * line_h
    };
    let heights = if view.wrap {
        RowHeights::Variable { revision: hash_of(&(key, cols, line_h.to_bits())), height: &height_fn }
    } else {
        RowHeights::Uniform(line_h)
    };
    let mut list = VirtualList::new(view.id, lines, heights).scroll_to(view.scroll_to_line);
    if let Some(h) = view.max_height {
        list = list.max_height(h);
    }

    let sel_range = sel.range();
    let mut sel_visuals: Option<Visuals> = None;
    let mut mark_visuals: Option<Visuals> = None;
    let mut scratch = Vec::new();
    let mut known = Lines { starts: &doc.starts, states: &mut states, text, lang, hl: view.highlighter };
    let kind = virtual_list::Kind { role: Role::Group, rows: false };
    let out = virtual_list::show_rows(ui, base, &list, kind, None, |ui, row| {
        let i = row.index;
        let range = line_range(known.starts, text, i);
        let line = text.get(range.clone()).unwrap_or("");
        let state = known.state(i, &mut scratch);
        let rev = hash_of(&(key, style_rev, state.to_bits()));
        let mut galley: Arc<Galley> = cache::galley(ui, (base, "line", i), rev, || {
            scratch.clear();
            let _ = view.highlighter.highlight_line(lang, line, state, &mut scratch);
            let mut job = job_from_spans(line, &scratch, font.clone(), theme);
            job.wrap = TextWrapping { max_width: wrap_w, break_anywhere: true, ..Default::default() };
            job
        });
        let rect = row.rect;
        let origin = pos2(rect.left() + pad + gutter_w, rect.top());

        // pointer: press starts a selection, a drag extends it
        if let Some(p) = pointer {
            let offset_at = |p: Pos2| {
                let local = (p - origin).max(vec2(0.0, 0.0));
                range.start + byte_index(line, galley.cursor_from_pos(local).index.0)
            };
            if pressed && rect.contains(p) {
                let off = offset_at(p);
                if !shift {
                    sel.anchor = off;
                }
                sel.head = off;
                sel.dragging = true;
                clicked_line = Some(i);
                ui.memory_mut(|m| m.request_focus(base));
            } else if sel.dragging && down && p.y >= rect.top() && p.y < rect.bottom() {
                sel.head = offset_at(p);
            }
        }

        // marks, then the selection, painted into the galley's background
        let first = view.marks.partition_point(|m| m.end <= range.start);
        for m in view.marks.iter().skip(first).take_while(|m| m.start < range.end.max(range.start + 1)).take(256) {
            let vis = mark_visuals.get_or_insert_with(|| {
                let mut v = ui.visuals().clone();
                v.selection.bg_fill = t.status.warning.gamma_multiply(0.35);
                v
            });
            paint_range(&mut galley, vis, line, &range, m);
        }
        if let Some(s) = &sel_range {
            let vis = sel_visuals.get_or_insert_with(|| {
                let mut v = ui.visuals().clone();
                v.selection.bg_fill = t.selection;
                v
            });
            paint_range(&mut galley, vis, line, &range, s);
        }

        let painter = ui.painter();
        if view.line_numbers {
            let gutter = Rect::from_min_max(pos2(rect.left(), rect.top()), pos2(rect.left() + gutter_w, rect.bottom()));
            painter.rect_filled(gutter, 0, t.surface.sunken);
            let n = i + 1;
            let num = cache::galley(ui, (base, "ln", n), hash_of(&(font.size.to_bits(), t.dark)), || {
                LayoutJob::simple_singleline(n.to_string(), font.clone(), t.text.disabled)
            });
            painter.galley(pos2(gutter.right() - pad - num.size().x, rect.top()), num, t.text.disabled);
        }
        painter.galley(origin, galley, theme.color(Kind::Plain));

        let label = short(line);
        let row_id = base.with(("row", i));
        ui.ctx().accesskit_node_builder(row_id, |node| {
            node.set_role(egui::accesskit::Role::Paragraph);
            node.set_label(label);
        });
        automation::register_fmt(ui, rect, format_args!("{}.line.{}", view.id, i + 1), label, Role::Label, true, Details::default());
    });
    doc.states = states;

    // keyboard: select all and copy, while the view has the focus
    let focused = out.response.has_focus();
    let mut copied = false;
    if focused {
        let (all, copy) = ui.input(|i| {
            let cmd = i.modifiers.command;
            (cmd && i.key_pressed(Key::A), i.events.iter().any(|e| matches!(e, Event::Copy)) || (cmd && i.key_pressed(Key::C)))
        });
        if all {
            sel = Sel { anchor: 0, head: text.len(), dragging: false };
        }
        if copy && let Some(s) = sel.range().and_then(|r| text.get(r)) {
            ui.ctx().copy_text(s.to_owned());
            copied = true;
        }
    }
    if !down {
        sel.dragging = false;
    }
    let selection = sel.range();
    ui.data_mut(|d| {
        d.insert_temp(sel_id, sel);
        d.insert_temp(doc_id, doc);
    });
    ui.ctx().accesskit_node_builder(out.response.id, |node| {
        node.set_role(egui::accesskit::Role::Document);
        node.set_label(view.id);
        node.set_read_only();
    });
    CodeViewResponse { response: out.response, selection, copied, clicked_line, visible: out.visible }
}

/// Paints the part of byte range `r` (of the whole text) that falls on this line.
fn paint_range(galley: &mut Arc<Galley>, vis: &Visuals, line: &str, line_range: &Range<usize>, r: &Range<usize>) {
    let from = r.start.max(line_range.start).saturating_sub(line_range.start);
    let to = r.end.min(line_range.end).saturating_sub(line_range.start);
    if to <= from {
        return;
    }
    let range = CCursorRange::two(CCursor::new(char_index(line, from)), CCursor::new(char_index(line, to)));
    egui::text_selection::visuals::paint_text_selection(galley, vis, &range, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_index_and_ranges() {
        let text = "a\r\nbc\n\nlast";
        let d = Doc::build(1, text);
        assert_eq!(line_count(&d.starts), 4);
        assert_eq!(line_range(&d.starts, text, 0), 0..1);
        assert_eq!(line_range(&d.starts, text, 1), 3..5);
        assert_eq!(line_range(&d.starts, text, 2), 6..6);
        assert_eq!(line_range(&d.starts, text, 3), 7..11);
        assert_eq!(line_range(&d.starts, text, 99), 11..11);
        assert_eq!(line_count(&Doc::build(1, "x\n").starts), 2);
        assert_eq!(line_count(&Doc::build(1, "").starts), 1);
    }

    #[test]
    fn char_and_byte_indices_round_trip() {
        let s = "aé😀b";
        assert_eq!(char_index(s, 3), 2);
        assert_eq!(char_index(s, 4), 2); // inside the emoji: clamped back
        assert_eq!(byte_index(s, 3), 7);
        assert_eq!(byte_index(s, 99), s.len());
    }

    #[test]
    fn states_are_lazy_and_carry_comments() {
        let text = "/* a\nb */\nfn x";
        let mut d = Doc::build(1, text);
        let mut scratch = Vec::new();
        assert_eq!(d.states.len(), 1);
        let mut lines = Lines { starts: &d.starts, states: &mut d.states, text, lang: "rust", hl: &Builtin };
        let s1 = lines.state(1, &mut scratch);
        assert_ne!(s1, State::START);
        assert_eq!(lines.state(2, &mut scratch), State::START);
        assert_eq!(d.states.len(), 3);
    }

    #[test]
    fn theme_from_tokens_differs_by_kind() {
        let th = SyntaxTheme::from_tokens(&Tokens::dark());
        assert_ne!(th.color(Kind::Keyword), th.color(Kind::Plain));
        assert_ne!(th.color(Kind::String), th.color(Kind::Comment));
        let mut th2 = th.clone();
        th2.set(Kind::Keyword, Color32::RED);
        assert_eq!(th2.color(Kind::Keyword), Color32::RED);
        assert_ne!(th.revision(), th2.revision());
    }
}
