//! The divider widget between two panes.

use std::ops::RangeInclusive;

use egui::accesskit::{Action, ActionData, Orientation, Role};
use egui::{Color32, CursorIcon, EventFilter, Id, Key, Modifiers, Rect, Response, Sense, Ui, Vec2, WidgetInfo, WidgetType};

use super::SplitAxis;

/// Fill colours of a divider in its three highlighted states (nothing is drawn otherwise).
#[derive(Clone, Copy, Debug)]
pub struct ResizeHandleStyle {
    pub hovered: Color32,
    pub dragged: Color32,
    pub focused: Color32,
}

/// What a [`ResizeHandle`] reported this frame.
pub struct ResizeResponse {
    pub response: Response,
    /// How far the user asked to move the divider this frame, along its axis (points).
    pub delta: f32,
}

/// A focusable, accessible divider that reports movement and never stores a size.
///
/// The pointer drags it, the arrow keys of its own axis move it by the keyboard step while it is
/// focused, and AccessKit increment/decrement (and set-value, once [`Self::value`] is given) work
/// as well. Apply the returned delta with [`super::SplitLayout::resized`] or your own model.
pub struct ResizeHandle<'a> {
    id: Id,
    axis: SplitAxis,
    rect: Rect,
    label: &'a str,
    style: ResizeHandleStyle,
    margin: f32,
    step: f32,
    value: Option<(f32, RangeInclusive<f32>)>,
}

/// Widest grab margin accepted on each side of the visible divider.
const MAX_MARGIN: f32 = 4096.0;

impl<'a> ResizeHandle<'a> {
    /// A divider at `rect` that moves along `axis`. `label` is for assistive technology only;
    /// it never changes the widget's identity.
    pub fn new(id: Id, axis: SplitAxis, rect: Rect, label: &'a str, style: ResizeHandleStyle) -> Self {
        Self { id, axis, rect, label, style, margin: 3.0, step: 8.0, value: None }
    }

    /// Extra grab area on both sides along the movement axis (default 3 points).
    pub fn hit_margin(mut self, margin: f32) -> Self {
        self.margin = margin;
        self
    }

    /// Distance one arrow press or AccessKit step moves the divider (default 8 points).
    pub fn keyboard_step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    /// The divider's current position within `range`, reported to AccessKit as a slider value
    /// and used to turn a set-value request into a delta.
    pub fn value(mut self, value: f32, range: RangeInclusive<f32>) -> Self {
        self.value = Some((value, range));
        self
    }

    fn hit_rect(&self) -> Rect {
        self.rect.expand2(self.axis.vec(self.margin))
    }

    fn valid(&self) -> bool {
        let value_ok = match &self.value {
            None => true,
            Some((v, range)) => v.is_finite() && range.start().is_finite() && range.end().is_finite() && range.start() <= range.end(),
        };
        super::usable(self.rect)
            && self.margin.is_finite()
            && (0.0..=MAX_MARGIN).contains(&self.margin)
            && super::usable(self.hit_rect())
            && self.step.is_finite()
            && self.step > 0.0
            && value_ok
    }

    pub fn show(self, ui: &mut Ui) -> ResizeResponse {
        let valid = self.valid();
        if !valid {
            let response = ui.interact(Rect::from_min_size(ui.min_rect().min, Vec2::ZERO), self.id, Sense::hover());
            response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, false, self.label));
            ui.ctx().accesskit_node_builder(self.id, |node| node.set_role(Role::Splitter));
            return ResizeResponse { response, delta: 0.0 };
        }

        let mut response = ui.interact(self.hit_rect(), self.id, Sense::drag());
        let enabled = response.enabled();
        let mut delta = 0.0f32;
        if enabled {
            if response.drag_started() {
                response.request_focus();
            }
            if response.dragged() {
                delta += self.axis.along(response.drag_delta());
            }
            if response.has_focus() {
                delta += self.keyboard(ui);
            }
            delta += self.accesskit(ui);
        }
        if !delta.is_finite() {
            delta = 0.0;
        }
        if delta != 0.0 {
            response.mark_changed();
        }

        let label = self.label;
        match &self.value {
            Some((v, _)) => response.widget_info(|| WidgetInfo::slider(enabled, f64::from(*v), label)),
            None => response.widget_info(|| WidgetInfo::labeled(WidgetType::Other, enabled, label)),
        }
        ui.ctx().accesskit_node_builder(self.id, |node| {
            node.set_role(Role::Splitter);
            // The divider line runs across the movement axis.
            node.set_orientation(match self.axis {
                SplitAxis::Horizontal => Orientation::Vertical,
                SplitAxis::Vertical => Orientation::Horizontal,
            });
            if enabled {
                node.add_action(Action::Increment);
                node.add_action(Action::Decrement);
                if let Some((v, range)) = &self.value {
                    node.set_numeric_value(f64::from(*v));
                    node.set_min_numeric_value(f64::from(*range.start()));
                    node.set_max_numeric_value(f64::from(*range.end()));
                    node.set_numeric_value_step(f64::from(self.step));
                    node.add_action(Action::SetValue);
                }
            }
        });

        if enabled {
            let fill = if response.dragged() {
                Some(self.style.dragged)
            } else if response.has_focus() {
                Some(self.style.focused)
            } else if response.hovered() {
                Some(self.style.hovered)
            } else {
                None
            };
            if let Some(fill) = fill {
                ui.painter().rect_filled(self.rect, 0.0, fill);
            }
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(match self.axis {
                    SplitAxis::Horizontal => CursorIcon::ResizeHorizontal,
                    SplitAxis::Vertical => CursorIcon::ResizeVertical,
                });
            }
        }
        ResizeResponse { response, delta }
    }

    /// Arrow keys of this axis, while focused. Tab still moves focus on.
    fn keyboard(&self, ui: &Ui) -> f32 {
        let horizontal = self.axis == SplitAxis::Horizontal;
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(self.id, EventFilter { horizontal_arrows: horizontal, vertical_arrows: !horizontal, ..Default::default() });
        });
        let (back, forward) = if horizontal { (Key::ArrowLeft, Key::ArrowRight) } else { (Key::ArrowUp, Key::ArrowDown) };
        ui.input_mut(|i| {
            let mut delta = 0.0;
            while i.consume_key(Modifiers::NONE, back) {
                delta -= self.step;
            }
            while i.consume_key(Modifiers::NONE, forward) {
                delta += self.step;
            }
            delta
        })
    }

    /// AccessKit increment, decrement and (with a value) set-value requests aimed at this widget.
    fn accesskit(&self, ui: &Ui) -> f32 {
        let mut delta = 0.0;
        ui.input_mut(|i| {
            i.consume_accesskit_action_requests(self.id, |request| match request.action {
                Action::Increment => {
                    delta += self.step;
                    true
                }
                Action::Decrement => {
                    delta -= self.step;
                    true
                }
                Action::SetValue => match (&self.value, &request.data) {
                    (Some((v, range)), Some(ActionData::NumericValue(n))) if n.is_finite() => {
                        // `valid()` checked that both ends are finite and ordered.
                        let wanted = (*n).clamp(f64::from(*range.start()), f64::from(*range.end()));
                        delta += (wanted - f64::from(*v)) as f32;
                        true
                    }
                    _ => false,
                },
                _ => false,
            });
        });
        delta
    }
}
