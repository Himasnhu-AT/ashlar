//! Progress bars, progress circles and spinners.

use std::f32::consts::{FRAC_PI_2, TAU};

use egui::{Color32, CornerRadius, Painter, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, vec2};

use super::{animate, arc_points};
use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};

/// Seconds per sweep of an indeterminate bar.
const SWEEP_S: f64 = 1.4;
/// Seconds per turn of a spinner.
const TURN_S: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Bar,
    Circle,
}

/// A determinate (`Some(fraction)`) or indeterminate (`None`) progress indicator.
///
/// Indeterminate ones animate while on screen (one frame per displayed frame, through
/// [`crate::wake`]); with [`Progress::reduced_motion`] they hold a static pose and ask for no
/// frames. A fraction outside 0–1 is clamped; a non-finite one counts as indeterminate.
pub struct Progress<'a> {
    id: &'a str,
    label: &'a str,
    fraction: Option<f32>,
    kind: Kind,
    width: Option<f32>,
    size: Option<f32>,
    reduced_motion: bool,
}

impl<'a> Progress<'a> {
    /// A horizontal bar. `label` is the accessible name.
    pub fn bar(id: &'a str, label: &'a str, fraction: Option<f32>) -> Self {
        Progress { id, label, fraction: clean(fraction), kind: Kind::Bar, width: None, size: None, reduced_motion: false }
    }
    /// A ring that fills clockwise from the top. `label` is the accessible name.
    pub fn circle(id: &'a str, label: &'a str, fraction: Option<f32>) -> Self {
        Progress { kind: Kind::Circle, ..Self::bar(id, label, fraction) }
    }
    /// Width of a bar in points (default: the available width, at most 8 control heights).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width.is_finite().then(|| width.clamp(4.0, 4000.0));
        self
    }
    /// Diameter of a circle in points (default: the control height).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size.is_finite().then(|| size.clamp(4.0, 512.0));
        self
    }
    /// Hold an indeterminate indicator still and ask for no frames.
    pub fn reduced_motion(mut self, reduced: bool) -> Self {
        self.reduced_motion = reduced;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let (rect, response) = match self.kind {
            Kind::Bar => {
                let w = self.width.unwrap_or_else(|| ui.available_width().clamp(4.0, t.control_height * 8.0));
                ui.allocate_exact_size(vec2(w, bar_height(&t)), Sense::hover())
            }
            Kind::Circle => {
                let d = self.size.unwrap_or(t.control_height);
                ui.allocate_exact_size(vec2(d, d), Sense::hover())
            }
        };
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            match self.kind {
                Kind::Bar => paint_bar(ui, painter, rect, &t, self.fraction, self.reduced_motion),
                Kind::Circle => paint_circle(ui, painter, rect, &t, self.fraction, self.reduced_motion),
            }
        }
        accessible(ui, &response, self.label, self.fraction);
        let mut buf = [0u8; 4];
        let value = self.fraction.map_or("", |f| percent(f, &mut buf));
        automation::register_with(ui, &response, self.id, self.label, Role::Progress, Details { value: Some(value), selected: None });
        response
    }
}

fn clean(fraction: Option<f32>) -> Option<f32> {
    fraction.filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0))
}

/// Height of a progress bar.
pub(crate) fn bar_height(t: &Tokens) -> f32 {
    t.space.sm.max(2.0)
}

fn accessible(ui: &Ui, response: &Response, label: &str, fraction: Option<f32>) {
    response.widget_info(|| WidgetInfo::labeled(WidgetType::ProgressIndicator, true, label));
    ui.ctx().accesskit_node_builder(response.id, |node| match fraction {
        Some(f) => {
            node.set_min_numeric_value(0.0);
            node.set_max_numeric_value(100.0);
            // to a tenth of a percent: an f32 fraction is not exact in f64
            node.set_numeric_value((f64::from(f) * 1000.0).round() / 10.0);
        }
        None => node.set_busy(),
    });
}

/// Where an indeterminate bar's segment is: `(start, end)` as fractions of the track.
pub(crate) fn sweep(phase: f32) -> (f32, f32) {
    let seg = 0.3;
    let start = -seg + (1.0 + seg) * phase;
    (start.max(0.0), (start + seg).min(1.0))
}

fn paint_bar(ui: &Ui, painter: &Painter, rect: Rect, t: &Tokens, fraction: Option<f32>, reduced: bool) {
    let radius = CornerRadius::same((rect.height() / 2.0).round().clamp(0.0, 255.0) as u8);
    painter.rect(rect, radius, t.surface.sunken, Stroke::new(1.0, t.border), StrokeKind::Inside);
    let (a, b) = match fraction {
        Some(f) => (0.0, f),
        None if reduced => (0.35, 0.65),
        None => {
            animate(ui, rect);
            let phase = (ui.input(|i| i.time) / SWEEP_S).fract() as f32;
            sweep(phase)
        }
    };
    if b > a {
        let x = |f: f32| rect.left() + rect.width() * f;
        painter.rect_filled(Rect::from_x_y_ranges(x(a)..=x(b), rect.y_range()), radius, t.accent);
    }
}

