//! Drawing, with what is kept between frames: the parsed blocks (by content or by the caller's
//! revision; streaming text is parsed again only from its last open block), each block's galley
//! (by block, width, style and find query) and each block's size, so blocks outside the clip rect
//! only reserve their space.

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Frame, Id, Margin, Rect, Sense, Stroke, Ui, UiBuilder};

use super::find::{find_ranges, mark};
use super::parse::{Doc, parse_doc};
use super::{MAX_BYTES, MAX_TABLE_ROWS, MarkdownLabels, MarkdownResponse, MarkdownStyle, Marker, MdBlock, Span, safe_link};
use crate::automation::{self, Details, Role};
use crate::cache::{self, hash_of};
use crate::code_view::{SyntaxTheme, highlight_job};
use crate::tokens;

/// What [`super::Markdown::show`] passes down.
pub(super) struct Opts<'a> {
    pub id: &'a str,
    pub style: &'a MarkdownStyle,
    pub labels: &'a MarkdownLabels,
    pub query: &'a str,
    pub on_task: Option<&'a mut dyn FnMut(usize, bool)>,
    pub streaming: bool,
    pub revision: Option<u64>,
}

/// What one view keeps between frames.
#[derive(Default)]
struct State {
    /// Text views only: the parse of the text last shown.
    parsed: Option<Parsed>,
    /// Per block, side by side so a block off screen is skipped by reading one entry.
    blocks: Vec<Info>,
    /// What the sizes were measured with (width, style, scale).
    layout_key: u64,
    /// The query `finds` counts (0: none).
    find_key: u64,
    /// Column widths of drawn tables: (block, what they were measured for, widths).
    columns: Vec<(usize, u64, Vec<f32>)>,
}

/// What is kept per block.
#[derive(Clone, Copy)]
struct Info {
    /// Content hash.
    hash: u64,
    /// Drawn size (NaN: not measured at the current width).
    size: egui::Vec2,
    /// Links, task checkboxes, code blocks and tables in it (for numbering past skipped blocks).
    links: u32,
    tasks: u32,
    code: u32,
    table: u32,
    /// Matches of the find query.
    finds: u32,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

struct Parsed {
    text_hash: u64,
    text_len: usize,
    /// The caller's revision of the text, when it keeps one.
    revision: Option<u64>,
    /// Parsed as streaming text (the last block repaired).
    repair: bool,
    doc: Doc,
}

impl Parsed {
    fn new(md: &str, repair: bool) -> Self {
        Parsed { text_hash: hash_of(&md), text_len: md.len(), revision: None, repair, doc: parse_doc(md, repair) }
    }

    /// Bring the parse up to `md`. Returns the number of leading blocks that did not change
    /// (`None`: nothing changed).
    fn update(&mut self, md: &str) -> Option<usize> {
        let hash = hash_of(&md);
        if hash == self.text_hash && md.len() == self.text_len {
            return None;
        }
        let appended =
            md.len() > self.text_len && md.len() <= MAX_BYTES && md.get(..self.text_len).is_some_and(|old| hash_of(&old) == self.text_hash);
        let tail = md.get(self.doc.resume..).filter(|_| appended);
        let kept = match tail {
            Some(tail) => {
                // text was appended: parse again from the line after the last final block
                let more = parse_doc(tail, self.repair);
                let kept = self.doc.stable;
                self.doc.blocks.truncate(kept);
                self.doc.blocks.extend(more.blocks);
                if more.stable > 0 {
                    self.doc.resume += more.resume;
                }
                self.doc.stable = kept + more.stable;
                kept
            }
            None => {
                self.doc = parse_doc(md, self.repair);
                0
            }
        };
        self.text_hash = hash;
        self.text_len = md.len();
        Some(kept)
    }
}

impl State {
    /// Per-block hashes, link, task and match counts for `blocks`, from block `from` on; a block
    /// whose content changed is measured again.
    fn sync(&mut self, blocks: &[MdBlock], from: usize, query: &str) {
        let from = from.min(self.blocks.len());
        for (i, b) in blocks.iter().enumerate().skip(from) {
            let hash = hash_of(b);
            if self.blocks.get(i).is_some_and(|x| x.hash == hash) {
                continue;
            }
            let info = Info {
                hash,
                size: egui::Vec2::NAN,
                links: count(links_in(b)),
                tasks: u32::from(matches!(b, MdBlock::Item { marker: Marker::Task(_), .. })),
                code: u32::from(matches!(b, MdBlock::Code { .. })),
                table: u32::from(matches!(b, MdBlock::Table { .. })),
                finds: count(matches_in(b, query)),
            };
            match self.blocks.get_mut(i) {
                Some(old) => *old = info,
                None => self.blocks.push(info),
            }
        }
        self.blocks.truncate(blocks.len());
        self.columns.retain(|(b, _, _)| *b < blocks.len());
    }

