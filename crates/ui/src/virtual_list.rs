//! Virtualized list and table: only visible rows are laid out, painted and registered, so a
//! frame costs O(visible rows) whatever the row count.
//!
//! Rows have one uniform height, or variable heights given by a function. Variable heights are
//! summed once into an offset table (O(rows)) and kept until the row count or the caller's
//! `revision` changes; each frame then finds the first visible row by binary search.
//!
//! The list draws hover, selection and stripes, handles the keyboard (arrows, Home/End, Page
//! Up/Down, Enter) when it has the focus, and calls the app's painter for each visible row. The app
//! owns the data and the selection; the response says what the user asked for.
//!
//! ```
//! # use ashlar_ui::virtual_list::{RowHeights, VirtualList};
//! # fn ui(ui: &mut egui::Ui, names: &[String], selected: &mut Option<usize>) {
//! let out = VirtualList::new("files", names.len(), RowHeights::Uniform(22.0))
//!     .selected(*selected)
//!     .show(ui, |ui, row| {
//!         ui.label(&names[row.index]);
//!     });
//! if let Some(i) = out.select {
//!     *selected = Some(i);
//! }
//! # }
//! ```

use std::ops::Range;

use egui::{Id, Key, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};

use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};

/// Rows beyond this are not shown (an offset table for 10M rows is 80 MB).
pub const MAX_ROWS: usize = 10_000_000;

/// How tall rows are.
#[derive(Clone, Copy)]
pub enum RowHeights<'a> {
    Uniform(f32),
    /// `height(i)` for every row; called for all rows only when `revision` or the row count
    /// changes. Non-finite or negative heights count as 0, heights are capped at 10 000 points.
    Variable {
        revision: u64,
        height: &'a dyn Fn(usize) -> f32,
    },
}

fn sane_height(h: f32) -> f64 {
    if h.is_finite() { h.clamp(0.0, 10_000.0) as f64 } else { 0.0 }
}

/// Offsets of variable-height rows: `offsets[i]` is row `i`'s top, `offsets[rows]` the total.
#[derive(Clone, Default)]
struct Offsets {
    offsets: Vec<f64>,
    revision: u64,
    rows: usize,
}

impl Offsets {
    fn update(&mut self, rows: usize, revision: u64, height: &dyn Fn(usize) -> f32) -> bool {
        if self.rows == rows && self.revision == revision && self.offsets.len() == rows + 1 {
            return false;
        }
        self.offsets.clear();
        self.offsets.reserve(rows + 1);
        let mut y = 0.0f64;
        self.offsets.push(0.0);
        for i in 0..rows {
            y += sane_height(height(i));
            self.offsets.push(y);
        }
        self.rows = rows;
        self.revision = revision;
        true
    }
}

/// Geometry of the rows (no egui involved): total height, row spans and visible ranges.
pub(crate) enum Layout<'o> {
    Uniform { rows: usize, height: f64 },
    Variable(&'o [f64]),
}

impl Layout<'_> {
    fn rows(&self) -> usize {
        match self {
            Layout::Uniform { rows, .. } => *rows,
            Layout::Variable(o) => o.len().saturating_sub(1),
        }
    }
    fn total(&self) -> f64 {
        match self {
            Layout::Uniform { rows, height } => *rows as f64 * height,
            Layout::Variable(o) => o.last().copied().unwrap_or(0.0),
        }
    }
    /// (top, height) of row `i`.
    fn span(&self, i: usize) -> (f64, f64) {
        match self {
            Layout::Uniform { height, .. } => (i as f64 * height, *height),
            Layout::Variable(o) => {
                let top = o.get(i).copied().unwrap_or(0.0);
                (top, o.get(i + 1).copied().unwrap_or(top) - top)
            }
        }
    }
    /// The row containing `y` (clamped to the rows).
    fn row_at(&self, y: f64) -> usize {
        let rows = self.rows();
        if rows == 0 {
            return 0;
        }
        let i = match self {
            Layout::Uniform { height, .. } => {
                if *height > 0.0 {
                    (y.max(0.0) / height).floor() as usize
                } else {
                    0
                }
            }
            Layout::Variable(o) => o.partition_point(|top| *top <= y).saturating_sub(1),
        };
        i.min(rows - 1)
    }
    /// Rows intersecting `top..bottom`, plus `overscan` on each side.
    fn visible(&self, top: f64, bottom: f64, overscan: usize) -> Range<usize> {
        let rows = self.rows();
        if rows == 0 || bottom <= top {
            return 0..0;
        }
        let first = self.row_at(top).saturating_sub(overscan);
        let last = (self.row_at(bottom) + 1 + overscan).min(rows);
        first..last
    }
}

