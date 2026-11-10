//! Virtualized list and table: only visible rows are laid out, painted and registered, so a
//! frame costs O(visible rows) whatever the row count.
//!
//! Rows have one uniform height, or variable heights given by a function. Variable heights are
//! read once into a Fenwick tree (O(rows)) and kept until the row count or the caller's
//! `revision` changes; rows named in [`VirtualList::changed_rows`] are read again in O(log rows)
//! each. Each frame then finds the first visible row in O(log rows).
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

use egui::accesskit::Role as NodeRole;
use egui::{Event, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};

use ashlar_motion::gesture::{VelocityTracker, decay_k, deceleration, project, rubber_band};
use ashlar_motion::{Class, Curve, tokens as mtokens};

use crate::automation::{self, Details, Role};
use crate::motion;
use crate::selection::Selection;
use crate::tokens::{self, Tokens};

/// Rows beyond this are not shown (an offset table for 10M rows is 80 MB).
pub const MAX_ROWS: usize = 10_000_000;

/// How tall rows are.
#[derive(Clone, Copy)]
pub enum RowHeights<'a> {
    Uniform(f32),
    /// `height(i)` for every row; called for all rows only when `revision` or the row count
    /// changes, and for the rows in [`VirtualList::changed_rows`]. Non-finite or negative heights
    /// count as 0, heights are capped at 10 000 points.
    Variable {
        revision: u64,
        height: &'a dyn Fn(usize) -> f32,
    },
}

fn sane_height(h: f32) -> f64 {
    if h.is_finite() { h.clamp(0.0, 10_000.0) as f64 } else { 0.0 }
}

/// Variable row heights and their running sums, in a Fenwick tree: a row's top, the row at a
/// given y and a change to one row's height each cost O(log rows).
#[derive(Clone, Default)]
pub(crate) struct Offsets {
    heights: Vec<f32>,
    /// `tree[k]` (1-based) sums the heights of rows `k - lowbit(k) .. k`.
    tree: Vec<f64>,
    total: f64,
    revision: u64,
}

fn lowbit(k: usize) -> usize {
    k & k.wrapping_neg()
}

impl Offsets {
    /// Read every row's height when the row count or `revision` changed, else only `changed`
    /// rows. True when anything was read.
    fn update(&mut self, rows: usize, revision: u64, height: &dyn Fn(usize) -> f32, changed: &[usize]) -> bool {
        if self.heights.len() == rows && self.revision == revision && self.tree.len() == rows + 1 {
            let mut any = false;
            for &row in changed {
                if let Some(old) = self.heights.get(row).copied() {
                    let new = sane_height(height(row)) as f32;
                    if new != old {
                        self.set(row, old, new);
                        any = true;
                    }
                }
            }
            return any;
        }
        // two linear passes: every node takes its prefix sum, then (from the top down, so the
        // lower prefixes are still there) the prefix before its range is taken off
        self.heights.clear();
        self.heights.reserve(rows);
        self.tree.clear();
        self.tree.reserve(rows + 1);
        self.tree.push(0.0);
        let mut sum = 0.0f64;
        for i in 0..rows {
            let h = sane_height(height(i));
            self.heights.push(h as f32);
            sum += h;
            self.tree.push(sum);
        }
        for k in (1..=rows).rev() {
            let before = self.tree.get(k - lowbit(k)).copied().unwrap_or(0.0);
            if let Some(node) = self.tree.get_mut(k) {
                *node -= before;
            }
        }
        self.total = sum;
        self.revision = revision;
        true
    }

    fn set(&mut self, row: usize, old: f32, new: f32) {
        if let Some(h) = self.heights.get_mut(row) {
            *h = new;
        }
        let delta = new as f64 - old as f64;
        let mut k = row + 1;
        while let Some(node) = self.tree.get_mut(k) {
            *node += delta;
            k += lowbit(k);
        }
        // sums drift by rounding with each change; the total is read from the tree
        self.total = self.prefix(self.heights.len());
    }

    fn rows(&self) -> usize {
        self.heights.len()
    }

    /// Sum of the heights of rows `0..i` (row `i`'s top).
    fn prefix(&self, i: usize) -> f64 {
        let mut k = i.min(self.heights.len());
        let mut sum = 0.0;
        while k > 0 {
            sum += self.tree.get(k).copied().unwrap_or(0.0);
            k -= lowbit(k);
        }
        sum.max(0.0)
    }