    /// Count the matches of a new find query in every block.
    fn refind(&mut self, blocks: &[MdBlock], query: &str) {
        let key = if query.is_empty() { 0 } else { hash_of(&query) | 1 };
        if key != self.find_key {
            self.find_key = key;
            for (x, b) in self.blocks.iter_mut().zip(blocks) {
                x.finds = count(matches_in(b, query));
            }
        }
    }
}

/// The links of a run list: consecutive runs with the same (non-empty) URL make one link.
/// Yields (char range, byte range, url).
fn link_runs(spans: &[Span]) -> impl Iterator<Item = (std::ops::Range<usize>, std::ops::Range<usize>, &str)> {
    let mut chars = 0usize;
    let mut bytes = 0usize;
    let mut it = spans.iter().peekable();
    std::iter::from_fn(move || {
        loop {
            let s = it.next()?;
            let (c0, b0) = (chars, bytes);
            chars += s.text.chars().count();
            bytes += s.text.len();
            let Some(url) = s.link.as_deref().filter(|u| !u.is_empty()) else { continue };
            while let Some(n) = it.next_if(|n| n.link.as_deref() == Some(url)) {
                chars += n.text.chars().count();
                bytes += n.text.len();
            }
            return Some((c0..chars, b0..bytes, url));
        }
    })
}

/// How many links [`link_runs`] yields, without counting characters.
fn link_count(spans: &[Span]) -> usize {
    let mut prev = None;
    let mut n = 0;
    for s in spans {
        let u = s.link.as_deref().filter(|u| !u.is_empty());
        if u.is_some() && u != prev {
            n += 1;
        }
        prev = u;
    }
    n
}

fn spans_of(b: &MdBlock) -> Option<&[Span]> {
    match b {
        MdBlock::Heading { spans, .. } | MdBlock::Paragraph(spans) | MdBlock::Item { spans, .. } | MdBlock::Quote(spans) => Some(spans),
        MdBlock::Code { .. } | MdBlock::Rule | MdBlock::Table { .. } => None,
    }
}

fn links_in(b: &MdBlock) -> usize {
    spans_of(b).map_or(0, link_count)
}

/// Matches of `query` in a block's drawn text (a table: in each cell).
fn matches_in(b: &MdBlock, query: &str) -> usize {
    if query.is_empty() {
        return 0;
    }
    let in_spans = |spans: &[Span]| match spans {
        [one] => find_ranges(&one.text, query).count(),
        _ => find_ranges(&super::plain(spans), query).count(),
    };
    match b {
        MdBlock::Code { text, .. } => find_ranges(text, query).count(),
        MdBlock::Table { header, rows } => header.iter().chain(rows.iter().take(MAX_TABLE_ROWS).flatten()).map(|c| in_spans(c)).sum(),
        MdBlock::Rule => 0,
        _ => spans_of(b).map_or(0, in_spans),
    }
}

// ---------------------------------------------------------------------------------------------
// The per-context store of states
// ---------------------------------------------------------------------------------------------

/// States unused for this many passes are dropped (they are rebuilt when shown again).
const KEEP_PASSES: u64 = 600;

#[derive(Default)]
struct Store {
    states: HashMap<Id, (u64, State)>,
    swept: u64,
}

#[derive(Clone, Default)]
struct Shared(Arc<egui::mutex::Mutex<Store>>);

fn store_id() -> Id {
    static ID: std::sync::OnceLock<Id> = std::sync::OnceLock::new();
    *ID.get_or_init(|| Id::new("ashlar_ui::markdown::states"))
}

fn shared(ctx: &egui::Context) -> Shared {
    let id = store_id();
    match ctx.data(|d| d.get_temp::<Shared>(id)) {
        Some(s) => s,
        None => ctx.data_mut(|d| d.get_temp_mut_or_default::<Shared>(id).clone()),
    }
}

/// Take `id`'s state out of the store (put it back with [`put`]); drawing needs no lock held.
fn take(ctx: &egui::Context, id: Id) -> State {
    let pass = ctx.cumulative_pass_nr();
    let shared = shared(ctx);
    let mut store = shared.0.lock();
    if pass >= store.swept.saturating_add(64) {
        store.swept = pass;
        store.states.retain(|_, (used, _)| used.saturating_add(KEEP_PASSES) >= pass);
    }
    store.states.remove(&id).map(|(_, s)| s).unwrap_or_default()
}

fn put(ctx: &egui::Context, id: Id, state: State) {
    let pass = ctx.cumulative_pass_nr();
    shared(ctx).0.lock().states.insert(id, (pass, state));
}

fn state_id(ui: &Ui, id: &str) -> Id {
    ui.make_persistent_id(("ashlar_ui::markdown", id))
}

/// Text: parse (or reuse the parse of) `md`, then draw. With a revision equal to the last one the
/// text is not looked at.
pub(super) fn show(ui: &mut Ui, md: &str, opts: Opts<'_>) -> MarkdownResponse {
    let sid = state_id(ui, opts.id);
    let mut state = take(ui.ctx(), sid);
    let mut parsed = match state.parsed.take() {
        Some(mut p) if p.repair == opts.streaming => {
            let same = opts.revision.is_some() && p.revision == opts.revision && p.text_len == md.len();
            if !same {
                if let Some(kept) = p.update(md) {
                    state.sync(&p.doc.blocks, kept, opts.query);
                }
                p.revision = opts.revision;
            }
            p
        }
        _ => {
            let mut p = Parsed::new(md, opts.streaming);
            p.revision = opts.revision;
            state.blocks.clear();
            state.sync(&p.doc.blocks, 0, opts.query);
            p
        }
    };
    let resp = draw(ui, sid, &parsed.doc.blocks, &mut state, opts);
    parsed.doc.blocks.shrink_to_fit();
    state.parsed = Some(parsed);
    put(ui.ctx(), sid, state);
    resp
}

/// Blocks parsed by the caller (hashed every frame to find changes).
pub(super) fn show_blocks(ui: &mut Ui, blocks: &[MdBlock], opts: Opts<'_>) -> MarkdownResponse {
    let sid = state_id(ui, opts.id);
    let mut state = take(ui.ctx(), sid);
    state.parsed = None;
    state.sync(blocks, 0, opts.query);
    let resp = draw(ui, sid, blocks, &mut state, opts);
    put(ui.ctx(), sid, state);
    resp
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

fn style_hash(s: &MarkdownStyle) -> u64 {
    let font = |f: &FontId| (f.size.to_bits(), f.family.clone());
    hash_of(&(
        [s.text, s.strong, s.weak, s.link, s.code_text, s.code_bg, s.code_border, s.rule, s.find_bg],
        font(&s.body),
        font(&s.mono),
        &s.bold_family,
        s.heading_sizes.map(f32::to_bits),
        [s.corner.nw, s.corner.ne, s.corner.sw, s.corner.se],
    ))
}

/// Blocks off screen, not drawn: their space is reserved in one go.
#[derive(Default)]
struct Skipped {
    height: f32,
    width: f32,
    n: usize,
}

impl Skipped {
    fn add(&mut self, size: egui::Vec2, gap: f32) {
        self.height += if self.n > 0 { gap + size.y } else { size.y };
        self.width = self.width.max(size.x);
        self.n += 1;
    }
    /// Where the next block starts, relative to the cursor.
    fn offset(&self, gap: f32) -> f32 {
        if self.n > 0 { self.height + gap } else { 0.0 }
    }
    fn flush(&mut self, ui: &mut Ui) {
        if self.n > 0 {
            // one item with the same spacing after it as the blocks it stands for
            let _ = ui.allocate_space(egui::vec2(self.width, self.height));
            *self = Skipped::default();
        }
    }
}

/// What a block's galley depends on besides its content.
#[derive(Clone, Copy)]
struct GalleyKey {
    key: u64,
    revision: u64,
    /// The block has find matches (its galleys carry the highlight).
    found: bool,
}

/// One draw call's context: options, counters and what the person did.
struct Cx<'a> {
    id: &'a str,
    style: &'a MarkdownStyle,
    labels: &'a MarkdownLabels,
    query: &'a str,
    on_task: Option<&'a mut dyn FnMut(usize, bool)>,
    gap: f32,
    /// Whether AccessKit is on (asked once, at the first link).
    accesskit: Option<bool>,
    link_n: usize,
    task_n: usize,
    code_n: usize,
    table_n: usize,
    resp: MarkdownResponse,
}

fn draw(ui: &mut Ui, sid: Id, blocks: &[MdBlock], state: &mut State, opts: Opts<'_>) -> MarkdownResponse {
    let style = opts.style;
    let gap = style.body.size * 0.45;
    let style_rev = style_hash(style);
    state.refind(blocks, opts.query);
    let find_key = state.find_key;
    let mut cx = Cx {
        id: opts.id,
        style,
        labels: opts.labels,
        query: opts.query,
        on_task: opts.on_task,
        gap,
        accesskit: None,
        link_n: 0,
        task_n: 0,
        code_n: 0,
        table_n: 0,
        resp: MarkdownResponse { matches: state.blocks.iter().map(|x| x.finds as usize).sum(), ..Default::default() },
    };
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = gap;
        let sp = ui.spacing();
        let layout_key = hash_of(&(
            ui.available_width().to_bits(),
            style_rev,
            ui.ctx().pixels_per_point().to_bits(),
            sp.interact_size.y.to_bits(),
            sp.button_padding.y.to_bits(),
            sp.item_spacing.x.to_bits(),
            cx.on_task.is_some(),
        ));
        if layout_key != state.layout_key {
            state.layout_key = layout_key;
            state.blocks.iter_mut().for_each(|x| x.size = egui::Vec2::NAN);
        }
        let clip = ui.clip_rect();
        let mut skipped = Skipped::default();
        for (i, b) in blocks.iter().enumerate() {
            let Some(info) = state.blocks.get(i).copied() else { break };
            let (links, tasks) = (info.links as usize, info.tasks as usize);
            if info.size.y.is_finite() {
                let top = ui.cursor().top() + skipped.offset(gap);
                if top > clip.bottom() || top + info.size.y < clip.top() {
                    skipped.add(info.size, gap);
                    cx.link_n += links;
                    cx.task_n += tasks;
                    cx.code_n += info.code as usize;
                    cx.table_n += info.table as usize;
                    continue;
                }
            }
            let found = info.finds > 0;
            skipped.flush(ui);
            let (first_link, first_task) = (cx.link_n, cx.task_n);
            let revision = hash_of(&(info.hash, style_rev, if found { find_key } else { 0 }));
            let key = GalleyKey { key: sid.with(i).value(), revision, found };
            let before = ui.cursor().top();
            let r = ui.scope_builder(UiBuilder::new().id(sid.with(("block", i))), |ui| {
                block(ui, &mut cx, b, key, i, &mut state.columns);
            });
            cx.link_n = first_link + links;
            cx.task_n = first_task + tasks;
            cx.code_n += info.code as usize;
            cx.table_n += info.table as usize;
            if let Some(x) = state.blocks.get_mut(i) {
                // how far the cursor moved, less the spacing: egui rounds each block's rect, so
                // this (not the rect's height) is what skipping the block must reproduce
                x.size = egui::vec2(r.response.rect.width(), (ui.cursor().top() - before - gap).max(0.0));
            }
        }
        skipped.flush(ui);
    });
    cx.resp
}