/// One visible row, as given to the row painter.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub index: usize,
    /// The row's full rect (the painter's `ui` is clipped to it, with horizontal padding).
    pub rect: Rect,
    pub selected: bool,
    pub hovered: bool,
}

/// What the list reports this frame.
pub struct ListResponse {
    /// The list's viewport.
    pub response: egui::Response,
    /// Row clicked with the pointer.
    pub clicked: Option<usize>,
    pub double_clicked: Option<usize>,
    pub secondary_clicked: Option<usize>,
    /// Row the user moved the selection to (click or keyboard).
    pub select: Option<usize>,
    /// Enter (or a double click) on the selected row.
    pub activated: Option<usize>,
    /// Rows laid out this frame.
    pub visible: Range<usize>,
}

/// A virtualized list. See the [module docs](self).
pub struct VirtualList<'a> {
    id: &'a str,
    rows: usize,
    heights: RowHeights<'a>,
    selected: Option<usize>,
    is_selected: Option<&'a dyn Fn(usize) -> bool>,
    overscan: usize,
    stripes: bool,
    scroll_to: Option<usize>,
    max_height: Option<f32>,
    row_label: Option<&'a dyn Fn(usize) -> &'a str>,
}

impl<'a> VirtualList<'a> {
    pub fn new(id: &'a str, rows: usize, heights: RowHeights<'a>) -> Self {
        VirtualList {
            id,
            rows: rows.min(MAX_ROWS),
            heights,
            selected: None,
            is_selected: None,
            overscan: 2,
            stripes: false,
            scroll_to: None,
            max_height: None,
            row_label: None,
        }
    }
    /// The selected (cursor) row: highlighted and moved by the keyboard.
    pub fn selected(mut self, row: Option<usize>) -> Self {
        self.selected = row;
        self
    }
    /// Highlight several rows (multi-selection); called for visible rows only.
    pub fn is_selected(mut self, f: &'a dyn Fn(usize) -> bool) -> Self {
        self.is_selected = Some(f);
        self
    }
    /// Extra rows laid out above and below the viewport.
    pub fn overscan(mut self, rows: usize) -> Self {
        self.overscan = rows.min(64);
        self
    }
    pub fn stripes(mut self, yes: bool) -> Self {
        self.stripes = yes;
        self
    }
    /// Scroll so this row is visible.
    pub fn scroll_to(mut self, row: Option<usize>) -> Self {
        self.scroll_to = row;
        self
    }
    pub fn max_height(mut self, h: f32) -> Self {
        self.max_height = Some(h);
        self
    }
    /// Accessible/automation label of a row (visible rows only).
    pub fn row_label(mut self, f: &'a dyn Fn(usize) -> &'a str) -> Self {
        self.row_label = Some(f);
        self
    }

    pub fn show(self, ui: &mut Ui, row: impl FnMut(&mut Ui, Row)) -> ListResponse {
        let base = ui.make_persistent_id(self.id);
        show_rows(ui, base, &self, LIST, None, row)
    }
}

pub(crate) struct Header<'h> {
    pub height: f32,
    pub paint: &'h mut dyn FnMut(&mut Ui, Rect),
}

