//! A virtualized message list that follows the newest message.

use std::collections::HashMap;
use std::ops::Range;

use egui::accesskit::{Live, Role as NodeRole};
use egui::{Layout, Rect, Ui, UiBuilder, pos2, vec2};

use super::{ChatLabels, default_labels};
use crate::automation::{self, Details, Role};
use crate::controls::{Button, Size, Variant};
use crate::tokens;

/// Messages beyond this are not shown.
pub const MAX_MESSAGES: usize = 1_000_000;
/// Height assumed for a message never drawn, before any message was measured.
const DEFAULT_ESTIMATE: f32 = 64.0;
/// A message taller than this is cut off.
const MAX_ROW_HEIGHT: f32 = 200_000.0;

/// What [`Transcript`] needs to know about a message.
pub trait TranscriptItem {
    /// Stable for the message's life.
    fn key(&self) -> u64;
    /// Changes whenever the message's content (and so its height) changes.
    fn revision(&self) -> u64;
}

#[derive(Clone, Copy, Debug)]
struct RowState {
    key: u64,
    rev: u64,
    width: f32,
    /// Height including the gap below; an estimate until measured.
    height: f32,
    measured: bool,
}

/// Row heights in a Fenwick tree: prefix sums and the row at a y in O(log rows).
#[derive(Clone, Default)]
struct Heights {
    rows: Vec<RowState>,
    tree: Vec<f64>,
    total: f64,
    revision: Option<u64>,
    measured_sum: f64,
    measured_count: usize,
}

fn lowbit(k: usize) -> usize {
    k & k.wrapping_neg()
}

impl Heights {
    fn estimate(&self) -> f32 {
        if self.measured_count == 0 { DEFAULT_ESTIMATE } else { (self.measured_sum / self.measured_count as f64) as f32 }
    }

    /// Follow `items`. Costs O(rows) only when `revision` or the row count changed.
    fn sync<T: TranscriptItem>(&mut self, items: &[T], revision: u64) {
        let items = items.get(..items.len().min(MAX_MESSAGES)).unwrap_or_default();
        if self.revision == Some(revision) && self.rows.len() == items.len() {
            return;
        }
        self.revision = Some(revision);
        let same_keys = self.rows.len() <= items.len() && self.rows.iter().zip(items).all(|(r, m)| r.key == m.key());
        if same_keys {
            // the common cases: a message streamed, or messages were appended
            for (r, m) in self.rows.iter_mut().zip(items) {
                r.rev = m.revision();
            }
            if self.rows.len() == items.len() {
                return;
            }
            let est = self.estimate();
            let new = items.get(self.rows.len()..).unwrap_or_default();
            self.rows.extend(new.iter().map(|m| RowState { key: m.key(), rev: m.revision(), width: 0.0, height: est, measured: false }));
        } else {
            // inserted, removed or reordered: keep what is known by key
            let old: HashMap<u64, RowState> = self.rows.drain(..).map(|r| (r.key, r)).collect();
            let est = self.estimate();
            self.rows = items
                .iter()
                .map(|m| match old.get(&m.key()) {
                    Some(r) => RowState { rev: m.revision(), ..*r },
                    None => RowState { key: m.key(), rev: m.revision(), width: 0.0, height: est, measured: false },
                })
                .collect();
        }
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let n = self.rows.len();
        self.tree = vec![0.0; n + 1];
        self.total = 0.0;
        for i in 1..=n {
            let h = self.rows.get(i - 1).map_or(0.0, |r| r.height as f64);
            self.total += h;
            if let Some(t) = self.tree.get_mut(i) {
                *t += h;
            }
            let j = i + lowbit(i);
            if j <= n {
                let v = self.tree.get(i).copied().unwrap_or(0.0);
                if let Some(t) = self.tree.get_mut(j) {
                    *t += v;
                }
            }
        }
    }

    fn add(&mut self, row: usize, delta: f64) {
        let mut k = row + 1;
        while k < self.tree.len() {
            if let Some(t) = self.tree.get_mut(k) {
                *t += delta;
            }
            k += lowbit(k);
        }
        self.total += delta;
    }

