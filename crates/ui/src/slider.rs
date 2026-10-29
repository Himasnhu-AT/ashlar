//! Slider and knob, reporting the same [`Edit`] phases as [`crate::value_field`].
//!
//! - [`Slider`]: a horizontal track. Dragging follows the pointer; a click jumps there (one-shot
//!   commit); arrow keys step.
//! - [`Knob`]: a rotary control. Dragging up or down turns it (Shift ×10, Alt or ⌘/Ctrl ×0.1), a double
//!   click returns it to its default, arrow keys step.
//!
//! Escape during a drag restores the start value and reports [`crate::edit::EditPhase::Cancel`].

use std::ops::RangeInclusive;

use egui::{Align2, CursorIcon, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, pos2, text::LayoutJob, vec2};

use crate::automation::{self, Details, Role};
use crate::cache;
use crate::edit::{Bounds, DragMode, Edit, drive_drag, snap};
use crate::tokens::{self, Tokens};
use crate::value_field::{Decimal, ValueFormat, ValueResponse};

const DEFAULT_FORMAT: Decimal = Decimal { decimals: 2, suffix: "" };

fn key_step_input(ui: &Ui) -> (f64, bool) {
    ui.input(|i| {
        let up = i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::ArrowRight);
        let down = i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::ArrowLeft);
        ((up as i8 - down as i8) as f64, i.modifiers.shift)
    })
}

fn key_edit(ui: &Ui, focused: bool, value: &mut f64, bounds: Bounds, step: f64, key_step: f64) -> Option<Edit> {
    if !focused {
        return None;
    }
    let (dir, coarse) = key_step_input(ui);
    if dir == 0.0 {
        return None;
    }
    let start = bounds.clamp(*value);
    let next = bounds.clamp(snap(start + dir * key_step * if coarse { 10.0 } else { 1.0 }, step));
    (next != start).then(|| {
        *value = next;
        Edit::commit(start, next)
    })
}

fn default_key_step(bounds: Bounds, step: f64) -> f64 {
    if step.is_finite() && step > 0.0 {
        step
    } else if bounds.finite() && bounds.max > bounds.min {
        (bounds.max - bounds.min) / 100.0
    } else {
        1.0
    }
}

fn value_text(ui: &Ui, id: egui::Id, value: f64, format: &dyn ValueFormat, t: &Tokens, color: egui::Color32) -> std::sync::Arc<egui::Galley> {
    let font = Tokens::font(t.type_scale.small);
    let revision = cache::hash_of(&(value.to_bits(), format.tag(), color, font.size.to_bits()));
    cache::galley(ui, (id, "slider-value"), revision, || {
        let mut text = String::new();
        format.format(value, &mut text);
        LayoutJob::simple_singleline(text, font, color)
    })
}

/// A horizontal slider. See the [module docs](self).
pub struct Slider<'a> {
    id: &'a str,
    label: &'a str,
    value: &'a mut f64,
    range: RangeInclusive<f64>,
    step: f64,
    width: Option<f32>,
    format: Option<&'a dyn ValueFormat>,
}

impl<'a> Slider<'a> {
    pub fn new(id: &'a str, label: &'a str, value: &'a mut f64, range: RangeInclusive<f64>) -> Self {
        Slider { id, label, value, range, step: 0.0, width: None, format: None }
    }
    /// Snap to multiples of `step`; arrow keys move by one step.
    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
    /// Show the value as text after the track.
    pub fn show_value(mut self, format: &'a dyn ValueFormat) -> Self {
        self.format = Some(format);
        self
    }

