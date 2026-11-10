//! Everyday controls drawn from the design [`tokens`](crate::tokens): text buttons, a switch, a
//! segmented control and radio group, badges and keycaps, progress bars, circles and spinners.
//!
//! Every control:
//! - takes its state by reference (`&mut bool`, `&mut usize`) or as a value and reports what the
//!   user did through the returned [`egui::Response`] (`clicked()`, `changed()`);
//! - registers a dotted [`automation`](crate::automation) id with a role, and fills the AccessKit
//!   node (role, name, toggled, selected, value, busy);
//! - works from the keyboard: Tab reaches it, Space and Enter activate it, and the option groups
//!   ([`Segmented`], [`RadioGroup`]) are one Tab stop whose options move with the arrow keys,
//!   Home and End;
//! - takes every visible string from the caller, so an app localises them in one place.
//!
//! Disabled: put the control in a disabled `Ui` (`ui.add_enabled_ui(false, …)`); [`Button`] also
//! has [`Button::enabled`].
//!
//! Motion: indeterminate progress and spinners ask for frames through [`crate::wake`] with
//! [`Reason::ANIMATION`](crate::wake::Reason::ANIMATION) only while they are on screen, one per
//! displayed frame, and none once they are gone. Each animated control takes `reduced_motion(true)`,
//! which holds a static pose and asks for no frames: the app passes the operating system's
//! reduced-motion preference there. The switch knob slides with egui's animation time and snaps
//! under reduced motion.
//!
//! ```
//! # use ashlar_ui::controls::{Button, Switch, Variant};
//! # fn ui(ui: &mut egui::Ui, sync: &mut bool, busy: bool) {
//! if Button::new("export.run", "Export").variant(Variant::Primary).loading(busy).show(ui).clicked() {
//!     // start the export
//! }
//! if Switch::new("settings.sync", "Sync", sync).show(ui).changed() {
//!     // save the setting
//! }
//! # }
//! ```

mod badge;
mod button;
mod choice;
mod progress;
mod switch;

pub use badge::{Badge, Keycap, Tone};
pub use button::{Button, IconPainter, Size, Variant};
pub use choice::{RadioGroup, Segmented};
pub use progress::{Progress, Spinner};
pub use switch::Switch;

use std::hash::Hash;
use std::sync::Arc;

use egui::{Color32, CornerRadius, Galley, Painter, Pos2, Rect, Stroke, StrokeKind, Ui};

use crate::tokens::Tokens;
use crate::wake::{self, Reason};

/// Width of the keyboard focus ring drawn around a focused control.
pub(crate) const FOCUS_WIDTH: f32 = 1.5;

/// A one-line label galley from the shared cache (laid out again only when the text or size
/// changes). Painted with the colour passed to `Painter::galley`.
pub(crate) fn text_galley(ui: &Ui, key: impl Hash, text: &str, size: f32) -> Arc<Galley> {
    let font = Tokens::font(size);
    crate::cache::galley(ui, key, crate::cache::hash_of(&(text, size.to_bits())), || {
        egui::text::LayoutJob::simple_singleline(text.to_owned(), font, Color32::PLACEHOLDER)
    })
}

/// The focus ring: a stroke just outside `rect`.
pub(crate) fn focus_ring(painter: &Painter, rect: Rect, radius: CornerRadius, t: &Tokens) {
    painter.rect_stroke(rect.expand(2.0), radius, Stroke::new(FOCUS_WIDTH, t.focus), StrokeKind::Outside);
}

/// `a` moved towards `b` by `t` (0–1) per channel.
pub(crate) fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// A painter that draws at the disabled opacity when `dim` is set (for a control disabled on its
/// own inside an enabled `Ui`; a disabled `Ui` already dims its painter).
pub(crate) fn dimmed(ui: &Ui, dim: bool) -> Painter {
    let mut painter = ui.painter().clone();
    if dim {
        painter.multiply_opacity(ui.visuals().disabled_alpha());
    }
    painter
}

/// Ask for the next frame of an animation drawn in `rect`, only while it is on screen. The host
/// presents at most one frame per display refresh, so this runs at the display rate.
pub(crate) fn animate(ui: &Ui, rect: Rect) {
    if ui.is_rect_visible(rect) {
        wake::wake(ui.ctx(), Reason::ANIMATION);
    }
}

/// Points of an arc around `center` (angles in radians, 0 = 3 o'clock, clockwise on screen).
pub(crate) fn arc_points(center: Pos2, radius: f32, start: f32, sweep: f32) -> Vec<Pos2> {
    let sweep = if sweep.is_finite() { sweep.clamp(-std::f32::consts::TAU, std::f32::consts::TAU) } else { 0.0 };
    let radius = if radius.is_finite() { radius.max(0.0) } else { 0.0 };
    // about one point per 4 degrees, at least two
    let n = ((sweep.abs() / std::f32::consts::TAU) * 90.0).ceil().clamp(1.0, 90.0) as usize;
    (0..=n)
        .map(|i| {
            let a = start + sweep * (i as f32 / n as f32);
            Pos2::new(center.x + radius * a.cos(), center.y + radius * a.sin())
        })
        .collect()
}

#[cfg(test)]
mod tests;
