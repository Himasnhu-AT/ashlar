//! Drawing, with what is kept between frames: the parsed blocks (by content; streaming text is
//! parsed again only from its last open block), each block's galley (by block, width and style)
//! and each block's size, so blocks outside the clip rect only reserve their space.

use std::collections::HashMap;
use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontId, Frame, Id, Margin, Sense, Stroke, Ui, UiBuilder};

use super::parse::{Doc, parse_doc};
use super::{MAX_BYTES, MarkdownResponse, MarkdownStyle, Marker, MdBlock, Span, safe_link};
use crate::automation::{self, Details, Role};
use crate::cache::{self, hash_of};
use crate::code_view::{SyntaxTheme, highlight_job};
use crate::tokens;

/// What one `show` call keeps between frames.
#[derive(Default)]
struct State {
    /// `show` only: the parse of the text last shown.
    parsed: Option<Parsed>,
    /// Per block: content hash, links in it, drawn size (NaN: not measured at the current width).
    hashes: Vec<u64>,
    links: Vec<usize>,
    sizes: Vec<egui::Vec2>,
    /// What the sizes were measured with (width, style, scale).
    layout_key: u64,
}

struct Parsed {
    text_hash: u64,
    text_len: usize,
    doc: Doc,
}

impl Parsed {
    fn new(md: &str) -> Self {
        Parsed { text_hash: hash_of(&md), text_len: md.len(), doc: parse_doc(md) }
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
                let more = parse_doc(tail);
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
                self.doc = parse_doc(md);
                0
            }
        };
        self.text_hash = hash;
        self.text_len = md.len();
        Some(kept)
    }
}

impl State {
    /// Per-block hashes and link counts for `blocks`, from block `from` on; a block whose content
    /// changed is measured again.
    fn sync(&mut self, blocks: &[MdBlock], from: usize) {
        let from = from.min(self.hashes.len());
        let n = blocks.len();
        for (i, b) in blocks.iter().enumerate().skip(from) {
            let h = hash_of(b);
            if self.hashes.get(i) != Some(&h) {
                match self.hashes.get_mut(i) {
                    Some(old) => *old = h,
                    None => self.hashes.push(h),
                }
                let links = links_in(b);
                match self.links.get_mut(i) {
                    Some(old) => *old = links,
                    None => self.links.push(links),
                }
                match self.sizes.get_mut(i) {
                    Some(old) => *old = egui::Vec2::NAN,
                    None => self.sizes.push(egui::Vec2::NAN),
                }
            }
        }
        self.hashes.truncate(n);
        self.links.truncate(n);
        self.sizes.truncate(n);
    }
}