    pub fn show(self, ui: &mut Ui) -> ValueResponse {
        let t = tokens::current(ui.ctx());
        let bounds = Bounds::new(&self.range);
        let id = ui.make_persistent_id(self.id);
        let width = self.width.filter(|w| w.is_finite() && *w > 0.0).unwrap_or(t.control_height * 6.0).min(4096.0);
        let text_w = if self.format.is_some() { t.control_height * 2.0 } else { 0.0 };
        let (outer, _) = ui.allocate_exact_size(vec2(width + text_w, t.control_height), Sense::hover());
        let track = Rect::from_min_size(outer.min, vec2(width, t.control_height));
        let response = ui.interact(track, id, Sense::click_and_drag());
        let enabled = response.enabled();
        let handle_r = (t.control_height * 0.3).max(4.0);
        let rail = track.shrink2(vec2(handle_r, 0.0));
        let pointer_value = response.interact_pointer_pos().map(|p| bounds.lerp(((p.x - rail.left()) / rail.width().max(1.0)) as f64));

        let value = self.value;
        let mut edit = None;
        if bounds.finite() {
            edit = drive_drag(ui, &response, value, bounds, self.step, DragMode::<fn(Vec2, f64) -> f64>::Absolute(pointer_value));
            if edit.is_none()
                && response.clicked()
                && let Some(v) = pointer_value
            {
                let start = bounds.clamp(*value);
                let v = bounds.clamp(snap(v, self.step));
                if v != start {
                    *value = v;
                    edit = Some(Edit::commit(start, v));
                }
            }
            if edit.is_none() {
                edit = key_edit(ui, crate::edit::has_keyboard(&response) && enabled, value, bounds, self.step, default_key_step(bounds, self.step));
            }
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }

        let shown = bounds.clamp(*value);
        let frac = bounds.fraction(shown) as f32;
        let painter = ui.painter_at(outer.expand(2.0));
        let rail_h = 4.0;
        let rail_rect = Rect::from_center_size(rail.center(), vec2(rail.width(), rail_h));
        let (fill, handle_fill) = if enabled { (t.accent, t.text.on_accent) } else { (t.text.disabled, t.surface.raised) };
        painter.rect_filled(rail_rect, rail_h / 2.0, t.surface.sunken);
        painter.rect_stroke(rail_rect, rail_h / 2.0, Stroke::new(1.0, t.border), StrokeKind::Outside);
        let filled = Rect::from_min_max(rail_rect.min, pos2(rail_rect.left() + rail_rect.width() * frac, rail_rect.bottom()));
        painter.rect_filled(filled, rail_h / 2.0, fill);
        let center = pos2(rail.left() + rail.width() * frac, rail.center().y);
        let hot = response.hovered() || response.dragged();
        painter.circle(center, handle_r + if hot { 1.0 } else { 0.0 }, handle_fill, Stroke::new(1.5, fill));
        if response.has_focus() {
            painter.circle_stroke(center, handle_r + 3.0, Stroke::new(1.5, t.focus));
        }
        let format = self.format.unwrap_or(&DEFAULT_FORMAT);
        let galley = value_text(ui, id, shown, format, &t, if enabled { t.text.secondary } else { t.text.disabled });
        if self.format.is_some() {
            let text_rect = Rect::from_min_max(pos2(track.right(), outer.top()), outer.max);
            let pos = Align2::RIGHT_CENTER.align_size_within_rect(galley.size(), text_rect).min;
            painter.galley(pos, galley.clone(), t.text.secondary);
        }
        response.widget_info(|| WidgetInfo::slider(enabled, shown, self.label));
        automation::register_with(ui, &response, self.id, self.label, Role::Slider, Details { value: Some(galley.text()), selected: None });
        ValueResponse { response, edit }
    }
}

/// A rotary knob. See the [module docs](self).
pub struct Knob<'a> {
    id: &'a str,
    label: &'a str,
    value: &'a mut f64,
    range: RangeInclusive<f64>,
    default: Option<f64>,
    step: f64,
    diameter: Option<f32>,
}

/// The knob's sweep: from 7:30 to 4:30, clockwise.
const START_ANGLE: f32 = std::f32::consts::PI * 0.75;
const SWEEP: f32 = std::f32::consts::PI * 1.5;
const ARC_SEGMENTS: usize = 32;

impl<'a> Knob<'a> {
    pub fn new(id: &'a str, label: &'a str, value: &'a mut f64, range: RangeInclusive<f64>) -> Self {
        Knob { id, label, value, range, default: None, step: 0.0, diameter: None }
    }
    /// The value a double click restores.
    pub fn default_value(mut self, value: f64) -> Self {
        self.default = Some(value);
        self
    }
    pub fn step(mut self, step: f64) -> Self {
        self.step = step;
        self
    }
    pub fn diameter(mut self, diameter: f32) -> Self {
        self.diameter = Some(diameter);
        self
    }

