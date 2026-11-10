//! One-of-many choices: a segmented control and a radio group.
//!
//! Both are one Tab stop (the chosen option, or the first when none is): the arrow keys move
//! the choice and the focus together, Home and End jump to the ends, and a click picks an option.
//! Options listed in `disabled(&[..])` are drawn dimmed, cannot be picked and are skipped by the
//! keys.

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FocusDirection, Galley, Id, Key, Modifiers, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder, WidgetInfo,
    WidgetType, pos2, vec2,
};

use super::{focus_ring, text_galley};
use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};

/// Options beyond this many are not shown (a choice control is not a list).
pub const MAX_OPTIONS: usize = 64;

/// The radius of a radio circle for a token set.
pub(crate) fn radio_radius(t: &Tokens) -> f32 {
    (t.control_height * 0.3).round().max(3.0)
}

/// Fill, border, and the dot colour (`None` when not chosen) of a radio circle.
pub(crate) fn radio_colors(t: &Tokens, chosen: bool, hovered: bool, pressed: bool) -> (Color32, Stroke, Option<Color32>) {
    if chosen {
        let fill = if hovered || pressed { t.accent_hover } else { t.accent };
        (fill, Stroke::new(1.0, fill), Some(t.text.on_accent))
    } else {
        let fill = if pressed {
            t.surface.pressed
        } else if hovered {
            t.surface.hover
        } else {
            t.surface.sunken
        };
        (fill, Stroke::new(1.0, if hovered || pressed { t.border_strong } else { t.border }), None)
    }
}

struct Group<'a> {
    id: &'a str,
    label: &'a str,
    options: &'a [&'a str],
    disabled: &'a [usize],
}

impl Group<'_> {
    fn off(&self, i: usize) -> bool {
        self.disabled.contains(&i)
    }

    /// The next enabled option from `from`, stepping by `step` (±1) with wrap-around.
    fn step(&self, from: usize, count: usize, forward: bool) -> Option<usize> {
        (1..=count).map(|k| if forward { (from + k) % count } else { (from + count * k - k) % count }).find(|&i| !self.off(i))
    }

    fn egui_id(&self, ui: &Ui, auto: Id) -> Id {
        if self.id.is_empty() { auto } else { ui.make_persistent_id(("ashlar_ui::controls::choice", self.id)) }
    }

    fn galleys(&self, ui: &Ui, size: f32) -> Vec<Arc<Galley>> {
        self.options
            .iter()
            .take(MAX_OPTIONS)
            .enumerate()
            .map(|(i, o)| text_galley(ui, ("ashlar_ui::controls::choice", self.id, i), o, size))
            .collect()
    }

    /// Sense the options at `rects` (one Tab stop), apply clicks and keys to `selected`, and
    /// report each option's response and whether the choice changed.
    fn interact(&self, ui: &mut Ui, group: &Response, rects: &[Rect], selected: &mut usize) -> (Vec<Response>, bool) {
        let count = rects.len();
        let first = (0..count).find(|&i| !self.off(i));
        let tab_stop = if *selected < count && !self.off(*selected) { Some(*selected) } else { first };
        let enabled = ui.is_enabled();
        let child = ui.new_child(UiBuilder::new().max_rect(group.rect).accessibility_parent(group.id));
        let items: Vec<Response> = rects
            .iter()
            .enumerate()
            .map(|(i, rect)| {
                let sense = if self.off(i) {
                    Sense::hover()
                } else if Some(i) == tab_stop {
                    Sense::click()
                } else {
                    Sense::CLICK
                };
                let r = child.interact(*rect, group.id.with(i), sense);
                let label = self.options.get(i).copied().unwrap_or_default();
                let chosen = i == *selected;
                let on = enabled && !self.off(i);
                r.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, on, chosen, label));
                child.ctx().accesskit_node_builder(r.id, |node| {
                    node.set_position_in_set(i + 1);
                    node.set_size_of_set(count);
                });
                r
            })
            .collect();
        let mut changed = false;
        if let Some(i) = items.iter().position(Response::clicked)
            && *selected != i
            && !self.off(i)
        {
            *selected = i;
            changed = true;
        }
        if count > 0
            && let Some(f) = items.iter().position(Response::has_focus)
        {
            let next = ui.input_mut(|inp| {
                let mut key = |k| inp.consume_key(Modifiers::NONE, k);
                if key(Key::ArrowRight) || key(Key::ArrowDown) {
                    self.step(f, count, true)
                } else if key(Key::ArrowLeft) || key(Key::ArrowUp) {
                    self.step(f, count, false)
                } else if key(Key::Home) {
                    first
                } else if key(Key::End) {
                    (0..count).rev().find(|&i| !self.off(i))
                } else {
                    None
                }
            });
            if let Some(n) = next {
                // the arrow already asked egui to move the focus spatially: keep it in the group
                ui.memory_mut(|m| {
                    m.move_focus(FocusDirection::None);
                    m.request_focus(group.id.with(n));
                });
                if *selected != n {
                    *selected = n;
                    changed = true;
                }
            }
        }
        (items, changed)
    }

    fn register(&self, ui: &Ui, group: &Response, items: &[Response], selected: usize) {
        let value = self.options.get(selected).copied().unwrap_or_default();
        automation::register_with(ui, group, self.id, self.label, Role::RadioGroup, Details { value: Some(value), selected: None });
        if self.id.is_empty() {
            return;
        }
        for (i, r) in items.iter().enumerate() {
            let label = self.options.get(i).copied().unwrap_or_default();
            automation::register_fmt(
                ui,
                r.rect,
                format_args!("{}.option.{i}", self.id),
                label,
                Role::Radio,
                r.enabled() && !self.off(i),
                Details { value: None, selected: Some(i == selected) },
            );
        }
    }
}