fn block(ui: &mut Ui, cx: &mut Cx<'_>, b: &MdBlock, key: GalleyKey, index: usize, columns: &mut Vec<(usize, u64, Vec<f32>)>) {
    let style = cx.style;
    match b {
        MdBlock::Heading { level, spans } => {
            let size =
                style.heading_sizes.get(usize::from(level.saturating_sub(1))).or(style.heading_sizes.last()).copied().unwrap_or(style.body.size);
            let font = FontId::new(size, style.bold_family.clone().unwrap_or_else(|| style.body.family.clone()));
            ui.add_space(cx.gap * 0.5);
            paragraph(ui, cx, spans, true, key, &font, style.strong);
        }
        MdBlock::Paragraph(spans) => paragraph(ui, cx, spans, false, key, &style.body, style.text),
        MdBlock::Quote(spans) => {
            let r = ui.horizontal_top(|ui| {
                ui.add_space(10.0);
                paragraph(ui, cx, spans, false, key, &style.body, style.weak);
            });
            let rect = r.response.rect;
            ui.painter()
                .line_segment([rect.left_top() + egui::vec2(3.0, 0.0), rect.left_bottom() + egui::vec2(3.0, 0.0)], Stroke::new(2.0, style.rule));
        }
        MdBlock::Item { depth, marker, spans } => {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_space(f32::from(*depth) * style.body.size * 1.2);
                let m = match marker {
                    Marker::Bullet => if *depth == 0 { "•" } else { "◦" }.to_string(),
                    Marker::Number(n) => format!("{n}."),
                    Marker::Task(true) => "☑".into(),
                    Marker::Task(false) => "☐".into(),
                };
                let w = style.body.size * 1.1;
                let size = egui::vec2(w, style.body.size * 1.3);
                match marker {
                    Marker::Task(checked) if cx.on_task.is_some() => task_box(ui, cx, size, &m, *checked, spans),
                    _ => {
                        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                        ui.painter().text(rect.right_top(), egui::Align2::RIGHT_TOP, m, style.body.clone(), style.weak);
                    }
                }
                paragraph(ui, cx, spans, false, key, &style.body, style.text);
            });
        }
        MdBlock::Rule => {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), cx.gap), Sense::hover());
            ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, style.rule));
        }
        MdBlock::Code { lang, text, .. } => {
            let n = cx.code_n;
            if code_block(ui, cx, n, lang, text, key) {
                cx.resp.copied = Some(n);
            }
        }
        MdBlock::Table { header, rows } => table(ui, cx, header, rows, key, index, columns),
    }
}