    fn height(&self, i: usize) -> f64 {
        self.heights.get(i).copied().unwrap_or(0.0) as f64
    }

    /// The last row whose top is at or above `y` (rows of zero height are passed over).
    fn row_at(&self, y: f64) -> usize {
        let n = self.heights.len();
        let mut pos = 0usize;
        let mut rest = y;
        let mut step = if n == 0 { 0 } else { 1usize << (usize::BITS - 1 - n.leading_zeros()) };
        while step > 0 {
            if let Some(&sum) = self.tree.get(pos + step)
                && sum <= rest
            {
                pos += step;
                rest -= sum;
            }
            step >>= 1;
        }
        pos
    }
}

/// Geometry of the rows (no egui involved): total height, row spans and visible ranges.
pub(crate) enum Layout<'o> {
    Uniform { rows: usize, height: f64 },
    Variable(&'o Offsets),
}

impl Layout<'_> {
    fn rows(&self) -> usize {
        match self {
            Layout::Uniform { rows, .. } => *rows,
            Layout::Variable(o) => o.rows(),
        }
    }
    fn total(&self) -> f64 {
        match self {
            Layout::Uniform { rows, height } => *rows as f64 * height,
            Layout::Variable(o) => o.total,
        }
    }
    /// (top, height) of row `i`.
    fn span(&self, i: usize) -> (f64, f64) {
        match self {
            Layout::Uniform { height, .. } => (i as f64 * height, *height),
            Layout::Variable(o) => (o.prefix(i), o.height(i)),
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
            Layout::Variable(o) => o.row_at(y),
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
    type_ahead: Option<&'a dyn Fn(usize) -> &'a str>,
    selection: Option<&'a mut Selection>,
    changed_rows: &'a [usize],
    fling: bool,
}

/// Typed letters within this many seconds of each other build one type-ahead prefix.
pub const TYPE_AHEAD_SECS: f64 = 1.0;

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
            type_ahead: None,
            selection: None,
            changed_rows: &[],
            fling: false,
        }
    }
    /// Drag the list to scroll it (off by default): let go while moving and it keeps going, then
    /// eases to rest; pulled past either end it stretches with rubber-band resistance and springs
    /// back. Reduced motion keeps the drag but drops the fling.
    pub fn fling(mut self, on: bool) -> Self {
        self.fling = on;
        self
    }
    /// Rows whose height changed since the last frame, with [`RowHeights::Variable`]: only these
    /// are measured again (O(log rows) each) while the revision and row count stay the same.
    pub fn changed_rows(mut self, rows: &'a [usize]) -> Self {
        self.changed_rows = rows;
        self
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
    /// Accessible/automation label of a row (visible rows only): the screen reader's name for it.
    pub fn row_label(mut self, f: &'a dyn Fn(usize) -> &'a str) -> Self {
        self.row_label = Some(f);
        self
    }
    /// Type-ahead: letters typed while the list has the focus, within [`TYPE_AHEAD_SECS`] of
    /// each other, move the cursor to the next row whose text starts with them (ignoring case).
    /// `text(row)` is called only on a keystroke, from the cursor on.
    pub fn type_ahead(mut self, text: &'a dyn Fn(usize) -> &'a str) -> Self {
        self.type_ahead = Some(text);
        self
    }
    /// Multi-selection owned by the app: click, Shift-click and Shift+arrows, ⌘/Ctrl-click and
    /// ⌘A update it in place. Its cursor replaces [`selected`](Self::selected); rows from
    /// [`is_selected`](Self::is_selected) still show as selected.
    pub fn selection(mut self, selection: &'a mut Selection) -> Self {
        self.selection = Some(selection);
        self
    }

    pub fn show(mut self, ui: &mut Ui, row: impl FnMut(&mut Ui, Row)) -> ListResponse {
        let base = ui.make_persistent_id(self.id);
        show_rows(ui, base, &mut self, LIST, None, row)
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
    /// AccessKit roles of the container and of each visible row.
    pub node: NodeRole,
    pub item: NodeRole,
}

pub(crate) const LIST: Kind = Kind { role: Role::List, rows: true, node: NodeRole::List, item: NodeRole::ListItem };

/// The widget id of visible row `i` (its AccessKit node id).
pub(crate) fn row_id(base: Id, i: usize) -> Id {
    base.with(("row", i))
}

/// `s` starts with `prefix`, ignoring case.
fn starts_with_ci(s: &str, prefix: &str) -> bool {
    let mut s = s.chars().flat_map(char::to_lowercase);
    prefix.chars().flat_map(char::to_lowercase).all(|p| s.next() == Some(p))
}

/// Feed this frame's typed text into the type-ahead buffer and find the row it names.
fn type_ahead<'t>(ui: &Ui, base: Id, rows: usize, cursor: Option<usize>, text: &dyn Fn(usize) -> &'t str) -> Option<usize> {
    let typed: String = ui.input(|i| {
        if i.modifiers.command || i.modifiers.ctrl || i.modifiers.alt {
            return String::new();
        }
        i.events.iter().filter_map(|e| if let Event::Text(t) = e { Some(t.as_str()) } else { None }).collect()
    });
    if typed.is_empty() || rows == 0 {
        return None;
    }
    let key = base.with("type_ahead");
    let now = ui.input(|i| i.time);
    let (mut buf, last): (String, f64) = ui.data_mut(|d| d.get_temp(key)).unwrap_or_default();
    if !(now - last).is_finite() || now - last > TYPE_AHEAD_SECS || now < last {
        buf.clear();
    }
    let fresh = buf.is_empty();
    for c in typed.chars().filter(|c| !c.is_control()) {
        if (c != ' ' || !buf.is_empty()) && buf.len() < 64 {
            buf.push(c);
        }
    }
    ui.data_mut(|d| d.insert_temp(key, (buf.clone(), now)));
    if buf.is_empty() {
        return None;
    }
    let find = |prefix: &str, from: usize| (0..rows).map(|k| from.saturating_add(k) % rows).find(|&r| starts_with_ci(text(r), prefix));
    // a new prefix starts after the cursor; a longer one may stay on it
    let from = cursor.map_or(0, |c| if fresh { c.saturating_add(1) } else { c });
    find(&buf, from).or_else(|| {
        // "f", "f", "f" cycles through the rows starting with "f"
        let mut chars = buf.chars();
        let first = chars.next()?;
        chars.all(|c| c == first).then(|| find(first.encode_utf8(&mut [0; 4]), cursor.map_or(0, |c| c.saturating_add(1)))).flatten()
    })
}