    /// Sum of the heights of rows `0..i`.
    fn prefix(&self, i: usize) -> f64 {
        let mut k = i.min(self.tree.len().saturating_sub(1));
        let mut s = 0.0;
        while k > 0 {
            s += self.tree.get(k).copied().unwrap_or(0.0);
            k -= lowbit(k);
        }
        s
    }

    /// The row covering `y` (the last row past the end).
    fn row_at(&self, y: f64) -> usize {
        let n = self.rows.len();
        if n == 0 || y <= 0.0 {
            return 0;
        }
        let mut pos = 0;
        let mut rem = y;
        let mut step = n.next_power_of_two();
        while step > 0 {
            let next = pos + step;
            if next <= n {
                let v = self.tree.get(next).copied().unwrap_or(0.0);
                if v <= rem {
                    pos = next;
                    rem -= v;
                }
            }
            step >>= 1;
        }
        pos.min(n - 1)
    }

    /// Record a measured height; true when it moved.
    fn measure(&mut self, row: usize, height: f32, width: f32) -> bool {
        let Some(r) = self.rows.get_mut(row) else { return false };
        let old = r.height;
        if !r.measured {
            self.measured_sum += height as f64;
            self.measured_count += 1;
        } else {
            self.measured_sum += height as f64 - old as f64;
        }
        r.measured = true;
        r.width = width;
        r.height = height;
        if (old - height).abs() > 0.5 {
            self.add(row, height as f64 - old as f64);
            true
        } else {
            false
        }
    }
}

/// What [`Transcript::show`] reports.
pub struct TranscriptResponse {
    /// The transcript's viewport.
    pub response: egui::Response,
    /// The view shows the newest message (and follows it while it grows).
    pub at_bottom: bool,
    /// "Jump to latest" was pressed.
    pub jumped: bool,
    /// Messages drawn this frame.
    pub visible: Range<usize>,
}

/// A virtualized, variable-height message list. Only the messages on screen are drawn; each
/// one's height is kept per (message key, revision, width) and drawn messages are measured as
/// they are painted, so a frame costs O(visible) whatever the message count.
///
/// With [`Transcript::stick_to_bottom`], the view follows the newest message while it streams.
/// Once the user scrolls up it stays put, and a "Jump to latest" button (`<id>.jump`) appears.
pub struct Transcript<'a, T: TranscriptItem> {
    id: &'a str,
    items: &'a [T],
    revision: u64,
    stick: bool,
    labels: Option<&'a ChatLabels>,
}

impl<'a, T: TranscriptItem> Transcript<'a, T> {
    /// `revision` changes whenever `items` changes (a message added, removed or streamed).
    pub fn new(id: &'a str, items: &'a [T], revision: u64) -> Self {
        Transcript { id, items, revision, stick: true, labels: None }
    }
    /// Follow the newest message unless the user scrolled away (on by default).
    pub fn stick_to_bottom(mut self, stick: bool) -> Self {
        self.stick = stick;
        self
    }
    pub fn labels(mut self, labels: &'a ChatLabels) -> Self {
        self.labels = Some(labels);
        self
    }

