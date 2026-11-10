//! Spacing helpers that take space tokens: small sugar over `ui.horizontal` and margins, sized
//! from the current theme's [`SpaceScale`](crate::tokens::SpaceScale).

use crate::tokens::{self, Space};

/// Lay out `add` left to right with `gap` between items.
pub fn row<R>(ui: &mut egui::Ui, gap: Space, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let gap = tokens::current(ui.ctx()).space.get(gap);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        add(ui)
    })
}

/// Lay out `add` top to bottom with `gap` between items.
pub fn column<R>(ui: &mut egui::Ui, gap: Space, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let gap = tokens::current(ui.ctx()).space.get(gap);
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = gap;
        add(ui)
    })
}

/// Lay out `add` inset by `space` on every side (no fill or stroke).
pub fn inset<R>(ui: &mut egui::Ui, space: Space, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let s = tokens::current(ui.ctx()).space.get(space);
    egui::Frame::NONE.inner_margin(egui::Margin::same(margin(s))).show(ui, add)
}

/// A margin in whole points (egui margins are `i8`), clamped to its range.
fn margin(points: f32) -> i8 {
    if points.is_finite() {
        // clamped first, so the conversion cannot wrap
        points.round().clamp(0.0, f32::from(i8::MAX)) as i8
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_margins_are_clamped() {
        assert_eq!(margin(f32::NAN), 0);
        assert_eq!(margin(-4.0), 0);
        assert_eq!(margin(1e9), i8::MAX);
        assert_eq!(margin(7.6), 8);
    }

    #[test]
    fn row_and_inset_use_the_theme_scale() {
        let ctx = egui::Context::default();
        let t = tokens::current(&ctx);
        let mut gap = 0.0;
        let mut inner = egui::Rect::NOTHING;
        let mut outer = egui::Rect::NOTHING;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            row(ui, Space::Lg, |ui| gap = ui.spacing().item_spacing.x);
            let r = inset(ui, Space::Md, |ui| ui.label("x").rect);
            inner = r.inner;
            outer = r.response.rect;
        });
        out.textures_delta.clear();
        assert_eq!(gap, t.space.lg);
        assert!((inner.min.x - outer.min.x - t.space.md.round()).abs() < 0.01);
    }
}