pub(crate) fn show_rows(
    ui: &mut Ui,
    base: Id,
    list: &mut VirtualList<'_>,
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
            o.update(list.rows, revision, height, list.changed_rows);
            offsets = Some(o);
            match &offsets {
                Some(o) => Layout::Variable(o),
                None => Layout::Uniform { rows: 0, height: 1.0 },
            }
        }
    };

    // the frame: a focusable background that receives keys
    let avail = ui.available_rect_before_wrap();
    let height = list.max_height.filter(|h| h.is_finite() && *h > 0.0).map_or(avail.height(), |h| h.min(avail.height().max(h.min(40.0))));
    let outer = Rect::from_min_size(avail.min, vec2(avail.width(), height.max(t.control_height)));
    let response = ui.interact(outer, base, Sense::focusable_noninteractive());
    let mut sel = list.selection.take();
    // nodes are built only while a screen reader (or a test) has AccessKit enabled
    let accesskit = ui
        .ctx()
        .accesskit_node_builder(base, |node| {
            node.set_role(kind.node);
            if sel.is_some() {
                node.set_multiselectable();
            }
        })
        .is_some();
    let focused = crate::edit::has_keyboard(&response);
    if focused {
        // arrows move the selection, not the focus
        let filter = egui::EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: false };
        ui.memory_mut(|m| m.set_focus_lock_filter(base, filter));
    }

    let mut select = None;
    let mut activated = None;
    let rows = layout.rows();
    let cursor = sel.as_ref().and_then(|s| s.cursor()).or(list.selected);
    let page = ((outer.height() as f64 / layout.span(cursor.unwrap_or(0)).1.max(1.0)) as usize).max(1);
    if focused && rows > 0 {
        let cur = cursor.map(|s| s.min(rows - 1));
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
            list.type_ahead.and_then(|text| type_ahead(ui, base, rows, cur, text))
        };
        if let Some(s) = sel.as_deref_mut() {
            let (shift, all) = ui.input_mut(|i| (i.modifiers.shift, i.consume_key(Modifiers::COMMAND, Key::A)));
            if let Some(n) = next {
                if shift { s.extend_to(n) } else { s.select_only(n) }
            }
            if all {
                s.select_all(rows);
            }
        }
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

    let over = if list.fling { drive_fling(ui, base, &layout, body_rect, scroll_target.is_some(), &mut scroll) } else { 0.0 };

    let mut clicked = None;
    let mut double_clicked = None;
    let mut secondary_clicked = None;
    let mut visible = 0..0;
    let mut body = ui.new_child(UiBuilder::new().max_rect(body_rect).id_salt("ashlar_list_body").accessibility_parent(base));
    let out = scroll.show_viewport(&mut body, |ui, viewport| {
        let total = layout.total().min(f32::MAX as f64 / 4.0) as f32;
        ui.set_height(total);
        ui.set_width(ui.available_width());
        // past an end, the rows follow the rubber band
        let origin = ui.max_rect().min - vec2(0.0, over);
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
            let rid = row_id(base, i);
            let resp = ui.interact(rect, rid, Sense::click());
            let selected = sel.as_ref().is_some_and(|s| s.contains(i)) || list.selected == Some(i) || list.is_selected.is_some_and(|f| f(i));
            let hovered = resp.hovered();
            if resp.clicked() || resp.secondary_clicked() {
                ui.memory_mut(|m| m.request_focus(base));
            }
            if resp.clicked() {
                clicked = Some(i);
                if let Some(s) = sel.as_deref_mut() {
                    let m = ui.input(|inp| inp.modifiers);
                    if m.shift {
                        s.extend_to(i);
                    } else if m.command {
                        s.toggle(i);
                    } else {
                        s.select_only(i);
                    }
                }
                if cursor != Some(i) {
                    select = Some(i);
                }
            }
            if accesskit {
                ui.ctx().accesskit_node_builder(rid, |node| {
                    node.set_role(kind.item);
                    if let Some(f) = label_fn {
                        node.set_label(f(i));
                    }
                    node.set_selected(selected);
                    node.set_position_in_set(i.saturating_add(1));
                    node.set_size_of_set(rows);
                });
            }
            if resp.double_clicked() {
                double_clicked = Some(i);
                activated = Some(i);
            }
            if resp.secondary_clicked() {
                secondary_clicked = Some(i);
            }
            paint_row_background(ui, &t, rect, i, selected, hovered, list.stripes, focused);
            let mut child =
                ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(t.space.sm, 0.0))).id_salt(("row", i)).accessibility_parent(rid));
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
    if list.fling {
        // the scroll area's own id (it lives in the body's child ui), for reading its offset
        ui.data_mut(|d| d.insert_temp(base.with("fling").with("scroll_id"), out.id));
    }
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