/// A clickable task checkbox (when the app takes [`super::Markdown::on_task`]).
fn task_box(ui: &mut Ui, cx: &mut Cx<'_>, size: egui::Vec2, glyph: &str, checked: bool, spans: &[Span]) {
    let n = cx.task_n;
    cx.task_n += 1;
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    let color = if r.hovered() || r.has_focus() { cx.style.strong } else { cx.style.weak };
    ui.painter().text(rect.right_top(), egui::Align2::RIGHT_TOP, glyph, cx.style.body.clone(), color);
    if r.has_focus() {
        let glyph_rect = Rect::from_min_size(rect.right_top() - egui::vec2(cx.style.body.size, 0.0), egui::vec2(cx.style.body.size, size.y));
        crate::controls::focus_ring(ui.painter(), glyph_rect, cx.style.corner, &tokens::current(ui.ctx()));
    }
    r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, checked, super::plain(spans)));
    automation::register_fmt(ui, rect, format_args!("{}.task.{n}", cx.id), "", Role::Toggle, true, Details { value: None, selected: Some(checked) });
    if r.clicked() {
        cx.resp.task = Some((n, !checked));
        if let Some(f) = cx.on_task.as_mut() {
            f(n, !checked);
        }
    }
}

/// The galley of a text block, from the context-wide cache: laid out again only when the block,
/// the style, the find query or the width changes. Placed the way `egui::Label` places a job.
fn block_galley(ui: &Ui, key: GalleyKey, width: f32, job: impl FnOnce() -> LayoutJob) -> Arc<egui::Galley> {
    let halign = ui.layout().horizontal_placement();
    let justify = ui.layout().horizontal_justify();
    let revision = hash_of(&(key.revision, width.to_bits(), halign, justify));
    cache::galley_in(ui.ctx(), key.key, revision, || {
        let mut job = job();
        job.wrap.max_width = width;
        job.halign = halign;
        job.justify = justify;
        job
    })
}