/// A row of equal-width segments, one of them chosen: a compact one-of-few choice (a view mode,
/// an alignment). `selected` out of range shows none chosen until the user picks one.
pub struct Segmented<'a> {
    group: Group<'a>,
    selected: &'a mut usize,
}

impl<'a> Segmented<'a> {
    /// `label` names the whole control for screen readers and automation.
    pub fn new(id: &'a str, label: &'a str, options: &'a [&'a str], selected: &'a mut usize) -> Self {
        Segmented { group: Group { id, label, options, disabled: &[] }, selected }
    }
    /// Options (by index) that cannot be picked.
    pub fn disabled(mut self, options: &'a [usize]) -> Self {
        self.group.disabled = options;
        self
    }

    /// The group's response: `changed()` when the user picked another option.
    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let galleys = self.group.galleys(ui, t.type_scale.button);
        let count = galleys.len();
        let seg_w = galleys.iter().map(|g| g.size().x).fold(0.0, f32::max) + 2.0 * t.space.lg;
        let seg_w = seg_w.max(t.control_height);
        let (auto_id, rect) = ui.allocate_space(vec2(seg_w * count as f32, t.control_height));
        let id = self.group.egui_id(ui, auto_id);
        let mut group = ui.interact(rect, id, Sense::hover());
        let enabled = ui.is_enabled();
        group.widget_info(|| WidgetInfo::labeled(WidgetType::RadioGroup, enabled, self.group.label));
        let rects: Vec<Rect> =
            (0..count).map(|i| Rect::from_min_size(pos2(rect.left() + seg_w * i as f32, rect.top()), vec2(seg_w, rect.height()))).collect();
        let (items, changed) = self.group.interact(ui, &group, &rects, self.selected);
        if changed {
            group.mark_changed();
        }
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let sm = CornerRadius::same(t.radius.sm);
            painter.rect(rect, CornerRadius::same(t.radius.md), t.surface.sunken, Stroke::new(1.0, t.border), StrokeKind::Inside);
            for (i, (item, galley)) in items.iter().zip(galleys).enumerate() {
                let seg = item.rect.shrink(2.0);
                let chosen = i == *self.selected;
                let off = self.group.off(i);
                let painter = &super::dimmed(ui, enabled && off);
                let pressed = enabled && !off && item.is_pointer_button_down_on();
                let hovered = enabled && !off && item.hovered();
                let state = if chosen || !(pressed || hovered) {
                    Color32::TRANSPARENT
                } else if pressed {
                    t.surface.pressed
                } else {
                    t.surface.hover
                };
                let state = crate::tokens::state_color(ui.ctx(), item.id, state);
                if chosen {
                    painter.rect(seg, sm, t.surface.raised, Stroke::new(1.0, t.border), StrokeKind::Inside);
                } else if state.a() > 0 {
                    painter.rect_filled(seg, sm, state);
                }
                let text = if chosen || hovered { t.text.primary } else { t.text.secondary };
                painter.galley(egui::Align2::CENTER_CENTER.align_size_within_rect(galley.size(), item.rect).min, galley, text);
                if item.has_focus() {
                    focus_ring(painter, seg, CornerRadius::same(t.radius.sm.saturating_add(2)), &t);
                }
            }
        }
        self.group.register(ui, &group, &items, *self.selected);
        group
    }
}