/// A list's drag-to-scroll state between frames.
#[derive(Clone, Copy, Debug, Default)]
struct ListFling {
    tracker: VelocityTracker<f32>,
    /// The offset the hand asks for (may be past either end).
    raw: f32,
    /// The last offset given to the tracker.
    tracked: f32,
    dragging: bool,
    /// A fling or spring-back is running toward `target`.
    running: bool,
    target: f32,
    /// The scroll offset set last frame (a different one means something else scrolled).
    last: f32,
}

/// Drag-to-scroll with fling and rubber band for the list `base` (see [`VirtualList::fling`]):
/// sets `scroll`'s offset while dragging or flinging and returns the rubber-band overscroll to
/// draw the rows with (negative above the top).
fn drive_fling(ui: &Ui, base: Id, layout: &Layout<'_>, body: Rect, scrolling_to: bool, scroll: &mut egui::ScrollArea) -> f32 {
    let ctx = ui.ctx().clone();
    let key = base.with("fling");
    let anim = key.with("offset");
    let view_h: f32 = ui.data(|d| d.get_temp(base.with("view_h"))).unwrap_or(body.height()).max(1.0);
    let max = (layout.total().min(f32::MAX as f64 / 4.0) as f32 - view_h).max(0.0);
    let scroll_id = ui.data(|d| d.get_temp::<Id>(key.with("scroll_id"))).unwrap_or_else(|| ui.make_persistent_id(base.with("scroll")));
    let current = egui::scroll_area::State::load(&ctx, scroll_id).map_or(0.0, |s| s.offset.y);
    // the list scrolls by hand itself (egui's own drag-to-scroll would fight it)
    *scroll = scroll.clone().scroll_source(egui::scroll_area::ScrollSource { drag: egui::scroll_area::DragScroll::Never, ..Default::default() });
    let drag = ui.interact(body, key, Sense::drag());
    let now = ui.input(|i| i.time);
    let mut f: ListFling = ui.data(|d| d.get_temp(key)).unwrap_or_default();
    let band = |raw: f32| {
        if raw < 0.0 {
            rubber_band(raw, view_h)
        } else if raw > max {
            max + rubber_band(raw - max, view_h)
        } else {
            raw
        }
    };
    let mut offset = None;
    if drag.drag_started() {
        f = ListFling { raw: current, tracked: current, dragging: true, last: current, ..ListFling::default() };
        f.tracker.add(now, current);
    }
    if f.dragging {
        f.raw -= drag.drag_delta().y;
        let shown = band(f.raw);
        // only movement is a sample: a still hand ages the last one until `velocity_at` says 0
        if shown != f.tracked {
            f.tracker.add(now, shown);
            f.tracked = shown;
        }
        motion::set(&ctx, anim, shown);
        offset = Some(shown);
        if drag.drag_stopped() || !drag.dragged() {
            f.dragging = false;
            let reduced = motion::prefs(&ctx).reduced;
            let v = if reduced { 0.0 } else { f.tracker.velocity_at(now) };
            let target = project(shown, v, deceleration::NORMAL).clamp(0.0, max);
            if target != shown {
                // never faster than the distance left can absorb: a fling stopped by an end eases
                // into it, and a stretched list springs back without swinging past
                let reach = (target - shown) * decay_k(deceleration::NORMAL);
                let v = if v.signum() == reach.signum() { v.abs().min(reach.abs()).copysign(v) } else { 0.0 };
                let curve = if reduced && v == 0.0 && (0.0..=max).contains(&shown) { Curve::Instant } else { Curve::Spring(mtokens::SETTLE.enter) };
                motion::release(&ctx, anim, v, target, curve);
                f.running = true;
                f.target = target;
            }
        }
    } else if f.running {
        if scrolling_to || (current - f.last).abs() > 0.5 {
            // the wheel, the keyboard or the app scrolled: they win
            f.running = false;
        } else {
            let o = motion::value(&ctx, anim, f.target, Class::Gesture, mtokens::SETTLE.enter);
            if o == f.target {
                f.running = false;
            }
            offset = Some(o);
        }
    }
    let mut over = 0.0;
    if let Some(o) = offset.filter(|o| o.is_finite()) {
        let inside = o.clamp(0.0, max);
        *scroll = scroll.clone().vertical_scroll_offset(inside);
        over = o - inside;
        f.last = inside;
    }
    ui.data_mut(|d| d.insert_temp(key, f));
    over
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
    /// See [`VirtualList::changed_rows`].
    pub fn changed_rows(mut self, rows: &'a [usize]) -> Self {
        self.list = self.list.changed_rows(rows);
        self
    }

    /// See [`VirtualList::type_ahead`].
    pub fn type_ahead(mut self, text: &'a dyn Fn(usize) -> &'a str) -> Self {
        self.list = self.list.type_ahead(text);
        self
    }
    /// See [`VirtualList::selection`].
    pub fn selection(mut self, selection: &'a mut Selection) -> Self {
        self.list = self.list.selection(selection);
        self
    }

    /// `cell(ui, row, column)` paints one visible cell.
    pub fn show(mut self, ui: &mut Ui, mut cell: impl FnMut(&mut Ui, usize, usize)) -> TableResponse {
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
            &mut self.list,
            Kind { role: Role::Table, rows: true, node: NodeRole::Table, item: NodeRole::Row },
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