    /// Draw the visible messages with `row(ui, message)`, filling the available rect.
    pub fn show(self, ui: &mut Ui, mut row: impl FnMut(&mut Ui, &T)) -> TranscriptResponse {
        let labels: &ChatLabels = match self.labels {
            Some(l) => l,
            None => default_labels(),
        };
        let t = tokens::current(ui.ctx());
        let base = ui.make_persistent_id(("ashlar_ui::chat::transcript", self.id));
        let heights_id = base.with("heights");
        let jump_id = base.with("jump");
        let mut h: Heights = ui.data_mut(|d| d.remove_temp::<Heights>(heights_id)).unwrap_or_default();
        h.sync(self.items, self.revision);
        let items = self.items.get(..h.rows.len()).unwrap_or_default();
        let gap = t.space.md;

        let outer = ui.available_rect_before_wrap();
        let response = ui.interact(outer, base, egui::Sense::hover());
        ui.ctx().accesskit_node_builder(base, |node| {
            node.set_role(NodeRole::Log);
            node.set_live(Live::Polite);
            node.set_label(labels.transcript.as_str());
        });

        let jump: bool = ui.data(|d| d.get_temp(jump_id)).unwrap_or(false);
        let mut scroll = egui::ScrollArea::vertical().id_salt(base.with("scroll")).auto_shrink([false, false]).stick_to_bottom(self.stick);
        if jump {
            ui.data_mut(|d| d.remove_temp::<bool>(jump_id));
            scroll = scroll.vertical_scroll_offset(f32::MAX);
        }

        let mut visible = 0..0;
        let mut moved = false;
        let mut above_shift = 0.0_f64;
        let mut body = ui.new_child(UiBuilder::new().max_rect(outer).id_salt("ashlar_chat_body").accessibility_parent(base));
        let out = scroll.show_viewport(&mut body, |ui, viewport| {
            let origin = ui.max_rect().min;
            let width = ui.max_rect().width().max(1.0);
            let first = h.row_at(viewport.min.y as f64);
            let mut top = h.prefix(first);
            let mut i = first;
            while i < items.len() && top < viewport.max.y as f64 {
                let Some(item) = items.get(i) else { break };
                let rid = base.with(("row", item.key()));
                ui.ctx().accesskit_node_builder(rid, |node| {
                    node.set_role(NodeRole::Article);
                    node.set_position_in_set(i.saturating_add(1));
                    node.set_size_of_set(items.len());
                });
                let max = Rect::from_min_size(pos2(origin.x, origin.y + top as f32), vec2(width, MAX_ROW_HEIGHT));
                let mut child = ui.new_child(
                    UiBuilder::new().max_rect(max).id_salt(("row", item.key())).accessibility_parent(rid).layout(Layout::top_down(egui::Align::Min)),
                );
                child.set_clip_rect(ui.clip_rect());
                row(&mut child, item);
                let measured = (child.min_rect().height() + gap).clamp(1.0, MAX_ROW_HEIGHT);
                let old = h.rows.get(i).map_or(measured, |r| r.height);
                if h.measure(i, measured, width) {
                    moved = true;
                    if top < viewport.min.y as f64 {
                        above_shift += measured as f64 - old as f64;
                    }
                }
                top += measured as f64;
                i += 1;
            }
            visible = first..i;
            ui.set_min_height(h.total.min(f32::MAX as f64 / 4.0) as f32);
            ui.set_min_width(width);
        });
        ui.data_mut(|d| d.insert_temp(heights_id, h));

        let max_offset = (out.content_size.y - out.inner_rect.height()).max(0.0);
        let mut at_bottom = out.state.offset.y >= max_offset - 1.0;
        if above_shift != 0.0 && !at_bottom {
            // keep the message under the reader's eyes when one above it changed height
            let mut state = out.state;
            state.offset.y = (state.offset.y + above_shift as f32).clamp(0.0, max_offset);
            state.store(ui.ctx(), out.id);
            at_bottom = state.offset.y >= max_offset - 1.0;
        }
        if moved {
            // the layout settles on the next frame; then nothing changes and no frame is asked for
            ui.ctx().request_repaint();
        }

        let mut jumped = false;
        if self.stick && !at_bottom && !items.is_empty() {
            let strip =
                Rect::from_min_max(pos2(outer.min.x, outer.max.y - t.control_height - t.space.md), pos2(outer.max.x, outer.max.y - t.space.md));
            let mut bui = ui.new_child(UiBuilder::new().max_rect(strip).layout(Layout::top_down(egui::Align::Center)).accessibility_parent(base));
            if Button::new(self.id, &labels.jump_to_latest).part("jump").variant(Variant::Secondary).size(Size::Sm).show(&mut bui).clicked() {
                ui.data_mut(|d| d.insert_temp(jump_id, true));
                ui.ctx().request_repaint();
                jumped = true;
            }
        }
        ui.advance_cursor_after_rect(outer);
        automation::register_with(ui, &response, self.id, &labels.transcript, Role::List, Details::default());
        TranscriptResponse { response, at_bottom, jumped, visible }
    }
}