/// Radio buttons, one per option, stacked (or in a row with [`RadioGroup::horizontal`]). The
/// labels are part of each option's click target.
pub struct RadioGroup<'a> {
    group: Group<'a>,
    selected: &'a mut usize,
    horizontal: bool,
}

impl<'a> RadioGroup<'a> {
    /// `label` names the whole group for screen readers and automation.
    pub fn new(id: &'a str, label: &'a str, options: &'a [&'a str], selected: &'a mut usize) -> Self {
        RadioGroup { group: Group { id, label, options, disabled: &[] }, selected, horizontal: false }
    }
    /// Options (by index) that cannot be picked.
    pub fn disabled(mut self, options: &'a [usize]) -> Self {
        self.group.disabled = options;
        self
    }
    pub fn horizontal(mut self, horizontal: bool) -> Self {
        self.horizontal = horizontal;
        self
    }

    /// The group's response: `changed()` when the user picked another option.
    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let galleys = self.group.galleys(ui, t.type_scale.body);
        let r = radio_radius(&t);
        let row_h = t.control_height;
        let widths: Vec<f32> = galleys.iter().map(|g| 2.0 * r + if g.size().x > 0.0 { t.space.sm + g.size().x } else { 0.0 }).collect();
        let size = if self.horizontal {
            let gaps = t.space.lg * widths.len().saturating_sub(1) as f32;
            vec2(widths.iter().sum::<f32>() + gaps, row_h)
        } else {
            vec2(widths.iter().copied().fold(0.0, f32::max), row_h * widths.len() as f32)
        };
        let (auto_id, rect) = ui.allocate_space(size);
        let id = self.group.egui_id(ui, auto_id);
        let mut group = ui.interact(rect, id, Sense::hover());
        let enabled = ui.is_enabled();
        group.widget_info(|| WidgetInfo::labeled(WidgetType::RadioGroup, enabled, self.group.label));
        let mut x = rect.left();
        let rects: Vec<Rect> = widths
            .iter()
            .enumerate()
            .map(|(i, w)| {
                if self.horizontal {
                    let item = Rect::from_min_size(pos2(x, rect.top()), vec2(*w, row_h));
                    x += w + t.space.lg;
                    item
                } else {
                    Rect::from_min_size(pos2(rect.left(), rect.top() + row_h * i as f32), vec2(*w, row_h))
                }
            })
            .collect();
        let (items, changed) = self.group.interact(ui, &group, &rects, self.selected);
        if changed {
            group.mark_changed();
        }
        if ui.is_rect_visible(rect) {
            for (i, (item, galley)) in items.iter().zip(galleys).enumerate() {
                let chosen = i == *self.selected;
                let off = self.group.off(i);
                let painter = &super::dimmed(ui, enabled && off);
                let pressed = enabled && !off && item.is_pointer_button_down_on();
                let hovered = enabled && !off && item.hovered();
                let center = pos2(item.rect.left() + r, item.rect.center().y);
                let (fill, stroke, dot) = radio_colors(&t, chosen, hovered, pressed);
                let fill = crate::tokens::state_color(ui.ctx(), item.id, fill);
                painter.circle(center, r, fill, stroke);
                if let Some(dot) = dot {
                    painter.circle_filled(center, (r * 0.4).round().max(1.0), dot);
                }
                painter.galley(pos2(center.x + r + t.space.sm, item.rect.center().y - galley.size().y / 2.0), galley, t.text.primary);
                if item.has_focus() {
                    painter.circle_stroke(center, r + 2.0, Stroke::new(super::FOCUS_WIDTH, t.focus));
                }
            }
        }
        self.group.register(ui, &group, &items, *self.selected);
        group
    }
}