/// Shared by the list, the table and the tree.
/// How `show_rows` registers itself for automation: its own role, and whether it registers
/// `<id>.row.<n>` per row (the tree registers its nodes by node id instead).
#[derive(Clone, Copy)]
pub(crate) struct Kind {
    pub role: Role,
    pub rows: bool,
}

pub(crate) const LIST: Kind = Kind { role: Role::List, rows: true };

pub(crate) fn show_rows(
    ui: &mut Ui,
    base: Id,
    list: &VirtualList<'_>,
    kind: Kind,
    header: Option<Header<'_>>,
    mut row_fn: impl FnMut(&mut Ui, Row),
) -> ListResponse {
    let t = tokens::current(ui.ctx());
    let offsets_id = base.with("offsets");
    let mut offsets = None;
    let layout = match list.heights {
        RowHeights::Uniform(h) => Layout::Uniform { rows: list.rows, height: sane_height(h).max(1.0) },
        RowHeights::Variable { revision, height } => {
            let mut o: Offsets = ui.data_mut(|d| d.remove_temp::<Offsets>(offsets_id)).unwrap_or_default();
            o.update(list.rows, revision, height);
            offsets = Some(o);
            match &offsets {
                Some(o) => Layout::Variable(&o.offsets),
                None => Layout::Uniform { rows: 0, height: 1.0 },
            }
        }
    };

    // the frame: a focusable background that receives keys
    let avail = ui.available_rect_before_wrap();
    let height = list.max_height.filter(|h| h.is_finite() && *h > 0.0).map_or(avail.height(), |h| h.min(avail.height().max(h.min(40.0))));
    let outer = Rect::from_min_size(avail.min, vec2(avail.width(), height.max(t.control_height)));
    let response = ui.interact(outer, base, Sense::focusable_noninteractive());
    let focused = response.has_focus();
    if focused {
        // arrows move the selection, not the focus
        let filter = egui::EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: false };
        ui.memory_mut(|m| m.set_focus_lock_filter(base, filter));
    }

    let mut select = None;
    let mut activated = None;
    let rows = layout.rows();
    let page = ((outer.height() as f64 / layout.span(list.selected.unwrap_or(0)).1.max(1.0)) as usize).max(1);
    if focused && rows > 0 {
        let cur = list.selected.map(|s| s.min(rows - 1));
        let (up, down, home, end, pgup, pgdn, enter) = ui.input(|i| {
            (
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::PageUp),
                i.key_pressed(Key::PageDown),
                i.key_pressed(Key::Enter),
            )
        });
        let next = if up {
            Some(cur.map_or(0, |c| c.saturating_sub(1)))
        } else if down {
            Some(cur.map_or(0, |c| (c + 1).min(rows - 1)))
        } else if home {
            Some(0)
        } else if end {
            Some(rows - 1)
        } else if pgup {
            Some(cur.map_or(0, |c| c.saturating_sub(page)))
        } else if pgdn {
            Some(cur.map_or(0, |c| (c + page).min(rows - 1)))
        } else {
            None
        };
        if next.is_some() && next != cur {
            select = next;
        }
        if enter {
            activated = cur;
        }
    }
    let scroll_target = select.or(list.scroll_to.filter(|r| *r < rows));

    // header (table)
    let mut body_rect = outer;
    if let Some(h) = header {
        let header_rect = Rect::from_min_size(outer.min, vec2(outer.width(), h.height));
        let mut hui = ui.new_child(UiBuilder::new().max_rect(header_rect).id_salt("ashlar_list_header"));
        hui.set_clip_rect(header_rect.intersect(ui.clip_rect()));
        (h.paint)(&mut hui, header_rect);
        body_rect.min.y = header_rect.max.y;
    }

    let view_h_id = base.with("view_h");
    let mut scroll = egui::ScrollArea::vertical().id_salt(base.with("scroll")).auto_shrink([false, false]).max_height(body_rect.height());
    if let Some(target) = scroll_target {
        let state = egui::scroll_area::State::load(ui.ctx(), ui.make_persistent_id(base.with("scroll")));
        let offset = state.map_or(0.0, |s| s.offset.y as f64);
        let view_h: f32 = ui.data(|d| d.get_temp(view_h_id)).unwrap_or(body_rect.height());
        let (top, h) = layout.span(target);
        if top < offset {
            scroll = scroll.vertical_scroll_offset(top as f32);
        } else if top + h > offset + view_h as f64 {
            scroll = scroll.vertical_scroll_offset((top + h - view_h as f64).max(0.0) as f32);
        }
    }

    let mut clicked = None;
    let mut double_clicked = None;
    let mut secondary_clicked = None;
    let mut visible = 0..0;
    let mut body = ui.new_child(UiBuilder::new().max_rect(body_rect).id_salt("ashlar_list_body"));
    let out = scroll.show_viewport(&mut body, |ui, viewport| {
        let total = layout.total().min(f32::MAX as f64 / 4.0) as f32;
        ui.set_height(total);
        ui.set_width(ui.available_width());
        let origin = ui.max_rect().min;
        let width = ui.max_rect().width();
        visible = layout.visible(viewport.min.y as f64, viewport.max.y as f64, list.overscan);
        let label_fn = list.row_label;
        let list_id = list.id;
        for i in visible.clone() {
            let (top, h) = layout.span(i);
            let rect = Rect::from_min_size(pos2(origin.x, origin.y + top as f32), vec2(width, h as f32));
            if h <= 0.0 {
                continue;
            }
            let resp = ui.interact(rect, base.with(("row", i)), Sense::click());
            let selected = list.selected == Some(i) || list.is_selected.is_some_and(|f| f(i));
            let hovered = resp.hovered();
            if resp.clicked() || resp.secondary_clicked() {
                ui.memory_mut(|m| m.request_focus(base));
            }
            if resp.clicked() {
                clicked = Some(i);
                if list.selected != Some(i) {
                    select = Some(i);
                }
            }
            if resp.double_clicked() {
                double_clicked = Some(i);
                activated = Some(i);
            }
            if resp.secondary_clicked() {
                secondary_clicked = Some(i);
            }
            paint_row_background(ui, &t, rect, i, selected, hovered, list.stripes, focused);
            let mut child = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(t.space.sm, 0.0))).id_salt(("row", i)));
            child.set_clip_rect(rect.intersect(ui.clip_rect()));
            if selected {
                child.visuals_mut().override_text_color = Some(t.selection_text);
            }
            row_fn(&mut child, Row { index: i, rect, selected, hovered });
            if kind.rows {
                let label = label_fn.map_or("", |f| f(i));
                automation::register_fmt(
                    ui,
                    rect,
                    format_args!("{list_id}.row.{i}"),
                    label,
                    Role::Row,
                    true,
                    Details { value: None, selected: Some(selected) },
                );
            }
        }
    });
    let view_h = out.inner_rect.height();
    ui.data_mut(|d| d.insert_temp(view_h_id, view_h));
    if let Some(o) = offsets {
        ui.data_mut(|d| d.insert_temp(offsets_id, o));
    }
    ui.advance_cursor_after_rect(outer);
    if focused {
        ui.painter().rect_stroke(outer, t.radius.sm, Stroke::new(1.0, t.focus.gamma_multiply(0.6)), StrokeKind::Inside);
    }
    automation::register_with(ui, &response, list.id, "", kind.role, Details::default());
    ListResponse { response, clicked, double_clicked, secondary_clicked, select, activated, visible }
}

