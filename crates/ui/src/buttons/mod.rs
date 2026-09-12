//! Icon-only buttons: the app picks the colours of each state and paints the icon.

use egui::{Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType};

/// How a button looks in one state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonVisuals {
    pub fill: Color32,
    pub stroke: Stroke,
    /// Colour handed to the icon painter.
    pub icon: Color32,
}

/// Colours and shape of an [`IconButton`] in every state.
#[derive(Clone, Copy, Debug)]
pub struct IconButtonStyle {
    pub normal: ButtonVisuals,
    pub hovered: ButtonVisuals,
    /// While the pointer is held down on the button; `None` keeps the hovered look.
    pub pressed: Option<ButtonVisuals>,
    /// When selected (a toggled tool, an active mode). Wins over every other state.
    pub selected: ButtonVisuals,
    pub corner_radius: CornerRadius,
    /// Outline drawn inside the button while it has keyboard focus.
    pub focus_stroke: Stroke,
}

/// Largest side accepted, in points.
const MAX_SIDE: f32 = 4096.0;

/// A square-ish button that shows only an icon.
///
/// Its id comes from the position in the surrounding UI (scope it with `push_id` or a child UI
/// when buttons move around); the label is only the accessible name. Clicks, keyboard activation
/// and press cancellation are egui's. The button never runs a command or toggles itself: check
/// `clicked()` on the response.
pub struct IconButton<'a> {
    label: &'a str,
    size: Vec2,
    style: &'a IconButtonStyle,
    selected: bool,
    single_shape: bool,
}

impl<'a> IconButton<'a> {
    pub fn new(label: &'a str, size: Vec2, style: &'a IconButtonStyle) -> Self {
        Self { label, size, style, selected: false, single_shape: false }
    }

    /// Draw with the selected visuals and report the toggled state to AccessKit.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Paint fill and border as one rect shape instead of a fill followed by an inside stroke.
    /// Gives the same tessellation as a single `painter.rect` call.
    pub fn single_shape_frame(mut self) -> Self {
        self.single_shape = true;
        self
    }

    pub fn show(self, ui: &mut Ui, paint_icon: impl FnOnce(&Ui, Rect, Color32)) -> Response {
        let (label, selected) = (self.label, self.selected);
        let size_ok = self.size.is_finite() && self.size.x > 0.0 && self.size.y > 0.0 && self.size.x <= MAX_SIDE && self.size.y <= MAX_SIDE;
        if !size_ok {
            let (_, response) = ui.allocate_exact_size(Vec2::ZERO, Sense::hover());
            response.widget_info(|| WidgetInfo::selected(WidgetType::Button, false, selected, label));
            return response;
        }
        let (rect, response) = ui.allocate_exact_size(self.size, Sense::click());
        let enabled = ui.is_enabled();
        response.widget_info(|| WidgetInfo::selected(WidgetType::Button, enabled, selected, label));

        let style = self.style;
        let visuals = if selected {
            style.selected
        } else if let Some(pressed) = style.pressed.filter(|_| response.is_pointer_button_down_on()) {
            pressed
        } else if response.hovered() {
            style.hovered
        } else {
            style.normal
        };

        let painter = ui.painter();
        let radius = style.corner_radius;
        let stroke = bounded(visuals.stroke, rect);
        if self.single_shape {
            painter.rect(rect, radius, visuals.fill, stroke, StrokeKind::Inside);
        } else {
            if visuals.fill != Color32::TRANSPARENT {
                painter.rect_filled(rect, radius, visuals.fill);
            }
            if !stroke.is_empty() {
                painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
            }
        }
        paint_icon(ui, rect, visuals.icon);
        if response.has_focus() && rect.width() > 4.0 && rect.height() > 4.0 {
            let inner = rect.shrink(2.0);
            let focus = bounded(style.focus_stroke, inner);
            if !focus.is_empty() {
                ui.painter().rect_stroke(inner, radius, focus, StrokeKind::Inside);
            }
        }
        response
    }
}

/// `stroke` made safe for outlining `rect`: no wider than half its smaller side, and
/// `Stroke::NONE` for a non-finite or non-positive width.
fn bounded(stroke: Stroke, rect: Rect) -> Stroke {
    if !stroke.width.is_finite() || stroke.width <= 0.0 {
        return Stroke::NONE;
    }
    Stroke::new(stroke.width.min(rect.width().min(rect.height()) / 2.0), stroke.color)
}

#[cfg(test)]
mod tests;
