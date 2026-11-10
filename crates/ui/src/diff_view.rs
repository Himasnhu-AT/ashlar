//! A diff view over [`ashlar_diff::FileDiff`]s: unified or split, virtualized, syntax-coloured per
//! file language, with word-level emphasis, collapsed unchanged ranges, a sticky file header,
//! line selection and hunk navigation.
//!
//! A frame costs O(visible rows). The row model (files, hunk headers, lines, collapsed ranges)
//! is built once per `(revision, mode, expansion)` and kept by the view. Lexer states run
//! lazily within each hunk, so jumping to the end of a long diff highlights one hunk, not the
//! whole diff. Row galleys are cached and rebuilt only when the model, the theme or the font
//! changes.
//!
//! ```
//! # use ashlar_ui::diff_view::{DiffMode, DiffView, DiffViewState};
//! # fn ui(ui: &mut egui::Ui, state: &mut DiffViewState, files: &[ashlar_diff::FileDiff], revision: u64) {
//! let out = DiffView::new("review.diff", state, files, revision).mode(DiffMode::Split).show(ui);
//! for line in &out.selected_lines {
//!     // comment on `line.file`, `line.new_no`…
//! }
//! # }
//! ```
//!
//! Input: click selects a row, Shift-click and Shift+arrows extend, ⌘-click toggles. `N` / `F7`
//! moves to the next hunk, `P` / Shift+`F7` to the previous one. Clicking (or Enter on) a
//! collapsed range shows [`EXPAND_STEP`] more lines, Alt-click shows all of them. Clicking a file
//! header (or Enter on it) folds the file; clicking the sticky header jumps to its file.
//!
//! Automation (visible rows only, indices 0-based): `<id>.file.<f>` (file header),
//! `<id>.file.<f>.hunk.<h>` (hunk header), `<id>.file.<f>.hunk.<h>.line.<k>` (line `k` of the
//! hunk; in split mode the left line, else the right), `<id>.file.<f>.gap.<g>` (collapsed range),
//! `<id>.file.<f>.gap.<g>.line.<n>` (expanded line, `n` its new line number) and `<id>.header`
//! (the sticky header). AccessKit: the view is a read-only document; file and hunk headers are
//! headings, collapsed ranges buttons, lines paragraphs labelled "added 12: text".

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use ashlar_diff::{FileDiff, FileStatus, Gap, LineKind};
use ashlar_syntax::{Builtin, Highlighter, Kind, State};
use egui::accesskit::Role as NodeRole;
use egui::text::{ByteIndex, LayoutJob, LayoutSection, TextWrapping};
use egui::{Color32, FontId, Galley, Key, Rect, Sense, TextFormat, Ui, pos2, vec2};

use crate::automation::{self, Details, Role};
use crate::cache::{self, hash_of};
use crate::code_view::SyntaxTheme;
use crate::selection::Selection;
use crate::tokens::{self, Tokens};
use crate::virtual_list::{self, Header, MAX_ROWS, RowHeights, VirtualList};

/// Lines one click on a collapsed range reveals.
pub const EXPAND_STEP: u32 = 20;

/// Bytes of one line that are laid out; the rest is not shown.
const MAX_LINE_BYTES: usize = 2000;

/// Most entries [`DiffResponse::selected_lines`] lists.
pub const MAX_SELECTED_LINES: usize = 100_000;

/// Longest walk back to the start of a hunk when computing a lexer state.
const MAX_STATE_WALK: usize = 1_000_000;

const NONE: u32 = u32::MAX;

/// How lines are laid out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DiffMode {
    /// One column: removed lines above the added lines that replace them.
    #[default]
    Unified,
    /// Old text left, new text right, changed lines side by side.
    Split,
}

/// Words the view shows (localized by the app).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLabels {
    /// A collapsed range that can be expanded; `{n}` is the number of lines one click shows.
    pub expand: String,
    /// The size of a collapsed range; `{n}` is the number of hidden lines.
    pub hidden: String,
    /// In place of a binary file's lines.
    pub binary: String,
    /// Spoken before an added and a removed line's number.
    pub added: String,
    pub removed: String,
}

impl Default for DiffLabels {
    fn default() -> Self {
        DiffLabels {
            expand: "Expand {n} lines".into(),
            hidden: "{n} unchanged lines".into(),
            binary: "Binary file not shown".into(),
            added: "added".into(),
            removed: "removed".into(),
        }
    }
}

/// The view's state, kept by the app: selection, expanded ranges, folded files.
#[derive(Clone, Debug, Default)]
pub struct DiffViewState {
    /// Selected rows (row indices of the current layout).
    pub selection: Selection,
    expanded: HashMap<(usize, usize), u32>,
    collapsed: HashSet<usize>,
    /// Bumped on every change that alters the rows.
    version: u64,
    revision: Option<u64>,
}

