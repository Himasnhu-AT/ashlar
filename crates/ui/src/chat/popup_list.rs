//! A small floating list of suggestions, drawn above an anchor (the `@` / `/` popup).

use egui::accesskit::Role as NodeRole;
use egui::{Align2, Frame, Id, Margin, Order, Rect, Sense, Stroke, Ui, vec2};

use crate::automation::{self, Details, Role};
use crate::tokens;

/// At most this many rows are shown; the window follows the cursor.
pub const MAX_VISIBLE: usize = 8;

/// A list of suggestions floating above `anchor`. The caller owns the cursor and moves it with
/// the keyboard (the focus stays in the text field); the list reports a pointer pick.
/// Rows register as `<id>.item.<n>` (`n` counts from 0 over all items).
pub struct PopupList<'a, S: AsRef<str>> {
    id: &'a str,
    items: &'a [S],
    cursor: &'a mut usize,
    label: &'a str,
}

impl<'a, S: AsRef<str>> PopupList<'a, S> {
    pub fn new(id: &'a str, items: &'a [S], cursor: &'a mut usize) -> Self {
        PopupList { id, items, cursor, label: "" }
    }
    /// The accessible name.
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    /// Draw above `anchor`, as wide as it; returns the item clicked.
    pub fn show_above(self, ui: &Ui, anchor: Rect) -> Option<usize> {
        let n = self.items.len();
        if n == 0 {
            return None;
        }
        *self.cursor = (*self.cursor).min(n - 1);
        let t = tokens::current(ui.ctx());
        let row_h = t.control_height;
        let first = (*self.cursor + 1).saturating_sub(MAX_VISIBLE);
        let last = (first + MAX_VISIBLE).min(n);
        let base = Id::new(("ashlar_ui::chat::popup", self.id));
        let mut picked = None;
        egui::Area::new(base).order(Order::Foreground).fixed_pos(anchor.left_top() - vec2(0.0, t.space.xs)).pivot(Align2::LEFT_BOTTOM).show(
            ui.ctx(),
            |ui| {
                ui.ctx().accesskit_node_builder(base, |node| {
                    node.set_role(NodeRole::ListBox);
                    node.set_label(self.label);
                });
                Frame::new()
                    .fill(t.surface.raised)
                    .stroke(Stroke::new(1.0, t.border))
                    .corner_radius(t.radius.md)
                    .inner_margin(Margin::same(t.space.xxs as i8))
                    .show(ui, |ui| {
                        let width = anchor.width().max(160.0);
                        for i in first..last {
                            let Some(item) = self.items.get(i) else { break };
                            let (rect, resp) = ui.allocate_exact_size(vec2(width, row_h), Sense::click());
                            let selected = i == *self.cursor;
                            if selected || resp.hovered() {
                                let fill = if selected { t.selection } else { t.surface.row_alt };
                                ui.painter().rect_filled(rect, t.radius.sm, fill);
                            }
                            let color = if selected { t.selection_text } else { t.text.primary };
                            ui.painter().text(
                                rect.left_center() + vec2(t.space.sm, 0.0),
                                Align2::LEFT_CENTER,
                                item.as_ref(),
                                egui::FontId::proportional(t.type_scale.body),
                                color,
                            );
                            ui.ctx().accesskit_node_builder(resp.id, |node| {
                                node.set_role(NodeRole::ListBoxOption);
                                node.set_label(item.as_ref());
                                node.set_selected(selected);
                                node.set_position_in_set(i.saturating_add(1));
                                node.set_size_of_set(n);
                            });
                            if resp.clicked() {
                                picked = Some(i);
                            }
                            automation::register_fmt(
                                ui,
                                rect,
                                format_args!("{}.item.{i}", self.id),
                                item.as_ref(),
                                Role::MenuItem,
                                true,
                                Details { value: None, selected: Some(selected) },
                            );
                        }
                    });
            },
        );
        if let Some(i) = picked {
            *self.cursor = i;
        }
        picked
    }
}