fn ring_width(diameter: f32) -> f32 {
    (diameter * 0.125).clamp(1.5, 6.0)
}

fn paint_circle(ui: &Ui, painter: &Painter, rect: Rect, t: &Tokens, fraction: Option<f32>, reduced: bool) {
    let d = rect.width().min(rect.height());
    let w = ring_width(d);
    let r = (d - w) / 2.0;
    painter.circle_stroke(rect.center(), r, Stroke::new(w, t.surface.hover));
    match fraction {
        Some(f) if f > 0.0 => {
            painter.add(Shape::line(arc_points(rect.center(), r, -FRAC_PI_2, TAU * f), Stroke::new(w, t.accent)));
        }
        Some(_) => {}
        None => spinner(ui, painter, rect, t.accent, reduced),
    }
}

/// Paint a spinner (a three-quarter arc turning once a second) in `rect`, asking for the next
/// frame while it is on screen; under reduced motion it holds still, open at the top.
pub(crate) fn spinner(ui: &Ui, painter: &Painter, rect: Rect, color: Color32, reduced: bool) {
    let d = rect.width().min(rect.height());
    if !(d.is_finite() && d > 0.0) {
        return;
    }
    let w = ring_width(d);
    let r = (d - w) / 2.0;
    let start = if reduced {
        -FRAC_PI_2
    } else {
        animate(ui, rect);
        -FRAC_PI_2 + TAU * (ui.input(|i| i.time) / TURN_S).fract() as f32
    };
    painter.add(Shape::line(arc_points(rect.center(), r, start, TAU * 0.75), Stroke::new(w, color)));
}

/// A spinner on its own: something is happening and there is no fraction to show.
pub struct Spinner<'a> {
    id: &'a str,
    label: &'a str,
    size: Option<f32>,
    color: Option<Color32>,
    reduced_motion: bool,
}

impl<'a> Spinner<'a> {
    /// `label` is the accessible name ("Loading").
    pub fn new(id: &'a str, label: &'a str) -> Self {
        Spinner { id, label, size: None, color: None, reduced_motion: false }
    }
    /// Diameter in points (default: the body text size).
    pub fn size(mut self, size: f32) -> Self {
        self.size = size.is_finite().then(|| size.clamp(4.0, 512.0));
        self
    }
    /// Arc colour (default: the secondary text colour).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }
    /// Hold still and ask for no frames.
    pub fn reduced_motion(mut self, reduced: bool) -> Self {
        self.reduced_motion = reduced;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let d = self.size.unwrap_or(t.type_scale.body.round() + 3.0);
        let (rect, response) = ui.allocate_exact_size(vec2(d, d), Sense::hover());
        if ui.is_rect_visible(rect) {
            spinner(ui, ui.painter(), rect, self.color.unwrap_or(t.text.secondary), self.reduced_motion);
        }
        accessible(ui, &response, self.label, None);
        automation::register_with(ui, &response, self.id, self.label, Role::Progress, Details::default());
        response
    }
}

/// `0.42` → `"42%"` without allocating.
fn percent(f: f32, buf: &mut [u8; 4]) -> &str {
    let n = (f * 100.0).round().clamp(0.0, 100.0) as u32;
    let mut len = 0;
    for (i, d) in [n / 100, (n / 10) % 10, n % 10].into_iter().enumerate() {
        if d > 0 || len > 0 || i == 2 {
            if let Some(b) = buf.get_mut(len) {
                *b = b'0' + d as u8;
            }
            len += 1;
        }
    }
    if let Some(b) = buf.get_mut(len) {
        *b = b'%';
    }
    std::str::from_utf8(buf.get(..len + 1).unwrap_or_default()).unwrap_or_default()
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn percent_and_sweep() {
        let mut b = [0u8; 4];
        assert_eq!(percent(0.0, &mut b), "0%");
        assert_eq!(percent(0.42, &mut b), "42%");
        assert_eq!(percent(1.0, &mut b), "100%");
        assert_eq!(percent(0.05, &mut b), "5%");
        assert_eq!(sweep(0.0), (0.0, 0.0));
        let (a, b) = sweep(1.0);
        assert!(b - a < 1e-3, "nothing shown at the end of a sweep");
        let (a, b) = sweep(0.5);
        assert!(a > 0.0 && b < 1.0 && b > a);
        assert_eq!(clean(Some(f32::NAN)), None);
        assert_eq!(clean(Some(4.0)), Some(1.0));
        let pos2 = egui::pos2;
        let pts = arc_points(pos2(0.0, 0.0), 10.0, 0.0, f32::NAN);
        assert_eq!(pts.len(), 2);
        assert!(arc_points(pos2(0.0, 0.0), 10.0, 0.0, TAU * 10.0).len() <= 91);
    }
}