fn paint_row_background(ui: &Ui, t: &Tokens, rect: Rect, i: usize, selected: bool, hovered: bool, stripes: bool, focused: bool) {
    let fill = if selected {
        if focused { t.selection } else { t.selection.gamma_multiply(0.7) }
    } else if hovered {
        t.surface.hover
    } else if stripes && i % 2 == 1 {
        t.surface.row_alt
    } else {
        return;
    };
    ui.painter().rect_filled(rect, 0, fill);
}

// ---------------------------------------------------------------------------------------------
// Table
// ---------------------------------------------------------------------------------------------

/// One table column.
#[derive(Clone, Copy, Debug)]
pub struct Column<'a> {
    pub title: &'a str,
    /// Initial width in points (the user's resized width is remembered per table).
    pub width: f32,
    pub min_width: f32,
    pub resizable: bool,
}

impl<'a> Column<'a> {
    pub fn new(title: &'a str, width: f32) -> Self {
        Column { title, width, min_width: 24.0, resizable: true }
    }
}

/// The sort indicator shown in a header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub column: usize,
    pub ascending: bool,
}

/// What the table reports: the list's response plus header clicks.
pub struct TableResponse {
    pub list: ListResponse,
    /// Header clicked (the app re-sorts and passes the new [`Sort`]).
    pub header_clicked: Option<usize>,
    /// Current column widths.
    pub widths: Vec<f32>,
}