fn links_in(b: &MdBlock) -> usize {
    let count = |spans: &[Span]| spans.iter().filter(|s| s.link.is_some()).count();
    match b {
        MdBlock::Heading { spans, .. } | MdBlock::Paragraph(spans) | MdBlock::Item { spans, .. } | MdBlock::Quote(spans) => count(spans),
        MdBlock::Code { .. } | MdBlock::Rule | MdBlock::Table { .. } => 0,
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

/// [`super::show`]: parse (or reuse the parse of) `md`, then draw.
pub(super) fn show(ui: &mut Ui, id: &str, md: &str, style: &MarkdownStyle) -> MarkdownResponse {
    let sid = state_id(ui, id);
    let mut state = take(ui.ctx(), sid);
    let mut parsed = match state.parsed.take() {
        Some(mut p) => {
            if let Some(kept) = p.update(md) {
                state.sync(&p.doc.blocks, kept);
            }
            p
        }
        None => {
            let p = Parsed::new(md);
            state.sync(&p.doc.blocks, 0);
            p
        }
    };
    let resp = draw(ui, id, sid, &parsed.doc.blocks, &mut state, style);
    parsed.doc.blocks.shrink_to_fit();
    state.parsed = Some(parsed);
    put(ui.ctx(), sid, state);
    resp
}

/// [`super::show_blocks`]: blocks parsed by the caller (hashed every frame to find changes).
pub(super) fn show_blocks(ui: &mut Ui, id: &str, blocks: &[MdBlock], style: &MarkdownStyle) -> MarkdownResponse {
    let sid = state_id(ui, id);
    let mut state = take(ui.ctx(), sid);
    state.parsed = None;
    state.sync(blocks, 0);
    let resp = draw(ui, id, sid, blocks, &mut state, style);
    put(ui.ctx(), sid, state);
    resp
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

fn style_hash(s: &MarkdownStyle) -> u64 {
    let font = |f: &FontId| (f.size.to_bits(), f.family.clone());
    hash_of(&(
        [s.text, s.strong, s.weak, s.link, s.code_text, s.code_bg, s.code_border, s.rule],
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
}

fn draw(ui: &mut Ui, id: &str, sid: Id, blocks: &[MdBlock], state: &mut State, style: &MarkdownStyle) -> MarkdownResponse {
    let mut resp = MarkdownResponse::default();
    let mut code_n = 0usize;
    let mut link_n = 0usize;
    let mut table_n = 0usize;
    let gap = style.body.size * 0.45;
    let style_rev = style_hash(style);
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
        ));
        if layout_key != state.layout_key {
            state.layout_key = layout_key;
            state.sizes.fill(egui::Vec2::NAN);
        }
        let clip = ui.clip_rect();
        let mut skipped = Skipped::default();
        for (i, b) in blocks.iter().enumerate() {
            let size = state.sizes.get(i).copied().unwrap_or(egui::Vec2::NAN);
            let links = state.links.get(i).copied().unwrap_or(0);
            let block_rev = state.hashes.get(i).copied().unwrap_or(0);
            let is_code = matches!(b, MdBlock::Code { .. });
            let is_table = matches!(b, MdBlock::Table { .. });
            if size.y.is_finite() {
                let top = ui.cursor().top() + skipped.offset(gap);
                if top > clip.bottom() || top + size.y < clip.top() {
                    skipped.add(size, gap);
                    link_n += links;
                    code_n += usize::from(is_code);
                    table_n += usize::from(is_table);
                    continue;
                }
            }
            skipped.flush(ui);
            let first_link = link_n;
            let key = GalleyKey { key: sid.with(i).value(), revision: hash_of(&(block_rev, style_rev)) };
            let before = ui.cursor().top();
            let r = ui.scope_builder(UiBuilder::new().id(sid.with(("block", i))), |ui| {
                block(ui, id, b, key, style, gap, code_n, table_n, &mut link_n, &mut resp);
            });
            link_n = first_link + links;
            code_n += usize::from(is_code);
            table_n += usize::from(is_table);
            if let Some(s) = state.sizes.get_mut(i) {
                // how far the cursor moved, less the spacing: egui rounds each block's rect, so
                // this (not the rect's height) is what skipping the block must reproduce
                *s = egui::vec2(r.response.rect.width(), (ui.cursor().top() - before - gap).max(0.0));
            }
        }
        skipped.flush(ui);
    });
    resp
}

#[allow(clippy::too_many_arguments)]
fn block(
    ui: &mut Ui,
    id: &str,
    b: &MdBlock,
    key: GalleyKey,
    style: &MarkdownStyle,
    gap: f32,
    code_n: usize,
    table_n: usize,
    link_n: &mut usize,
    resp: &mut MarkdownResponse,
) {
    match b {
        MdBlock::Heading { level, spans } => {
            let size =
                style.heading_sizes.get(usize::from(level.saturating_sub(1))).or(style.heading_sizes.last()).copied().unwrap_or(style.body.size);
            let font = FontId::new(size, style.bold_family.clone().unwrap_or_else(|| style.body.family.clone()));
            ui.add_space(gap * 0.5);
            paragraph(ui, id, spans, true, key, style, &font, style.strong, link_n, resp);
        }
        MdBlock::Paragraph(spans) => paragraph(ui, id, spans, false, key, style, &style.body, style.text, link_n, resp),
        MdBlock::Quote(spans) => {
            let r = ui.horizontal_top(|ui| {
                ui.add_space(10.0);
                paragraph(ui, id, spans, false, key, style, &style.body, style.weak, link_n, resp);
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
                let (rect, _) = ui.allocate_exact_size(egui::vec2(w, style.body.size * 1.3), Sense::hover());
                ui.painter().text(rect.right_top(), egui::Align2::RIGHT_TOP, m, style.body.clone(), style.weak);
                paragraph(ui, id, spans, false, key, style, &style.body, style.text, link_n, resp);
            });
        }
        MdBlock::Rule => {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), gap), Sense::hover());
            ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(1.0, style.rule));
        }
        MdBlock::Code { lang, text, .. } => {
            if code_block(ui, id, code_n, lang, text, key, style) {
                resp.copied = Some(code_n);
            }
        }
        MdBlock::Table { header, rows } => table(ui, id, table_n, header, rows, style),
    }
}