impl DiffViewState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows `lines` more lines of collapsed range `gap` of `file` (see [`FileDiff::gaps`]).
    pub fn expand(&mut self, file: usize, gap: usize, lines: u32) {
        let e = self.expanded.entry((file, gap)).or_insert(0);
        *e = e.saturating_add(lines);
        self.version = self.version.wrapping_add(1);
    }

    /// Lines of range `gap` of `file` shown so far.
    pub fn expanded(&self, file: usize, gap: usize) -> u32 {
        self.expanded.get(&(file, gap)).copied().unwrap_or(0)
    }

    /// Folds or unfolds a file to its header.
    pub fn set_file_collapsed(&mut self, file: usize, collapsed: bool) {
        let changed = if collapsed { self.collapsed.insert(file) } else { self.collapsed.remove(&file) };
        if changed {
            self.version = self.version.wrapping_add(1);
        }
    }

    pub fn is_file_collapsed(&self, file: usize) -> bool {
        self.collapsed.contains(&file)
    }
}

/// One selected line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectedLine {
    pub file: usize,
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
}

/// What the view reports this frame.
pub struct DiffResponse {
    pub response: egui::Response,
    /// The selected lines in order (headers and collapsed ranges are left out), at most
    /// [`MAX_SELECTED_LINES`].
    pub selected_lines: Vec<SelectedLine>,
    /// A collapsed range expanded this frame: `(file, gap)`.
    pub expanded: Option<(usize, usize)>,
    /// A file folded or unfolded this frame.
    pub toggled_file: Option<usize>,
    /// The hunk keyboard navigation moved to this frame: `(file, hunk)`.
    pub hunk: Option<(usize, usize)>,
    /// The file the sticky header shows.
    pub top_file: Option<usize>,
    /// Rows laid out this frame.
    pub visible: Range<usize>,
}

/// A diff view. See the [module docs](self).
pub struct DiffView<'a> {
    id: &'a str,
    state: &'a mut DiffViewState,
    files: &'a [FileDiff],
    revision: u64,
    mode: DiffMode,
    theme: Option<&'a SyntaxTheme>,
    labels: Option<&'a DiffLabels>,
    highlighter: &'a dyn Highlighter,
    max_height: Option<f32>,
}

impl<'a> DiffView<'a> {
    /// `revision` changes whenever `files` change (it keys every cache; expansions and the
    /// selection reset when it changes).
    pub fn new(id: &'a str, state: &'a mut DiffViewState, files: &'a [FileDiff], revision: u64) -> Self {
        DiffView { id, state, files, revision, mode: DiffMode::Unified, theme: None, labels: None, highlighter: &Builtin, max_height: None }
    }

    pub fn mode(mut self, mode: DiffMode) -> Self {
        self.mode = mode;
        self
    }

    /// Syntax colours (default: [`SyntaxTheme::from_tokens`]).
    pub fn theme(mut self, theme: &'a SyntaxTheme) -> Self {
        self.theme = Some(theme);
        self
    }

    pub fn labels(mut self, labels: &'a DiffLabels) -> Self {
        self.labels = Some(labels);
        self
    }

    /// A different highlighting engine (default: [`Builtin`]).
    pub fn highlighter(mut self, h: &'a dyn Highlighter) -> Self {
        self.highlighter = h;
        self
    }

    /// Caps the view's height (default: the available height).
    pub fn max_height(mut self, h: f32) -> Self {
        self.max_height = Some(h);
        self
    }

    pub fn show(self, ui: &mut Ui) -> DiffResponse {
        show(self, ui)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    File,
    /// In place of a binary file's lines.
    Note,
    Hunk,
    Gap,
    /// An expanded line of a collapsed range.
    Ctx,
    Line,
}

/// One row of the layout. `item` is the hunk (`Hunk`, `Line`) or gap (`Gap`, `Ctx`) index.
/// `Line`: `a` and `b` are line indices in the hunk (unified: `a` only; split: left and right).
/// `Ctx`: `a` is the new and `b` the old line number. `Gap`: `a` is the number of hidden lines.
#[derive(Clone, Copy, Debug)]
struct RowRef {
    kind: RowKind,
    file: u32,
    item: u32,
    a: u32,
    b: u32,
}

#[derive(Clone, Default)]
struct Model {
    key: u64,
    rows: Vec<RowRef>,
    /// Rows of hunk headers, ascending.
    hunk_rows: Vec<usize>,
    /// Lexer states (old side, new side) at the start of each row, filled lazily.
    states: Vec<Option<(State, State)>>,
    gaps: Vec<Vec<Gap>>,
    langs: Vec<&'static str>,
    /// Lines added and removed per file.
    stats: Vec<(usize, usize)>,
    /// Digits of the largest line number.
    digits: usize,
}

fn to_u32(x: usize) -> u32 {
    u32::try_from(x).unwrap_or(NONE)
}

impl Model {
    fn build(key: u64, files: &[FileDiff], state: &DiffViewState, mode: DiffMode) -> Model {
        let mut m = Model { key, ..Model::default() };
        let mut max_no = 0u32;
        for (fi, f) in files.iter().enumerate() {
            if m.rows.len() >= MAX_ROWS {
                break;
            }
            let file = to_u32(fi);
            m.rows.push(RowRef { kind: RowKind::File, file, item: 0, a: 0, b: 0 });
            m.langs.push(ashlar_syntax::lang_for_path(f.path()).unwrap_or(""));
            m.stats.push(f.stats());
            let gaps = f.gaps();
            if let Some(h) = f.hunks.last() {
                max_no = max_no.max(h.old_range().end).max(h.new_range().end);
            }
            max_no = max_no.max(f.source_len().unwrap_or(0));
            if state.collapsed.contains(&fi) {
                m.gaps.push(gaps);
                continue;
            }
            if f.binary {
                m.rows.push(RowRef { kind: RowKind::Note, file, item: 0, a: 0, b: 0 });
            }
            let mut gi = 0usize;
            for (hi, h) in f.hunks.iter().enumerate() {
                while let Some(g) = gaps.get(gi).filter(|g| g.before_hunk <= hi) {
                    m.push_gap(file, gi, g, state.expanded(fi, gi));
                    gi += 1;
                }
                if m.rows.len() >= MAX_ROWS {
                    break;
                }
                m.hunk_rows.push(m.rows.len());
                let item = to_u32(hi);
                m.rows.push(RowRef { kind: RowKind::Hunk, file, item, a: 0, b: 0 });
                let line = |a: usize, b: Option<usize>| RowRef { kind: RowKind::Line, file, item, a: to_u32(a), b: b.map_or(NONE, to_u32) };
                let lines = &h.lines;
                let kind = |i: usize| lines.get(i).map(|l| l.kind);
                let mut i = 0usize;
                while i < lines.len() && m.rows.len() < MAX_ROWS {
                    match (mode, kind(i)) {
                        (DiffMode::Unified, _) => {
                            m.rows.push(line(i, None));
                            i += 1;
                        }
                        (DiffMode::Split, Some(LineKind::Context)) => {
                            m.rows.push(line(i, Some(i)));
                            i += 1;
                        }
                        (DiffMode::Split, _) => {
                            let removed = (i..lines.len()).take_while(|&k| kind(k) == Some(LineKind::Removed)).count();
                            let added = (i + removed..lines.len()).take_while(|&k| kind(k) == Some(LineKind::Added)).count();
                            for k in 0..removed.max(added) {
                                let a = if k < removed { to_u32(i + k) } else { NONE };
                                let b = if k < added { to_u32(i + removed + k) } else { NONE };
                                m.rows.push(RowRef { kind: RowKind::Line, file, item, a, b });
                            }
                            i += (removed + added).max(1);
                        }
                    }
                }
            }
            while let Some(g) = gaps.get(gi) {
                m.push_gap(file, gi, g, state.expanded(fi, gi));
                gi += 1;
            }
            m.gaps.push(gaps);
        }
        m.rows.truncate(MAX_ROWS);
        m.states = vec![None; m.rows.len()];
        m.digits = max_no.to_string().len().max(3);
        m
    }

