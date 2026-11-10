//! Skeleton rows: grey placeholder lines where content is still loading.

use egui::{CornerRadius, Rect, Response, Sense, Ui, WidgetInfo, WidgetType, pos2, vec2};

use super::{animate, mix};
use crate::automation::{self, Details, Role};
use crate::tokens;

/// Rows beyond this many are not drawn.
pub const MAX_ROWS: usize = 256;
/// Seconds per pulse.
const PULSE_S: f64 = 1.6;
/// Widths of successive rows, as fractions of the full width (repeating), so the block reads as
/// text rather than a grid.
const WIDTHS: [f32; 5] = [1.0, 0.86, 0.94, 0.72, 0.9];

/// Placeholder lines that stand in for content still loading. They pulse gently while on screen
/// (one frame per displayed frame, through [`crate::wake`]); with
/// [`Skeleton::reduced_motion`] they hold still and ask for no frames.
///
/// Screen readers see one busy progress indicator named by [`Skeleton::label`] (default: the
/// installed [`crate::labels`] word for "Loading").
pub struct Skeleton<'a> {
    id: &'a str,
    label: Option<&'a str>,
    rows: usize,
    width: Option<f32>,
    reduced_motion: bool,
}

impl<'a> Skeleton<'a> {
    /// `rows` placeholder lines (at most [`MAX_ROWS`]).
    pub fn rows(id: &'a str, rows: usize) -> Self {
        Skeleton { id, label: None, rows: rows.min(MAX_ROWS), width: None, reduced_motion: false }
    }
    /// The accessible name (default: "Loading", localized).
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = Some(label);
        self
    }
    /// Width in points (default: the available width).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width.is_finite().then(|| width.clamp(8.0, 8000.0));
        self
    }
    /// Hold still and ask for no frames.
    pub fn reduced_motion(mut self, reduced: bool) -> Self {
        self.reduced_motion = reduced;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let row_h = t.control_height;
        let bar_h = t.type_scale.body.round();
        let w = self.width.unwrap_or_else(|| ui.available_width().clamp(8.0, 8000.0));
        let (rect, response) = ui.allocate_exact_size(vec2(w, row_h * self.rows as f32), Sense::hover());
        if self.rows > 0 && ui.is_rect_visible(rect) {
            let base = t.surface.hover;
            let fill = if self.reduced_motion {
                base
            } else {
                animate(ui, rect);
                let phase = (ui.input(|i| i.time) / PULSE_S).fract() as f32;
                // 0 → 1 → 0 over a pulse
                let k = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos();
                mix(base, t.surface.sunken, 0.6 * k)
            };
            let radius = CornerRadius::same(t.radius.sm);
            let clip = ui.clip_rect();
            for i in 0..self.rows {
                let top = rect.top() + row_h * i as f32 + (row_h - bar_h) / 2.0;
                if top > clip.bottom() {
                    break;
                }
                if top + bar_h < clip.top() {
                    continue;
                }
                let frac = WIDTHS.get(i % WIDTHS.len()).copied().unwrap_or(1.0);
                let bar = Rect::from_min_size(pos2(rect.left(), top), vec2(w * frac, bar_h));
                ui.painter().rect_filled(bar, radius, fill);
            }
        }
        let installed;
        let label = match self.label {
            Some(l) => l,
            None => {
                installed = crate::labels::current(ui.ctx());
                installed.controls.loading.as_str()
            }
        };
        response.widget_info(|| WidgetInfo::labeled(WidgetType::ProgressIndicator, true, label));
        ui.ctx().accesskit_node_builder(response.id, |node| node.set_busy());
        automation::register_with(ui, &response, self.id, label, Role::Progress, Details::default());
        response
    }
}
