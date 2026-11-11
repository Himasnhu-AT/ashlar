//! Horizontal scrolling for views whose rows can be wider than the view (unwrapped code, diffs).
//!
//! The view keeps vertical scrolling in its virtual list and shifts each row's text by
//! [`HScroll::offset`]. The horizontal wheel (Shift+wheel with a mouse) over the view moves it, and
//! a thin bar along the bottom shows where it is and can be dragged. Nothing is drawn, and no
//! input is taken, while every row fits.

use egui::{CornerRadius, Id, Rect, Sense, Ui, pos2, vec2};

use crate::tokens::Tokens;

/// Content wider than this many points is clipped (a hostile line cannot make the range absurd).
const MAX_WIDTH: f32 = 1.0e6;

pub(crate) struct HScroll {
    id: Id,
    /// How far the content is shifted left, in points.
    pub offset: f32,
    content: f32,
    view: f32,
}

impl HScroll {
    /// Read the offset kept under `id` and apply this frame's horizontal wheel when the pointer
    /// is over `area`. `content` is the widest row known so far, `view` the width it shows in.
    pub(crate) fn begin(ui: &Ui, id: Id, area: Rect, content: f32, view: f32) -> Self {
        let content = if content.is_finite() { content.clamp(0.0, MAX_WIDTH) } else { 0.0 };
        let view = if view.is_finite() { view.max(1.0) } else { 1.0 };
        let mut offset: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
        let max = (content - view).max(0.0);
        if max > 0.0 && ui.rect_contains_pointer(area) {
            let dx = ui.input(|i| i.smooth_scroll_delta.x);
            if dx != 0.0 && dx.is_finite() {
                offset -= dx;
                ui.ctx().input_mut(|i| i.smooth_scroll_delta.x = 0.0);
            }
        }
        let offset = if offset.is_finite() { offset.clamp(0.0, max) } else { 0.0 };
        HScroll { id, offset, content, view }
    }

    /// A row `width` points wide was drawn: the range grows to fit it from the next frame.
    pub(crate) fn see(&mut self, width: f32) {
        if width.is_finite() {
            self.content = self.content.max(width.min(MAX_WIDTH));
        }
    }

    /// Draw the bar along the bottom of `track` (when the content is wider than the view), take
    /// its drags, and keep the offset for the next frame.
    pub(crate) fn end(mut self, ui: &mut Ui, track: Rect, t: &Tokens) {
        let max = (self.content - self.view).max(0.0);
        if max > 0.0 && track.width() > 8.0 {
            let h = t.space.xs.max(3.0) * 1.5;
            let bar = Rect::from_min_max(pos2(track.left(), track.bottom() - h), track.max);
            let thumb_w = (bar.width() * self.view / self.content).clamp(h * 2.0, bar.width());
            let room = (bar.width() - thumb_w).max(1.0);
            let x = bar.left() + room * (self.offset / max).clamp(0.0, 1.0);
            let thumb = Rect::from_min_size(pos2(x, bar.top()), vec2(thumb_w, h));
            let resp = ui.interact(thumb, self.id.with("thumb"), Sense::drag());
            if resp.dragged() {
                self.offset = (self.offset + resp.drag_delta().x * max / room).clamp(0.0, max);
            }
            if ui.is_rect_visible(bar) {
                let painter = ui.painter_at(track);
                let r = CornerRadius::same((h / 2.0).round().clamp(0.0, 255.0) as u8);
                let fill = if resp.dragged() || resp.hovered() { t.text.secondary } else { t.border_strong };
                painter.rect_filled(thumb, r, fill);
            }
        } else {
            self.offset = 0.0;
        }
        let (id, offset) = (self.id, self.offset);
        ui.data_mut(|d| d.insert_temp(id, offset));
    }
}