    fn push_gap(&mut self, file: u32, gi: usize, g: &Gap, revealed: u32) {
        let shown = if g.expandable { revealed.min(g.len) } else { 0 };
        let item = to_u32(gi);
        for k in 0..shown {
            if self.rows.len() >= MAX_ROWS {
                return;
            }
            let (a, b) = (g.new_start.saturating_add(k), g.old_start.saturating_add(k));
            self.rows.push(RowRef { kind: RowKind::Ctx, file, item, a, b });
        }
        if shown < g.len {
            self.rows.push(RowRef { kind: RowKind::Gap, file, item, a: g.len - shown, b: 0 });
        }
    }

    /// Whether row `j` starts a run of lines (a hunk's or an expanded range's).
    fn segment_start(&self, j: usize) -> bool {
        let (Some(cur), Some(prev)) = (self.rows.get(j), j.checked_sub(1).and_then(|p| self.rows.get(p))) else { return true };
        prev.kind != cur.kind || prev.file != cur.file || prev.item != cur.item
    }

    /// The lexer states at the start of row `i`, computed from the start of its run.
    fn state(
        &mut self,
        files: &[FileDiff],
        mode: DiffMode,
        hl: &dyn Highlighter,
        i: usize,
        scratch: &mut Vec<ashlar_syntax::Span>,
    ) -> (State, State) {
        if let Some(s) = self.states.get(i).copied().flatten() {
            return s;
        }
        let mut j = i;
        let mut walked = 0usize;
        while walked < MAX_STATE_WALK && !self.segment_start(j) && self.states.get(j).copied().flatten().is_none() {
            j -= 1;
            walked += 1;
        }
        let mut st = self.states.get(j).copied().flatten().unwrap_or_default();
        let lang = self.rows.get(i).and_then(|r| self.langs.get(r.file as usize)).copied().unwrap_or("");
        for r in j..i {
            if let Some(s) = self.states.get_mut(r) {
                *s = Some(st);
            }
            let (old, new) = self.rows.get(r).map_or((None, None), |row| texts(files, mode, row));
            if let Some(t) = old {
                scratch.clear();
                st.0 = hl.highlight_line(lang, display(t), st.0, scratch);
            }
            if let Some(t) = new {
                scratch.clear();
                st.1 = hl.highlight_line(lang, display(t), st.1, scratch);
            }
        }
        if let Some(s) = self.states.get_mut(i) {
            *s = Some(st);
        }
        st
    }
}

fn hunk_line<'f>(files: &'f [FileDiff], row: &RowRef, idx: u32) -> Option<&'f ashlar_diff::Line> {
    if idx == NONE {
        return None;
    }
    files.get(row.file as usize)?.hunks.get(row.item as usize)?.lines.get(idx as usize)
}