fn spans_job(spans: &[Span], bold: bool, style: &MarkdownStyle, font: &FontId, base: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for s in spans {
        job.append(&s.text, 0.0, style.format(s, bold, font, base));
    }
    job
}

/// The rects of a char range of a galley, one per row it covers (relative to the galley).
fn row_rects(galley: &egui::Galley, range: std::ops::Range<usize>, mut each: impl FnMut(usize, Rect)) {
    let mut start = 0usize;
    let mut k = 0usize;
    for row in &galley.rows {
        let len = row.char_count_excluding_newline().0;
        let (a, b) = (range.start.max(start), range.end.min(start + len));
        if a < b {
            let x0 = row.x_offset(egui::text::CharIndex(a - start));
            let x1 = row.x_offset(egui::text::CharIndex(b - start));
            let rect = Rect::from_x_y_ranges(row.pos.x + x0..=row.pos.x + x1, row.min_y()..=row.max_y());
            each(k, rect);
            k += 1;
        }
        start += row.char_count_including_newline().0;
        if start >= range.end {
            break;
        }
    }
}

fn paragraph(ui: &mut Ui, cx: &mut Cx<'_>, spans: &[Span], bold: bool, key: GalleyKey, font: &FontId, base: Color32) {
    let style = cx.style;
    let galley = block_galley(ui, key, ui.available_width(), || {
        let mut job = spans_job(spans, bold, style, font, base);
        if key.found {
            mark(&mut job, cx.query, style.find_bg);
        }
        job
    });
    let count = link_count(spans);
    if count == 0 {
        ui.add(egui::Label::new(galley).selectable(true));
        return;
    }
    // with links: one clickable label; a click opens the link under the pointer on any row it
    // wraps onto. Links take keyboard focus without being widgets (only the focused one is), so
    // a paragraph with many links costs little.
    let (pos, galley, r) = egui::Label::new(galley).selectable(false).sense(Sense::CLICK).layout_in_ui(ui);
    ui.painter().galley(pos, galley.clone(), base);
    let (id, first) = (cx.id, cx.link_n);
    let link_id = |ui: &Ui, n: usize| ui.id().with(("md-link", n));
    let layer = ui.layer_id();
    // in document order (Tab follows it): the links before the focused one now, the focused one
    // as a widget, the links after it right after that widget
    let interested = |ui: &Ui, links: std::ops::Range<usize>| {
        ui.memory_mut(|m| {
            for n in links {
                m.interested_in_focus(link_id(ui, n), layer);
            }
        });
    };
    let focused = ui.memory_mut(|m| {
        let focused = m.focused();
        for n in first..first + count {
            let lid = link_id(ui, n);
            if Some(lid) == focused {
                break;
            }
            m.interested_in_focus(lid, layer);
        }
        focused
    });
    let hover = r.hover_pos();
    let mut pointing = false;
    for (n, (chars, bytes, url)) in (first..).zip(link_runs(spans)) {
        let lid = link_id(ui, n);
        let has_focus = focused == Some(lid);
        let (mut first_row, mut bounds, mut under) = (None::<Rect>, Rect::NOTHING, false);
        let ring = has_focus.then(|| tokens::current(ui.ctx()));
        row_rects(&galley, chars, |_, rect| {
            let rect = rect.translate(pos.to_vec2());
            first_row.get_or_insert(rect);
            bounds = bounds.union(rect);
            under |= hover.is_some_and(|p| rect.contains(p));
            if let Some(t) = &ring {
                crate::controls::focus_ring(ui.painter(), rect, egui::CornerRadius::same(2), t);
            }
        });
        let Some(row) = first_row else {
            if has_focus {
                interested(ui, n + 1..first + count);
            }
            continue;
        };
        // the focused link is a real widget: it keeps focus and Enter or Space clicks it
        let mut activated = false;
        if has_focus {
            activated = ui.interact(row, lid, Sense::click()).clicked();
            interested(ui, n + 1..first + count);
        }
        if cx.accesskit != Some(false) {
            let text = galley.text().get(bytes).unwrap_or_default();
            let on = ui
                .ctx()
                .accesskit_node_builder(lid, |node| {
                    node.set_role(egui::accesskit::Role::Link);
                    node.set_label(text);
                    node.set_url(url);
                    node.set_bounds(egui::accesskit::Rect {
                        x0: bounds.min.x.into(),
                        y0: bounds.min.y.into(),
                        x1: bounds.max.x.into(),
                        y1: bounds.max.y.into(),
                    });
                    node.add_action(egui::accesskit::Action::Focus);
                    node.add_action(egui::accesskit::Action::Click);
                })
                .is_some();
            cx.accesskit = Some(on);
            activated |= on && ui.input(|i| i.has_accesskit_action_request(lid, egui::accesskit::Action::Click));
        }
        automation::register_fmt(ui, row, format_args!("{id}.link.{n}"), url, Role::Link, true, Details { value: Some(url), selected: None });
        pointing |= under;
        if (activated || under && r.clicked()) && safe_link(url) {
            ui.ctx().open_url(egui::OpenUrl::new_tab(url));
            cx.resp.opened = Some(url.to_string());
        }
    }
    cx.link_n = first + count;
    if pointing {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
}

/// A code block; true when its Copy was pressed.
fn code_block(ui: &mut Ui, cx: &Cx<'_>, n: usize, lang: &str, text: &str, key: GalleyKey) -> bool {
    let (id, style, labels) = (cx.id, cx.style, cx.labels);
    let mut copied = false;
    let r = Frame::new()
        .fill(style.code_bg)
        .stroke(Stroke::new(1.0, style.code_border))
        .corner_radius(style.corner)
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let small = FontId::new(style.body.size * 0.85, style.body.family.clone());
                let caption = if lang.is_empty() { &labels.code } else { lang };
                ui.label(egui::RichText::new(caption).font(small.clone()).color(style.weak));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let key = egui::Id::new((id, "md-copied", n));
                    let recently = ui.ctx().data(|d| d.get_temp::<f64>(key)).is_some_and(|t| ui.input(|i| i.time) - t < 1.5);
                    let word: &str = if recently { &labels.copied } else { &labels.copy };
                    let b = ui.add(egui::Button::new(egui::RichText::new(word).font(small).color(style.weak)).frame(false));
                    automation::register(ui, &b, &format!("{id}.code.{n}.copy"), &labels.copy, Role::Button);
                    if b.clicked() {
                        ui.ctx().copy_text(text.to_string());
                        let now = ui.input(|i| i.time);
                        ui.ctx().data_mut(|d| d.insert_temp(key, now));
                        crate::wake::wake_after(ui.ctx(), crate::wake::Reason::TIMER, std::time::Duration::from_millis(1600));
                        copied = true;
                    }
                });
            });
            // a known language is coloured from the theme's syntax colours
            let theme = ashlar_syntax::Lang::from_name(lang).map(|_| SyntaxTheme::from_tokens(&tokens::current(ui.ctx())));
            let key = match &theme {
                Some(th) => GalleyKey { revision: hash_of(&(key.revision, th.revision())), ..key },
                None => key,
            };
            egui::ScrollArea::horizontal().id_salt((id, "md-code", n)).show(ui, |ui| {
                let galley = block_galley(ui, key, f32::INFINITY, || {
                    let mut job = match &theme {
                        Some(th) => highlight_job(Some(lang), text, style.mono.clone(), th),
                        None => LayoutJob::single_section(
                            text.to_string(),
                            TextFormat { font_id: style.mono.clone(), color: style.code_text, ..Default::default() },
                        ),
                    };
                    if key.found {
                        mark(&mut job, cx.query, style.find_bg);
                    }
                    job
                });
                ui.add(egui::Label::new(galley).selectable(true));
            });
        });
    automation::register_fmt(
        ui,
        r.response.rect,
        format_args!("{id}.code.{n}"),
        lang,
        Role::Other,
        true,
        Details { value: Some(text.get(..text.len().min(200)).unwrap_or_default()), selected: None },
    );
    copied
}