/// The galley of a text block, from the context-wide cache: laid out again only when the block,
/// the style or the width changes. Placed the way `egui::Label` places a job.
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

#[allow(clippy::too_many_arguments)]
fn paragraph(
    ui: &mut Ui,
    id: &str,
    spans: &[Span],
    bold: bool,
    key: GalleyKey,
    style: &MarkdownStyle,
    font: &FontId,
    base: Color32,
    link_n: &mut usize,
    resp: &mut MarkdownResponse,
) {
    let galley = block_galley(ui, key, ui.available_width(), || {
        let mut job = LayoutJob::default();
        for s in spans {
            job.append(&s.text, 0.0, style.format(s, bold, font, base));
        }
        job
    });
    if spans.iter().all(|s| s.link.is_none()) {
        ui.add(egui::Label::new(galley).selectable(true));
        return;
    }
    let mut ranges: Vec<(std::ops::Range<usize>, &str)> = Vec::new();
    let mut chars = 0usize;
    for s in spans {
        let n = s.text.chars().count();
        if let Some(l) = &s.link {
            ranges.push((chars..chars + n, l));
        }
        chars += n;
    }
    // with links: lay out, paint, and map a click to the link under the pointer
    let (pos, galley, r) = egui::Label::new(galley).selectable(false).sense(Sense::click()).layout_in_ui(ui);
    let hovered_link = r.hover_pos().and_then(|p| {
        let c = galley.cursor_from_pos(p - pos).index.0;
        ranges.iter().find(|(range, _)| range.contains(&c)).map(|(_, u)| *u)
    });
    if hovered_link.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if r.clicked()
        && let Some(url) = hovered_link
        && safe_link(url)
    {
        ui.ctx().open_url(egui::OpenUrl::new_tab(url));
        resp.opened = Some(url.to_string());
    }
    ui.painter().galley(pos, galley.clone(), base);
    for (range, url) in &ranges {
        // a rect for each link (its first row), for automation
        let a = galley.pos_from_cursor(egui::text::CCursor::new(range.start));
        let b = galley.pos_from_cursor(egui::text::CCursor::new(range.end));
        let rect = if (a.min.y - b.min.y).abs() < 1.0 {
            egui::Rect::from_min_max(a.min, b.max)
        } else {
            egui::Rect::from_min_max(a.min, egui::pos2(galley.rect.right(), a.max.y))
        };
        automation::register_fmt(
            ui,
            rect.translate(pos.to_vec2()),
            format_args!("{id}.link.{}", *link_n),
            url,
            Role::Button,
            true,
            Details { value: Some(url), selected: None },
        );
        *link_n += 1;
    }
}