/// The text a row adds to the old and to the new side's lexer.
fn texts<'f>(files: &'f [FileDiff], mode: DiffMode, row: &RowRef) -> (Option<&'f str>, Option<&'f str>) {
    match row.kind {
        RowKind::Ctx => {
            let t = files.get(row.file as usize).and_then(|f| f.source_line(row.a));
            (t, t)
        }
        RowKind::Line => match mode {
            DiffMode::Unified => match hunk_line(files, row, row.a) {
                Some(l) => {
                    let t = Some(l.text.as_str());
                    match l.kind {
                        LineKind::Context => (t, t),
                        LineKind::Removed => (t, None),
                        LineKind::Added => (None, t),
                    }
                }
                None => (None, None),
            },
            DiffMode::Split => (hunk_line(files, row, row.a).map(|l| l.text.as_str()), hunk_line(files, row, row.b).map(|l| l.text.as_str())),
        },
        _ => (None, None),
    }
}

/// The part of a line that is laid out: no trailing `\r`, at most [`MAX_LINE_BYTES`].
fn display(text: &str) -> &str {
    let t = text.strip_suffix('\r').unwrap_or(text);
    if t.len() <= MAX_LINE_BYTES {
        return t;
    }
    let mut end = MAX_LINE_BYTES;
    while end > 0 && !t.is_char_boundary(end) {
        end -= 1;
    }
    t.get(..end).unwrap_or("")
}

fn clip_label(s: &str) -> &str {
    let s = s.trim_end_matches('\r');
    match s.char_indices().nth(160) {
        Some((i, _)) => s.get(..i).unwrap_or(s),
        None => s,
    }
}

/// A syntax-coloured job with `emphasis` ranges on `emph_bg`.
fn line_job(text: &str, spans: &[ashlar_syntax::Span], emphasis: &[Range<usize>], font: &FontId, theme: &SyntaxTheme, emph_bg: Color32) -> LayoutJob {
    let mut sections = Vec::with_capacity(spans.len() + 2 * emphasis.len());
    let mut e = emphasis.iter().peekable();
    for (r, kind) in spans {
        let color = theme.color(*kind);
        let end = r.end.min(text.len());
        let mut pos = r.start.min(end);
        let mut guard = 0usize;
        while pos < end && guard < 4 * (emphasis.len() + 1) {
            guard += 1;
            while e.peek().is_some_and(|x| x.end <= pos) {
                e.next();
            }
            let (next, bg) = match e.peek() {
                Some(x) if x.start <= pos => (x.end.min(end), emph_bg),
                Some(x) => (x.start.min(end), Color32::TRANSPARENT),
                None => (end, Color32::TRANSPARENT),
            };
            let next = if next <= pos || !text.is_char_boundary(next) { end } else { next };
            sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(pos)..ByteIndex(next),
                format: TextFormat { font_id: font.clone(), color, background: bg, ..Default::default() },
            });
            pos = next;
        }
    }
    LayoutJob {
        text: text.to_owned(),
        sections,
        wrap: TextWrapping { max_width: f32::INFINITY, ..Default::default() },
        break_on_newline: false,
        ..Default::default()
    }
}

fn file_label(f: &FileDiff) -> String {
    match (f.status, f.old_path.as_deref(), f.new_path.as_deref()) {
        (FileStatus::Renamed | FileStatus::Copied, Some(old), Some(new)) if old != new => format!("{old} → {new}"),
        _ => f.path().to_owned(),
    }
}

/// Colours of the view, derived once per frame.
struct Palette {
    added: Color32,
    removed: Color32,
    added_gutter: Color32,
    removed_gutter: Color32,
    added_emph: Color32,
    removed_emph: Color32,
    hunk: Color32,
}

impl Palette {
    fn new(t: &Tokens) -> Palette {
        let (ok, bad) = (t.status.success, t.status.danger);
        Palette {
            added: ok.gamma_multiply(0.12),
            removed: bad.gamma_multiply(0.12),
            added_gutter: ok.gamma_multiply(0.2),
            removed_gutter: bad.gamma_multiply(0.2),
            added_emph: ok.gamma_multiply(0.35),
            removed_emph: bad.gamma_multiply(0.35),
            hunk: t.status.info.gamma_multiply(0.1),
        }
    }

    fn line(&self, kind: LineKind) -> (Color32, Color32, Color32) {
        match kind {
            LineKind::Added => (self.added, self.added_gutter, self.added_emph),
            LineKind::Removed => (self.removed, self.removed_gutter, self.removed_emph),
            LineKind::Context => (Color32::TRANSPARENT, Color32::TRANSPARENT, Color32::TRANSPARENT),
        }
    }
}