/// Space between table columns.
const COLUMN_GAP: f32 = 14.0;

/// Each column's natural width (no wrapping) and narrowest width (its longest word).
fn measure_columns(ui: &Ui, style: &MarkdownStyle, header: &[Vec<Span>], rows: &[Vec<Vec<Span>>], cols: usize) -> (Vec<f32>, Vec<f32>) {
    let mut natural = vec![0.0f32; cols];
    let mut narrow = vec![0.0f32; cols];
    ui.ctx().fonts_mut(|f| {
        let mut measure = |c: usize, spans: &[Span], bold: bool| {
            let (mut line, mut word, mut longest) = (0.0f32, 0.0f32, 0.0f32);
            for s in spans {
                let font = style.format(s, bold, &style.body, style.text).font_id;
                for ch in s.text.chars() {
                    let w = f.glyph_width(&font, ch);
                    line += w;
                    if ch.is_whitespace() {
                        word = 0.0;
                    } else {
                        word += w;
                        longest = longest.max(word);
                    }
                }
            }
            if let (Some(n), Some(m)) = (natural.get_mut(c), narrow.get_mut(c)) {
                *n = n.max(line.ceil());
                *m = m.max(longest.ceil());
            }
        };
        for (c, cell) in header.iter().enumerate().take(cols) {
            measure(c, cell, true);
        }
        for row in rows {
            for (c, cell) in row.iter().enumerate().take(cols) {
                measure(c, cell, false);
            }
        }
    });
    (natural, narrow)
}