    pub fn show(self, ui: &mut Ui) -> ValueResponse {
        let t = tokens::current(ui.ctx());
        let bounds = Bounds::new(&self.range);
        let id = ui.make_persistent_id(self.id);
        let d = self.diameter.filter(|d| d.is_finite() && *d > 0.0).unwrap_or(t.control_height * 1.5).clamp(12.0, 512.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(d), Sense::hover());
        let response = ui.interact(rect, id, Sense::click_and_drag());
        let enabled = response.enabled();
        let value = self.value;
        let mut edit = None;
        if bounds.finite() && bounds.max > bounds.min {
            let per_point = (bounds.max - bounds.min) / 200.0;
            edit = drive_drag(ui, &response, value, bounds, self.step, DragMode::Relative(|delta: Vec2, mult| -(delta.y as f64) * per_point * mult));
            if edit.is_none()
                && response.double_clicked()
                && let Some(default) = self.default
            {
                let start = bounds.clamp(*value);
                let v = bounds.clamp(default);
                if v != start {
                    *value = v;
                    edit = Some(Edit::commit(start, v));
                }
            }
            if edit.is_none() {
                edit = key_edit(ui, crate::edit::has_keyboard(&response) && enabled, value, bounds, self.step, default_key_step(bounds, self.step));
            }
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
        let shown = bounds.clamp(*value);
        let frac = bounds.fraction(shown) as f32;
        let painter = ui.painter_at(rect.expand(3.0));
        let c = rect.center();
        let r = d / 2.0 - 2.0;
        let hot = response.hovered() || response.dragged();
        painter.circle(c, r * 0.72, if hot { t.surface.hover } else { t.surface.raised }, Stroke::new(1.0, t.border));
        let accent = if enabled { t.accent } else { t.text.disabled };
        arc(&painter, c, r, 0.0, 1.0, Stroke::new(2.5, t.border));
        arc(&painter, c, r, 0.0, frac, Stroke::new(2.5, accent));
        let a = START_ANGLE + SWEEP * frac;
        let dir = vec2(a.cos(), a.sin());
        painter.line_segment([c + dir * r * 0.25, c + dir * r * 0.65], Stroke::new(2.0, if enabled { t.text.primary } else { t.text.disabled }));
        if response.has_focus() {
            painter.circle_stroke(c, r + 2.0, Stroke::new(1.5, t.focus));
        }
        let galley = value_text(ui, id, shown, &DEFAULT_FORMAT, &t, t.text.secondary);
        response.widget_info(|| WidgetInfo::slider(enabled, shown, self.label));
        automation::register_with(ui, &response, self.id, self.label, Role::Knob, Details { value: Some(galley.text()), selected: None });
        ValueResponse { response, edit }
    }
}

/// Paint the part `from..to` (fractions of the sweep) of the knob arc as line segments (no
/// per-frame point buffer).
fn arc(painter: &egui::Painter, c: Pos2, r: f32, from: f32, to: f32, stroke: Stroke) {
    let (from, to) = (from.clamp(0.0, 1.0), to.clamp(0.0, 1.0));
    if to <= from {
        return;
    }
    let n = ((ARC_SEGMENTS as f32 * (to - from)).ceil() as usize).max(1);
    let point = |f: f32| {
        let a = START_ANGLE + SWEEP * f;
        c + vec2(a.cos(), a.sin()) * r
    };
    let mut prev = point(from);
    for i in 1..=n {
        let next = point(from + (to - from) * i as f32 / n as f32);
        painter.line_segment([prev, next], stroke);
        prev = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::EditPhase;
    use egui_kittest::Harness;

    struct State {
        value: f64,
        edits: Vec<Edit>,
        rect: Rect,
    }

    fn slider() -> Harness<'static, State> {
        Harness::builder().with_size(vec2(400.0, 60.0)).build_ui_state(
            |ui, s: &mut State| {
                let fmt = Decimal { decimals: 0, suffix: "" };
                let out = Slider::new("mix.gain", "Gain", &mut s.value, 0.0..=100.0).step(1.0).width(200.0).show_value(&fmt).show(ui);
                s.rect = out.response.rect;
                s.edits.extend(out.edit);
            },
            State { value: 50.0, edits: Vec::new(), rect: Rect::NOTHING },
        )
    }

    #[test]
    fn slider_click_jumps_drag_follows_and_escape_restores() {
        let mut h = slider();
        h.step();
        let r = h.state().rect;
        let at = |f: f32| pos2(r.left() + r.width() * f, r.center().y);
        h.hover_at(at(0.1));
        h.step();
        h.drag_at(at(0.1));
        h.step();
        h.drop_at(at(0.1));
        h.step();
        assert_eq!(h.state().edits.len(), 1);
        assert_eq!(h.state().edits[0].phase, EditPhase::Commit);
        assert!(h.state().value < 15.0, "{}", h.state().value);
        let after_click = h.state().value;
        h.state_mut().edits.clear();
        h.hover_at(at(0.5));
        h.step();
        h.drag_at(at(0.5));
        h.step();
        for f in [0.6, 0.7, 0.9] {
            h.hover_at(at(f));
            h.step();
        }
        assert!(h.state().value > 80.0);
        h.key_press(Key::Escape);
        h.step();
        assert_eq!(h.state().value, after_click);
        let phases: Vec<_> = h.state().edits.iter().map(|e| e.phase).collect();
        assert_eq!(phases.first(), Some(&EditPhase::Begin));
        assert_eq!(phases.last(), Some(&EditPhase::Cancel));
        let e = automation::find(&h.ctx, "mix.gain").unwrap();
        assert_eq!(e.role, Role::Slider);
        assert_eq!(e.value, format!("{after_click:.0}"));
    }

    #[test]
    fn knob_turns_with_vertical_drag_and_double_click_resets() {
        let mut h = Harness::builder().with_size(vec2(200.0, 200.0)).with_step_dt(0.02).build_ui_state(
            |ui, s: &mut State| {
                let out = Knob::new("eq.freq", "Frequency", &mut s.value, 0.0..=1.0).default_value(0.5).diameter(48.0).show(ui);
                s.rect = out.response.rect;
                s.edits.extend(out.edit);
            },
            State { value: 0.5, edits: Vec::new(), rect: Rect::NOTHING },
        );
        h.step();
        let c = h.state().rect.center();
        h.hover_at(c);
        h.step();
        h.drag_at(c);
        h.step();
        for dy in [10.0, 20.0, 40.0] {
            h.hover_at(c - vec2(0.0, dy));
            h.step();
        }
        h.drop_at(c - vec2(0.0, 40.0));
        h.step();
        let v = h.state().value;
        assert!(v > 0.6 && v <= 0.71, "{v}");
        let phases: Vec<_> = h.state().edits.iter().map(|e| e.phase).collect();
        assert_eq!((phases.first(), phases.last()), (Some(&EditPhase::Begin), Some(&EditPhase::Commit)));
        h.state_mut().edits.clear();
        h.hover_at(c);
        h.step();
        for _ in 0..2 {
            h.drag_at(c);
            h.step();
            h.event(egui::Event::PointerButton { pos: c, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
            h.step();
        }
        h.step();
        assert_eq!(h.state().value, 0.5);
        assert_eq!(h.state().edits.last().map(|e| e.phase), Some(EditPhase::Commit));
        assert_eq!(automation::find(&h.ctx, "eq.freq").map(|e| e.role), Some(Role::Knob));
    }

    #[test]
    fn unbounded_or_empty_ranges_are_inert() {
        let mut h = Harness::builder().with_size(vec2(300.0, 100.0)).build_ui_state(
            |ui, s: &mut State| {
                let a = Slider::new("a", "a", &mut s.value, f64::NEG_INFINITY..=f64::INFINITY).show(ui);
                s.edits.extend(a.edit);
                let b = Knob::new("b", "b", &mut s.value, 1.0..=1.0).show(ui);
                s.edits.extend(b.edit);
                s.rect = a.response.rect;
            },
            State { value: f64::NAN, edits: Vec::new(), rect: Rect::NOTHING },
        );
        h.step();
        let c = h.state().rect.center();
        h.drag_at(c);
        h.step();
        h.hover_at(c + vec2(50.0, 0.0));
        h.step();
        h.drop_at(c);
        h.step();
        assert!(h.state().edits.is_empty());
    }
}