fn show(view: DiffView<'_>, ui: &mut Ui) -> DiffResponse {
    let DiffView { id, state, files, revision, mode, theme, labels, highlighter, max_height } = view;
    let t = tokens::current(ui.ctx());
    let base = ui.make_persistent_id(id);
    let installed = crate::labels::current(ui.ctx());
    let labels = labels.unwrap_or(&installed.diff);
    let own_theme;
    let theme = match theme {
        Some(th) => th,
        None => {
            own_theme = SyntaxTheme::from_tokens(&t);
            &own_theme
        }
    };
    if state.revision != Some(revision) {
        state.revision = Some(revision);
        state.expanded.clear();
        state.collapsed.clear();
        state.selection.clear();
        state.version = state.version.wrapping_add(1);
    }

    // the row model, rebuilt when the files, the mode or the expansion change
    let model_id = base.with("model");
    let key = hash_of(&(revision, mode, state.version, files.as_ptr() as usize, files.len()));
    let mut model =
        ui.data_mut(|d| d.remove_temp::<Model>(model_id)).filter(|m| m.key == key).unwrap_or_else(|| Model::build(key, files, state, mode));
    let rows = model.rows.len();
    state.selection.clamp(rows);

    let font = t.mono();
    let (line_h, char_w) = ui.fonts_mut(|f| (f.row_height(&font), f.glyph_width(&font, '0')));
    let (line_h, char_w) = (line_h.max(1.0) + 2.0, char_w.max(1.0));
    let pad = t.space.sm;
    let num_w = model.digits as f32 * char_w + 2.0 * pad;
    let sign_w = 2.0 * char_w;
    let style_rev = hash_of(&(theme.revision(), font.size.to_bits(), t.dark));
    let pal = Palette::new(&t);

    // keyboard: next / previous hunk
    let top_id = base.with("top");
    let goto_id = base.with("goto");
    let (last_top, page): (usize, usize) = ui.data(|d| d.get_temp(top_id)).unwrap_or((0, 0));
    let mut scroll_to: Option<usize> = ui.data_mut(|d| d.remove_temp::<usize>(goto_id)).filter(|r| *r < rows);
    let mut moved_hunk = None;
    if ui.memory(|m| m.has_focus(base)) {
        let (next, prev) = ui.input(|i| {
            let plain = !i.modifiers.command && !i.modifiers.ctrl && !i.modifiers.alt;
            (
                (plain && !i.modifiers.shift && i.key_pressed(Key::N)) || (!i.modifiers.shift && i.key_pressed(Key::F7)),
                (plain && !i.modifiers.shift && i.key_pressed(Key::P)) || (i.modifiers.shift && i.key_pressed(Key::F7)),
            )
        });
        let cursor = state.selection.cursor().unwrap_or(last_top);
        let target = if next {
            model.hunk_rows.get(model.hunk_rows.partition_point(|&r| r <= cursor)).copied()
        } else if prev {
            model.hunk_rows.partition_point(|&r| r < cursor).checked_sub(1).and_then(|k| model.hunk_rows.get(k)).copied()
        } else {
            None
        };
        if let Some(r) = target {
            state.selection.select_only(r);
            // a hunk below the view comes in near the top, not at the bottom edge
            scroll_to = Some(if r > last_top { r.saturating_add(page.saturating_sub(3)).min(rows.saturating_sub(1)) } else { r });
            moved_hunk = model.rows.get(r).map(|row| (row.file as usize, row.item as usize));
        }
    }

    let mut list = VirtualList::new(id, rows, RowHeights::Uniform(line_h)).scroll_to(scroll_to).selection(&mut state.selection);
    if let Some(h) = max_height {
        list = list.max_height(h);
    }
    let mut header_rect = None;
    let mut record_header = |_: &mut Ui, r: Rect| header_rect = Some(r);
    let header = Header { height: line_h + 2.0, paint: &mut record_header };
    let kind = virtual_list::Kind { role: Role::List, rows: false, node: NodeRole::Document, item: NodeRole::Paragraph };
    let mut top_row: Option<usize> = None;
    let mut shown_rows = 0usize;
    let mut scratch = Vec::new();
    let ctx = ui.ctx().clone();

    let out = virtual_list::show_rows(ui, base, &mut list, kind, Some(header), |ui, row| {
        let i = row.index;
        let Some(r) = model.rows.get(i).copied() else { return };
        if ui.clip_rect().height() > line_h * 0.5 {
            top_row.get_or_insert(i);
            shown_rows += 1;
        }
        let rect = row.rect;
        let painter = ui.painter().clone();
        let fi = r.file as usize;
        let Some(f) = files.get(fi) else { return };
        let row_node = virtual_list::row_id(base, i);
        let node = |role: NodeRole, label: &str| {
            ctx.accesskit_node_builder(row_node, |n| {
                n.set_role(role);
                n.set_label(label);
            });
        };
        let galley_at =
            |key: (&str, usize, u8), rev: u64, job: &mut dyn FnMut() -> LayoutJob| -> Arc<Galley> { cache::galley(ui, (base, key), rev, job) };
        let text_y = rect.top() + 1.0;
        match r.kind {
            RowKind::File => {
                if !row.selected {
                    painter.rect_filled(rect, 0, t.surface.raised);
                }
                painter.hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, t.border));
                let g = file_galley(ui, base, f, fi, model.stats.get(fi).copied().unwrap_or_default(), state.collapsed.contains(&fi), &t, style_rev);
                painter.galley(pos2(rect.left() + pad, text_y), g, t.text.primary);
                let label = file_label(f);
                node(NodeRole::Heading, &label);
                automation::register_fmt(ui, rect, format_args!("{id}.file.{fi}"), &label, Role::Button, true, Details::default());
            }
            RowKind::Note => {
                painter.rect_filled(rect, 0, t.surface.sunken);
                let g =
                    galley_at(("note", i, 0), style_rev, &mut || LayoutJob::simple_singleline(labels.binary.clone(), font.clone(), t.text.secondary));
                painter.galley(pos2(rect.left() + pad + num_w, text_y), g, t.text.secondary);
                node(NodeRole::Paragraph, &labels.binary);
            }
            RowKind::Hunk => {
                let hi = r.item as usize;
                let header = f.hunks.get(hi).map(|h| h.header()).unwrap_or_default();
                if !row.selected {
                    painter.rect_filled(rect, 0, pal.hunk);
                }
                let g = galley_at(("hunk", i, 0), hash_of(&(model.key, style_rev)), &mut || {
                    LayoutJob::simple_singleline(display(&header).to_owned(), font.clone(), t.text.secondary)
                });
                let x = if mode == DiffMode::Unified { 2.0 * num_w } else { num_w };
                painter.galley(pos2(rect.left() + x + pad, text_y), g, t.text.secondary);
                node(NodeRole::Heading, &header);
                automation::register_fmt(ui, rect, format_args!("{id}.file.{fi}.hunk.{hi}"), &header, Role::Header, true, Details::default());
            }
            RowKind::Gap => {
                let gi = r.item as usize;
                let expandable = model.gaps.get(fi).and_then(|g| g.get(gi)).is_some_and(|g| g.expandable);
                if !row.selected {
                    painter.rect_filled(rect, 0, t.surface.sunken);
                }
                let hidden = labels.hidden.replace("{n}", &r.a.to_string());
                let label =
                    if expandable { format!("{hidden} · {}", labels.expand.replace("{n}", &r.a.min(EXPAND_STEP).to_string())) } else { hidden };
                let color = if expandable { t.accent } else { t.text.disabled };
                let g = galley_at(("gap", i, 0), hash_of(&(model.key, style_rev)), &mut || {
                    LayoutJob::simple_singleline(format!("⋯  {label}"), font.clone(), color)
                });
                painter.galley(pos2(rect.left() + pad, text_y), g, color);
                ctx.accesskit_node_builder(row_node, |n| {
                    n.set_role(if expandable { NodeRole::Button } else { NodeRole::Paragraph });
                    n.set_label(label.as_str());
                });
                automation::register_fmt(ui, rect, format_args!("{id}.file.{fi}.gap.{gi}"), &label, Role::Button, expandable, Details::default());
            }
            RowKind::Ctx | RowKind::Line => {
                let st = model.state(files, mode, highlighter, i, &mut scratch);
                let lang = model.langs.get(fi).copied().unwrap_or("");
                // the cells: (line, old number, new number, kind, lexer state, side)
                let cells: Vec<(Option<&ashlar_diff::Line>, Option<u32>, Option<u32>, LineKind, State, u8)> = match (r.kind, mode) {
                    (RowKind::Ctx, DiffMode::Unified) => vec![(None, Some(r.b), Some(r.a), LineKind::Context, st.1, 0)],
                    (RowKind::Ctx, DiffMode::Split) => {
                        vec![(None, Some(r.b), None, LineKind::Context, st.0, 0), (None, None, Some(r.a), LineKind::Context, st.1, 1)]
                    }
                    (_, DiffMode::Unified) => hunk_line(files, &r, r.a)
                        .map(|l| {
                            let s = if l.kind == LineKind::Removed { st.0 } else { st.1 };
                            vec![(Some(l), l.old_no, l.new_no, l.kind, s, 0)]
                        })
                        .unwrap_or_default(),
                    (_, DiffMode::Split) => {
                        let left = hunk_line(files, &r, r.a);
                        let right = hunk_line(files, &r, r.b);
                        vec![
                            (left, left.and_then(|l| l.old_no), None, left.map_or(LineKind::Context, |l| l.kind), st.0, 0),
                            (right, None, right.and_then(|l| l.new_no), right.map_or(LineKind::Context, |l| l.kind), st.1, 1),
                        ]
                    }
                };
                let half = rect.width() / 2.0;
                let mut spoken = String::new();
                for (line, old_no, new_no, lk, lex, side) in &cells {
                    let x0 = rect.left() + if *side == 1 { half } else { 0.0 };
                    let cell = Rect::from_min_max(
                        pos2(x0, rect.top()),
                        pos2(if mode == DiffMode::Split && *side == 0 { x0 + half } else { rect.right() }, rect.bottom()),
                    );
                    let empty = mode == DiffMode::Split && r.kind == RowKind::Line && line.is_none();
                    let (bg, gutter_bg, emph_bg) = pal.line(*lk);
                    let gutter_w = if mode == DiffMode::Unified { 2.0 * num_w } else { num_w };
                    if empty {
                        painter.rect_filled(cell, 0, t.surface.sunken);
                    } else if !row.selected {
                        painter.rect_filled(cell, 0, bg);
                        painter.rect_filled(Rect::from_min_size(cell.min, vec2(gutter_w, cell.height())), 0, gutter_bg);
                    }
                    if mode == DiffMode::Split && *side == 1 {
                        painter.vline(cell.left(), cell.y_range(), egui::Stroke::new(1.0, t.border));
                    }
                    if empty {
                        continue;
                    }
                    // numbers
                    let mut nx = cell.left();
                    let numbers: &[Option<u32>] = if mode == DiffMode::Unified {
                        &[*old_no, *new_no]
                    } else if *side == 0 {
                        &[*old_no]
                    } else {
                        &[*new_no]
                    };
                    for n in numbers {
                        if let Some(n) = n {
                            let g = cache::galley(ui, (base, "num", *n), hash_of(&(font.size.to_bits(), t.dark)), || {
                                LayoutJob::simple_singleline(n.to_string(), font.clone(), t.text.disabled)
                            });
                            painter.galley(pos2(nx + num_w - pad - g.size().x, text_y), g, t.text.disabled);
                        }
                        nx += num_w;
                    }
                    let sign = match lk {
                        LineKind::Added => "+",
                        LineKind::Removed => "-",
                        LineKind::Context => "",
                    };
                    if !sign.is_empty() {
                        let color = if *lk == LineKind::Added { t.status.success } else { t.status.danger };
                        let g = cache::galley(ui, (base, "sign", sign), hash_of(&(font.size.to_bits(), t.dark)), || {
                            LayoutJob::simple_singleline(sign.to_owned(), font.clone(), color)
                        });
                        painter.galley(pos2(nx + pad * 0.5, text_y), g, color);
                    }
                    // text
                    let raw = match line {
                        Some(l) => l.text.as_str(),
                        None => f.source_line(r.a).unwrap_or(""),
                    };
                    let shown = display(raw);
                    let emphasis: &[Range<usize>] = line.map_or(&[], |l| l.emphasis.as_slice());
                    let rev = hash_of(&(model.key, style_rev, lex.to_bits()));
                    let g = cache::galley(ui, (base, "text", i, *side), rev, || {
                        scratch.clear();
                        let _ = highlighter.highlight_line(lang, shown, *lex, &mut scratch);
                        line_job(shown, &scratch, emphasis, &font, theme, emph_bg)
                    });
                    painter.with_clip_rect(cell.intersect(ui.clip_rect())).galley(pos2(nx + sign_w, text_y), g, theme.color(Kind::Plain));
                    if !spoken.is_empty() {
                        spoken.push_str(" | ");
                    }
                    let num = new_no.or(*old_no).map(|n| n.to_string()).unwrap_or_default();
                    match lk {
                        LineKind::Added => spoken.push_str(&format!("{} {num}: ", labels.added)),
                        LineKind::Removed => spoken.push_str(&format!("{} {num}: ", labels.removed)),
                        LineKind::Context => spoken.push_str(&format!("{num}: ")),
                    }
                    spoken.push_str(clip_label(shown));
                }
                node(NodeRole::Paragraph, &spoken);
                let details = Details { value: None, selected: Some(row.selected) };
                if r.kind == RowKind::Ctx {
                    let n = r.a;
                    automation::register_fmt(ui, rect, format_args!("{id}.file.{fi}.gap.{}.line.{n}", r.item), &spoken, Role::Row, true, details);
                } else {
                    let k = if r.a != NONE { r.a } else { r.b };
                    automation::register_fmt(ui, rect, format_args!("{id}.file.{fi}.hunk.{}.line.{k}", r.item), &spoken, Role::Row, true, details);
                }
            }
        }
    });
    let top = top_row.unwrap_or(last_top);
    let top_file = model.rows.get(top).map(|r| r.file as usize);

    // the sticky header: the file of the top row
    if let (Some(rect), Some(fi)) = (header_rect, top_file)
        && let Some(f) = files.get(fi)
    {
        let resp = ui.interact(rect, base.with("sticky"), Sense::click());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0, t.surface.raised);
        painter.hline(rect.x_range(), rect.bottom() - 0.5, egui::Stroke::new(1.0, t.border_strong));
        let g = file_galley(ui, base, f, fi, model.stats.get(fi).copied().unwrap_or_default(), state.is_file_collapsed(fi), &t, style_rev);
        painter.galley(pos2(rect.left() + pad, rect.top() + 2.0), g, t.text.primary);
        let label = file_label(f);
        ui.ctx().accesskit_node_builder(resp.id, |n| {
            n.set_role(NodeRole::Heading);
            n.set_label(label.as_str());
        });
        automation::register_with(ui, &resp, &format!("{id}.header"), &label, Role::Header, Details::default());
        if resp.clicked()
            && let Some(file_row) = model.rows.iter().position(|r| r.file as usize == fi && r.kind == RowKind::File)
        {
            ui.data_mut(|d| d.insert_temp(goto_id, file_row));
            crate::wake::wake(ui.ctx(), crate::wake::Reason::INPUT);
        }
    }

    // clicks and Enter on file headers and collapsed ranges
    let alt = ui.input(|i| i.modifiers.alt);
    let activated = out.activated.filter(|_| out.double_clicked.is_none());
    let mut expanded = None;
    let mut toggled_file = None;
    if let Some(i) = out.clicked.or(activated)
        && let Some(r) = model.rows.get(i).copied()
    {
        let fi = r.file as usize;
        match r.kind {
            RowKind::File => {
                let now = !state.is_file_collapsed(fi);
                state.set_file_collapsed(fi, now);
                toggled_file = Some(fi);
            }
            RowKind::Gap if model.gaps.get(fi).and_then(|g| g.get(r.item as usize)).is_some_and(|g| g.expandable) => {
                state.expand(fi, r.item as usize, if alt { r.a } else { r.a.min(EXPAND_STEP) });
                expanded = Some((fi, r.item as usize));
            }
            _ => {}
        }
        if toggled_file.is_some() || expanded.is_some() {
            state.selection.select_only(i);
            crate::wake::wake(ui.ctx(), crate::wake::Reason::INPUT);
        }
    }

    // selected lines
    let mut selected_lines = Vec::new();
    for i in state.selection.iter() {
        if selected_lines.len() >= MAX_SELECTED_LINES {
            break;
        }
        let Some(r) = model.rows.get(i) else { break };
        let file = r.file as usize;
        match r.kind {
            RowKind::Ctx => selected_lines.push(SelectedLine { file, kind: LineKind::Context, old_no: Some(r.b), new_no: Some(r.a) }),
            RowKind::Line => {
                let both = if mode == DiffMode::Split && r.a != r.b { [r.a, r.b] } else { [r.a, NONE] };
                for idx in both {
                    if let Some(l) = hunk_line(files, r, idx) {
                        selected_lines.push(SelectedLine { file, kind: l.kind, old_no: l.old_no, new_no: l.new_no });
                    }
                }
            }
            _ => {}
        }
    }

    ui.ctx().accesskit_node_builder(out.response.id, |n| {
        n.set_role(NodeRole::Document);
        n.set_read_only();
    });
    ui.data_mut(|d| {
        d.insert_temp(top_id, (top, shown_rows));
        d.insert_temp(model_id, model);
    });
    DiffResponse { response: out.response, selected_lines, expanded, toggled_file, hunk: moved_hunk, top_file, visible: out.visible }
}