/// Column widths for `available` points: natural widths when they fit; otherwise each column
/// gives up width in proportion to how much it can wrap, never below its longest word.
fn fit_columns(natural: &[f32], narrow: &[f32], available: f32) -> Vec<f32> {
    let gaps = COLUMN_GAP * natural.len().saturating_sub(1) as f32;
    let total: f32 = natural.iter().sum();
    let floor: f32 = narrow.iter().sum();
    let room = (available - gaps).max(0.0);
    if total <= room {
        return natural.to_vec();
    }
    if floor >= room {
        return narrow.to_vec();
    }
    let share = (room - floor) / (total - floor).max(1e-3);
    natural.iter().zip(narrow).map(|(n, m)| m + (n - m).max(0.0) * share).collect()
}

fn table(
    ui: &mut Ui,
    cx: &Cx<'_>,
    header: &[Vec<Span>],
    rows: &[Vec<Vec<Span>>],
    key: GalleyKey,
    index: usize,
    columns: &mut Vec<(usize, u64, Vec<f32>)>,
) {
    let (id, n, style) = (cx.id, cx.table_n, cx.style);
    let cols = header.len().max(rows.iter().map(Vec::len).max().unwrap_or(0)).clamp(1, 24);
    let shown = rows.get(..rows.len().min(MAX_TABLE_ROWS)).unwrap_or_default();
    let available = ui.available_width();
    let measured_for = hash_of(&(key.revision, available.to_bits(), ui.ctx().pixels_per_point().to_bits()));
    let widths = match columns.iter().position(|(b, _, _)| *b == index) {
        Some(at) if columns.get(at).is_some_and(|(_, m, _)| *m == measured_for) => at,
        found => {
            let (natural, narrow) = measure_columns(ui, style, header, shown, cols);
            let w = fit_columns(&natural, &narrow, available);
            match found.and_then(|at| columns.get_mut(at).map(|c| (at, c))) {
                Some((at, c)) => {
                    *c = (index, measured_for, w);
                    at
                }
                None => {
                    columns.push((index, measured_for, w));
                    columns.len() - 1
                }
            }
        }
    };
    let widths: &[f32] = columns.get(widths).map(|(_, _, w)| w.as_slice()).unwrap_or_default();
    let cell = |ui: &mut Ui, c: usize, spans: &[Span], bold: bool| {
        let mut job = spans_job(spans, bold, style, &style.body, style.text);
        if key.found {
            mark(&mut job, cx.query, style.find_bg);
        }
        // a hair of slack: widths are summed glyph advances, the layout rounds
        job.wrap.max_width = widths.get(c).copied().unwrap_or(320.0) + 0.5;
        // laid out here: a label in the horizontal scroll area would not wrap a job
        let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
        ui.add(egui::Label::new(galley).selectable(true));
    };
    egui::ScrollArea::horizontal().id_salt((id, "md-table", n)).show(ui, |ui| {
        egui::Grid::new((id, "md-grid", n)).striped(true).min_col_width(0.0).spacing(egui::vec2(COLUMN_GAP, 4.0)).show(ui, |ui| {
            for c in 0..cols {
                cell(ui, c, header.get(c).map(Vec::as_slice).unwrap_or_default(), true);
            }
            ui.end_row();
            for row in shown {
                for c in 0..cols {
                    cell(ui, c, row.get(c).map(Vec::as_slice).unwrap_or_default(), false);
                }
                ui.end_row();
            }
        });
    });
    let hidden = rows.len().saturating_sub(shown.len());
    if hidden > 0 {
        let text = cx.labels.more_rows.replace("{n}", &hidden.to_string());
        let small = FontId::new(style.body.size * 0.85, style.body.family.clone());
        let r = ui.label(egui::RichText::new(&text).font(small).color(style.weak));
        automation::register_fmt(ui, r.rect, format_args!("{id}.table.{n}.more"), &text, Role::Label, true, Details::default());
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    const DOC: &str = "# Title\r\n\nSome *soft*\nwrapped text with a [link](https://example.com).\n| not | a table yet\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\nafter the table\n\n- one\n  more\n- [x] done\n  1. sub\n\n```rust\nfn main() {}\n```\n> quoted\n> twice\n---\n- - -\npara\n---\n## Tail `code` and **bold**\n~~~\nunclosed\n";

    #[test]
    fn streaming_parse_matches_a_full_parse_at_every_length() {
        for step in [1usize, 3, 7, 20] {
            let mut p = Parsed::new("", false);
            let mut end = 0;
            while end < DOC.len() {
                end = (end + step).min(DOC.len());
                while !DOC.is_char_boundary(end) {
                    end += 1;
                }
                let text = &DOC[..end];
                p.update(text);
                assert_eq!(p.doc.blocks, parse_doc(text, false).blocks, "step {step}, {end} bytes: {text:?}");
            }
        }
    }

    #[test]
    fn link_count_matches_link_runs() {
        let md = "[a](u) [**b** c](v)[d](v) e [f](u) https://x.example [](w) `[g](u)`";
        let spans = super::super::inline(md);
        let runs: Vec<_> = link_runs(&spans).map(|(c, b, u)| (c, b, u.to_string())).collect();
        assert_eq!(link_count(&spans), runs.len());
        assert_eq!(runs.iter().map(|r| r.2.as_str()).collect::<Vec<_>>(), ["u", "v", "u", "https://x.example"]);
    }

    #[test]
    fn finished_blocks_are_not_parsed_again() {
        let mut p = Parsed::new("# A\n\npara one\n\n", false);
        let kept = p.update("# A\n\npara one\n\npara two");
        assert_eq!(kept, Some(1), "the heading is final; the first paragraph waits for one more complete line");
        assert_eq!(p.update("# A\n\npara one\n\npara two"), None, "same text, nothing to do");
        assert_eq!(p.update("other"), Some(0), "not an append: parsed again");
        assert_eq!(p.doc.blocks, parse("other"));
    }

    fn style() -> MarkdownStyle {
        MarkdownStyle::from_visuals(&egui::Visuals::dark(), 13.0)
    }

    /// Draws `md` clipped to the top 200 points; returns the height it took and the link ids.
    fn draw_clipped(ctx: &egui::Context, md: &str) -> (f32, Vec<String>) {
        let mut height = 0.0;
        let _ = crate::test_support::run(ctx, egui::RawInput::default(), |ui| {
            let clip = egui::Rect::from_min_size(ui.max_rect().min, egui::vec2(600.0, 200.0));
            ui.set_clip_rect(clip);
            let top = ui.cursor().top();
            super::super::show(ui, "doc", md, &style());
            height = ui.cursor().top() - top;
        });
        let ids = crate::automation::query(ctx, "doc.link.").into_iter().map(|e| e.id).collect();
        (height, ids)
    }

    #[test]
    fn blocks_off_screen_keep_their_space_and_numbering() {
        let ctx = egui::Context::default();
        crate::automation::install(&ctx);
        let md: String = (0..200).map(|i| format!("Paragraph {i} with [a link](https://example.com/{i}).\n\n")).collect();
        let first = cache::galley_stats(&ctx);
        let (full, _) = draw_clipped(&ctx, &md);
        let measured = cache::galley_stats(&ctx);
        assert_eq!(measured.misses - first.misses, 200, "first frame: every block is drawn to measure it");
        let (culled, shown) = draw_clipped(&ctx, &md);
        let drawn = cache::galley_stats(&ctx).hits - measured.hits;
        assert!((full - culled).abs() < 0.1, "the same height whether drawn or skipped: {full} vs {culled}");
        assert!(drawn < 20, "only blocks in the clip rect are drawn: {drawn}");
        assert!(shown.contains(&"doc.link.0".to_string()));
        // appending keeps the numbering of the blocks before
        let more = format!("{md}One more [link](https://example.com/x).");
        let (_, _) = draw_clipped(&ctx, &more);
        let (_, after) = draw_clipped(&ctx, &more);
        assert!(after.contains(&"doc.link.0".to_string()) && !after.contains(&"doc.link.200".to_string()));
    }

    #[test]
    fn a_drawn_block_reuses_its_galley() {
        let ctx = egui::Context::default();
        let md = "A paragraph that stays the same.\n\nAnother one.";
        for _ in 0..3 {
            draw_clipped(&ctx, md);
        }
        let before = cache::galley_stats(&ctx);
        draw_clipped(&ctx, md);
        let after = cache::galley_stats(&ctx);
        assert_eq!(after.misses, before.misses, "nothing laid out again");
        assert_eq!(after.hits, before.hits + 2);
    }
}
