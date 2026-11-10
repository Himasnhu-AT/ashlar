//! An on/off switch with a label.

use egui::{Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, pos2, vec2};

use super::{focus_ring, mix, text_galley};
use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};

/// Track size of a switch for a token set: `(width, height)`.
pub(crate) fn track_size(t: &Tokens) -> (f32, f32) {
    let h = (t.control_height * 0.75).round().max(8.0);
    (h * 2.0 - 4.0, h)
}

/// Track fill, track border and knob colours.
pub(crate) fn colors(t: &Tokens, on: bool, hovered: bool, pressed: bool) -> (Color32, Stroke, Color32) {
    if on {
        let fill = if pressed {
            mix(t.accent, Color32::BLACK, 0.25)
        } else if hovered {
            t.accent_hover
        } else {
            t.accent
        };
        (fill, Stroke::new(1.0, fill), t.text.on_accent)
    } else {
        let fill = if pressed {
            t.surface.pressed
        } else if hovered {
            t.surface.hover
        } else {
            t.surface.sunken
        };
        let border = if hovered || pressed { t.border_strong } else { t.border };
        (fill, Stroke::new(1.0, border), if hovered || pressed { t.text.primary } else { t.text.secondary })
    }
}

/// A switch: a click, Space or Enter flips `on`, and the response reports `changed()`. The label
/// (drawn right of the track, also the accessible name) is part of the click target.
pub struct Switch<'a> {
    id: &'a str,
    label: &'a str,
    on: &'a mut bool,
    reduced_motion: bool,
}

impl<'a> Switch<'a> {
    pub fn new(id: &'a str, label: &'a str, on: &'a mut bool) -> Self {
        Switch { id, label, on, reduced_motion: false }
    }
    /// Snap the knob instead of sliding it.
    pub fn reduced_motion(mut self, reduced: bool) -> Self {
        self.reduced_motion = reduced;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let (tw, th) = track_size(&t);
        let galley = (!self.label.is_empty()).then(|| text_galley(ui, ("ashlar_ui::controls::switch", self.id), self.label, t.type_scale.body));
        let gap = if galley.is_some() { t.space.sm } else { 0.0 };
        let text_w = galley.as_ref().map_or(0.0, |g| g.size().x);
        let height = galley.as_ref().map_or(th, |g| g.size().y.max(th));
        let (auto_id, rect) = ui.allocate_space(vec2(tw + gap + text_w, height));
        let id = if self.id.is_empty() { auto_id } else { ui.make_persistent_id(("ashlar_ui::controls::switch", self.id)) };
        let mut response = ui.interact(rect, id, Sense::click());
        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }
        let on = *self.on;
        let enabled = ui.is_enabled();
        response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, on, self.label));
        ui.ctx().accesskit_node_builder(response.id, |node| node.set_role(egui::accesskit::Role::Switch));

        let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - th / 2.0), vec2(tw, th));
        if ui.is_rect_visible(rect) {
            let time = if self.reduced_motion { 0.0 } else { ui.style().animation_time };
            let pos = ui.ctx().animate_bool_with_time(id, on, time);
            let pressed = enabled && response.is_pointer_button_down_on();
            let hovered = enabled && response.hovered();
            let (fill, stroke, knob) = colors(&t, on, hovered, pressed);
            let radius = CornerRadius::same((th / 2.0).round().clamp(0.0, 255.0) as u8);
            let painter = ui.painter();
            painter.rect(track, radius, fill, stroke, StrokeKind::Inside);
            let r = th / 2.0;
            let x = egui::lerp((track.left() + r)..=(track.right() - r), pos);
            painter.circle_filled(pos2(x, track.center().y), (r - 3.0).max(1.0), knob);
            if let Some(galley) = galley {
                painter.galley(pos2(track.right() + gap, rect.center().y - galley.size().y / 2.0), galley, t.text.primary);
            }
            if response.has_focus() {
                focus_ring(painter, track, CornerRadius::same(radius.nw.saturating_add(2)), &t);
            }
        }
        let value = if on { "on" } else { "off" };
        automation::register_with(ui, &response, self.id, self.label, Role::Switch, Details { value: Some(value), selected: Some(on) });
        response
    }
}