/// A code block; true when its Copy was pressed.
fn code_block(ui: &mut Ui, id: &str, n: usize, lang: &str, text: &str, key: GalleyKey, style: &MarkdownStyle) -> bool {
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
                ui.label(egui::RichText::new(if lang.is_empty() { "code" } else { lang }).font(small.clone()).color(style.weak));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let key = egui::Id::new((id, "md-copied", n));
                    let recently = ui.ctx().data(|d| d.get_temp::<f64>(key)).is_some_and(|t| ui.input(|i| i.time) - t < 1.5);
                    let b = ui.add(
                        egui::Button::new(egui::RichText::new(if recently { "Copied" } else { "Copy" }).font(small).color(style.weak)).frame(false),
                    );
                    automation::register(ui, &b, &format!("{id}.code.{n}.copy"), "Copy", Role::Button);
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
                Some(th) => GalleyKey { key: key.key, revision: hash_of(&(key.revision, th.revision())) },
                None => key,
            };
            egui::ScrollArea::horizontal().id_salt((id, "md-code", n)).show(ui, |ui| {
                let galley = block_galley(ui, key, f32::INFINITY, || match &theme {
                    Some(th) => highlight_job(Some(lang), text, style.mono.clone(), th),
                    None => LayoutJob::single_section(
                        text.to_string(),
                        TextFormat { font_id: style.mono.clone(), color: style.code_text, ..Default::default() },
                    ),
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

fn table(ui: &mut Ui, id: &str, n: usize, header: &[Vec<Span>], rows: &[Vec<Vec<Span>>], style: &MarkdownStyle) {
    let cols = header.len().max(rows.iter().map(Vec::len).max().unwrap_or(0)).clamp(1, 24);
    egui::ScrollArea::horizontal().id_salt((id, "md-table", n)).show(ui, |ui| {
        egui::Grid::new((id, "md-grid", n)).striped(true).spacing(egui::vec2(14.0, 4.0)).show(ui, |ui| {
            for c in 0..cols {
                cell(ui, header.get(c).map(Vec::as_slice).unwrap_or_default(), true, style);
            }
            ui.end_row();
            for row in rows.iter().take(500) {
                for c in 0..cols {
                    cell(ui, row.get(c).map(Vec::as_slice).unwrap_or_default(), false, style);
                }
                ui.end_row();
            }
        });
    });
}

fn cell(ui: &mut Ui, spans: &[Span], bold: bool, style: &MarkdownStyle) {
    let mut job = LayoutJob::default();
    for s in spans {
        job.append(&s.text, 0.0, style.format(s, bold, &style.body, style.text));
    }
    job.wrap.max_width = 320.0;
    ui.add(egui::Label::new(job).selectable(true));
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    const DOC: &str = "# Title\r\n\nSome *soft*\nwrapped text with a [link](https://example.com).\n| not | a table yet\n\n| a | b |\n|---|:-:|\n| 1 | 2 |\nafter the table\n\n- one\n  more\n- [x] done\n  1. sub\n\n```rust\nfn main() {}\n```\n> quoted\n> twice\n---\n- - -\npara\n---\n## Tail `code` and **bold**\n~~~\nunclosed\n";

    #[test]
    fn streaming_parse_matches_a_full_parse_at_every_length() {
        for step in [1usize, 3, 7, 20] {
            let mut p = Parsed::new("");
            let mut end = 0;
            while end < DOC.len() {
                end = (end + step).min(DOC.len());
                while !DOC.is_char_boundary(end) {
                    end += 1;
                }
                let text = &DOC[..end];
                p.update(text);
                assert_eq!(p.doc.blocks, parse_doc(text).blocks, "step {step}, {end} bytes: {text:?}");
            }
        }
    }

    #[test]
    fn finished_blocks_are_not_parsed_again() {
        let mut p = Parsed::new("# A\n\npara one\n\n");
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
            show(ui, "doc", md, &style());
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