/// A virtualized table: a [`VirtualList`] whose rows are split into resizable columns.
pub struct VirtualTable<'a> {
    list: VirtualList<'a>,
    columns: &'a [Column<'a>],
    sort: Option<Sort>,
}

/// Most columns a table shows.
pub const MAX_COLUMNS: usize = 256;

impl<'a> VirtualTable<'a> {
    pub fn new(id: &'a str, columns: &'a [Column<'a>], rows: usize, heights: RowHeights<'a>) -> Self {
        VirtualTable {
            list: VirtualList::new(id, rows, heights).stripes(true),
            columns: columns.get(..columns.len().min(MAX_COLUMNS)).unwrap_or(&[]),
            sort: None,
        }
    }
    pub fn selected(mut self, row: Option<usize>) -> Self {
        self.list = self.list.selected(row);
        self
    }
    pub fn is_selected(mut self, f: &'a dyn Fn(usize) -> bool) -> Self {
        self.list = self.list.is_selected(f);
        self
    }
    pub fn sort(mut self, sort: Option<Sort>) -> Self {
        self.sort = sort;
        self
    }
    pub fn scroll_to(mut self, row: Option<usize>) -> Self {
        self.list = self.list.scroll_to(row);
        self
    }
    pub fn max_height(mut self, h: f32) -> Self {
        self.list = self.list.max_height(h);
        self
    }
    pub fn row_label(mut self, f: &'a dyn Fn(usize) -> &'a str) -> Self {
        self.list = self.list.row_label(f);
        self
    }

    /// `cell(ui, row, column)` paints one visible cell.
    pub fn show(self, ui: &mut Ui, mut cell: impl FnMut(&mut Ui, usize, usize)) -> TableResponse {
        let t = tokens::current(ui.ctx());
        let base = ui.make_persistent_id(self.list.id);
        let widths_id = base.with("widths");
        let mut stored: Vec<f32> = ui.data_mut(|d| d.remove_temp::<Vec<f32>>(widths_id)).unwrap_or_default();
        if stored.len() != self.columns.len() {
            stored = self.columns.iter().map(|c| sane_width(c.width, c.min_width)).collect();
        }
        // read by the header and the rows, written by the header's resize handles
        let widths: Vec<std::cell::Cell<f32>> = stored.iter().map(|w| std::cell::Cell::new(*w)).collect();
        let mut header_clicked = None;
        let columns = self.columns;
        let sort = self.sort;
        let table_id = self.list.id;
        let header_h = t.control_height;
        let mut paint_header = |ui: &mut Ui, rect: Rect| {
            ui.painter().rect_filled(rect, 0, t.surface.panel);
            ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, t.separator));
            let mut x = rect.left();
            for (c, col) in columns.iter().enumerate() {
                let w = widths.get(c).map_or(col.width, std::cell::Cell::get);
                let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height()));
                let resp = ui.interact(cell, base.with(("header", c)), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(cell, 0, t.surface.hover);
                }
                if resp.clicked() {
                    header_clicked = Some(c);
                }
                let font = Tokens::font(t.type_scale.small);
                let galley =
                    crate::cache::galley(ui, (base, "header", c), crate::cache::hash_of(&(col.title, font.size.to_bits(), t.text.secondary)), || {
                        egui::text::LayoutJob::simple_singleline(col.title.to_owned(), font, t.text.secondary)
                    });
                let text_pos = pos2(cell.left() + t.space.sm, cell.center().y - galley.size().y / 2.0);
                let painter = ui.painter().with_clip_rect(cell.intersect(ui.clip_rect()));
                painter.galley(text_pos, galley.clone(), t.text.secondary);
                if let Some(s) = sort.filter(|s| s.column == c) {
                    let ax = (text_pos.x + galley.size().x + t.space.sm).min(cell.right() - 8.0);
                    let ay = cell.center().y;
                    let (a, b) = if s.ascending { (-3.0, 3.0) } else { (3.0, -3.0) };
                    painter.line_segment([pos2(ax - 3.5, ay + b * 0.5), pos2(ax, ay + a * 0.5 - 0.5)], Stroke::new(1.5, t.text.secondary));
                    painter.line_segment([pos2(ax, ay + a * 0.5 - 0.5), pos2(ax + 3.5, ay + b * 0.5)], Stroke::new(1.5, t.text.secondary));
                }
                resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, col.title));
                let sorted = sort.map(|s| s.column == c);
                automation::register_fmt(
                    ui,
                    cell,
                    format_args!("{table_id}.header.{c}"),
                    col.title,
                    Role::Header,
                    true,
                    Details { value: None, selected: sorted },
                );
                // resize handle on the right edge
                if col.resizable {
                    let handle = Rect::from_center_size(pos2(cell.right(), cell.center().y), vec2(6.0, cell.height()));
                    let hr = ui.interact(handle, base.with(("resize", c)), Sense::drag());
                    if hr.hovered() || hr.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
                    }
                    if hr.dragged()
                        && let Some(w) = widths.get(c)
                    {
                        w.set(sane_width(w.get() + hr.drag_delta().x, col.min_width));
                    }
                }
                ui.painter().vline(cell.right() - 0.5, rect.y_range().shrink(4.0), Stroke::new(1.0, t.separator));
                x += w;
            }
        };
        let list = show_rows(
            ui,
            base,
            &self.list,
            Kind { role: Role::Table, rows: true },
            Some(Header { height: header_h, paint: &mut paint_header }),
            |ui, row| {
                let mut x = row.rect.left();
                for (c, col) in columns.iter().enumerate() {
                    let w = widths.get(c).map_or(col.width, std::cell::Cell::get);
                    let rect = Rect::from_min_size(pos2(x, row.rect.top()), vec2(w, row.rect.height()));
                    let inner = rect.shrink2(vec2(t.space.sm, 0.0));
                    if inner.width() > 0.0 {
                        let mut cui =
                            ui.new_child(UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)).id_salt(c));
                        cui.set_clip_rect(inner.intersect(ui.clip_rect()));
                        cell(&mut cui, row.index, c);
                    }
                    x += w;
                }
            },
        );
        for (dst, w) in stored.iter_mut().zip(&widths) {
            *dst = w.get();
        }
        let out = stored.clone();
        ui.data_mut(|d| d.insert_temp(widths_id, stored));
        TableResponse { list, header_clicked, widths: out }
    }
}

fn sane_width(w: f32, min: f32) -> f32 {
    let min = if min.is_finite() { min.clamp(4.0, 2000.0) } else { 24.0 };
    if w.is_finite() { w.clamp(min, 4000.0) } else { min }
}

#[cfg(test)]
mod tests;