/// The file header text: fold arrow, path, `+added −removed`.
fn file_galley(
    ui: &Ui,
    base: egui::Id,
    f: &FileDiff,
    fi: usize,
    (added, removed): (usize, usize),
    collapsed: bool,
    t: &Tokens,
    style_rev: u64,
) -> Arc<Galley> {
    let rev = hash_of(&(style_rev, collapsed, added, removed, f.path(), f.binary));
    cache::galley(ui, (base, "file", fi), rev, || {
        let font = egui::TextStyle::Body.resolve(ui.style());
        let mut job = LayoutJob::default();
        let fmt = |color: Color32| TextFormat { font_id: font.clone(), color, ..Default::default() };
        job.append(if collapsed { "▸ " } else { "▾ " }, 0.0, fmt(t.text.secondary));
        job.append(&file_label(f), 0.0, fmt(t.text.primary));
        if added > 0 {
            job.append(&format!("+{added}"), 12.0, fmt(t.status.success));
        }
        if removed > 0 {
            job.append(&format!("−{removed}"), 8.0, fmt(t.status.danger));
        }
        job.wrap = TextWrapping { max_width: f32::INFINITY, ..Default::default() };
        job
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_pairs_removed_and_added_runs() {
        let d = FileDiff::from_texts("a\nb\nc\nd\n", "a\nB\nC\nX\nd\n");
        let files = [d];
        let m = Model::build(1, &files, &DiffViewState::new(), DiffMode::Split);
        let lines: Vec<(u32, u32)> = m.rows.iter().filter(|r| r.kind == RowKind::Line).map(|r| (r.a, r.b)).collect();
        // a | B/b, C/c, X/- | d
        assert_eq!(lines, vec![(0, 0), (1, 3), (2, 4), (NONE, 5), (6, 6)]);
        let m = Model::build(1, &files, &DiffViewState::new(), DiffMode::Unified);
        assert_eq!(m.rows.iter().filter(|r| r.kind == RowKind::Line).count(), 7);
        assert_eq!(m.hunk_rows, vec![1]);
    }

    #[test]
    fn gaps_expand_and_files_fold() {
        let old: String = (1..=50).map(|i| format!("{i}\n")).collect();
        let new = old.replacen("25\n", "x\n", 1);
        let files = [FileDiff::from_texts(&old, &new)];
        let mut st = DiffViewState::new();
        let m = Model::build(1, &files, &st, DiffMode::Unified);
        let gaps: Vec<u32> = m.rows.iter().filter(|r| r.kind == RowKind::Gap).map(|r| r.a).collect();
        assert_eq!(gaps, vec![21, 22]);
        st.expand(0, 0, 5);
        let m2 = Model::build(2, &files, &st, DiffMode::Unified);
        assert_eq!(m2.rows.len(), m.rows.len() + 5);
        st.expand(0, 0, 100);
        let m3 = Model::build(3, &files, &st, DiffMode::Unified);
        assert_eq!(m3.rows.len(), m.rows.len() + 21 - 1);
        st.set_file_collapsed(0, true);
        assert_eq!(Model::build(4, &files, &st, DiffMode::Unified).rows.len(), 1);
    }

    #[test]
    fn emphasis_sections_split_spans() {
        let theme = SyntaxTheme::from_tokens(&Tokens::default());
        let font = FontId::monospace(12.0);
        let text = "let x = 1;";
        let spans = ashlar_syntax::highlight(Some("rust"), text);
        let job = line_job(text, &spans, &[4..5, 8..9], &font, &theme, Color32::RED);
        let red: Vec<Range<usize>> =
            job.sections.iter().filter(|s| s.format.background == Color32::RED).map(|s| s.byte_range.start.0..s.byte_range.end.0).collect();
        assert_eq!(red, vec![4..5, 8..9]);
        assert_eq!(job.sections.last().map(|s| s.byte_range.end.0), Some(text.len()));
        // hostile ranges past the end
        let job = line_job(text, &spans, &[100..200, 3..1000], &font, &theme, Color32::RED);
        assert!(job.sections.iter().all(|s| s.byte_range.end.0 <= text.len()));
    }
}
