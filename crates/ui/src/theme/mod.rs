//! Widget-state colouring: the low-level step [`crate::tokens`] uses to colour egui's widgets.

use egui::style::{WidgetVisuals, Widgets};
use egui::{Color32, CornerRadius, Stroke};

/// One background colour for each of egui's five widget states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WidgetPalette {
    pub noninteractive: Color32,
    pub inactive: Color32,
    pub hovered: Color32,
    pub active: Color32,
    pub open: Color32,
}

impl WidgetPalette {
    /// Give every state its background (both `bg_fill` and `weak_bg_fill`), `radius` and
    /// `foreground` stroke. Borders (`bg_stroke`), `expansion` and everything else are left as
    /// they were, so an app can set those before or after. Nothing is installed globally.
    pub fn apply_to(self, widgets: &mut Widgets, radius: CornerRadius, foreground: Stroke) {
        let paint = |state: &mut WidgetVisuals, fill: Color32| {
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.corner_radius = radius;
            state.fg_stroke = foreground;
        };
        paint(&mut widgets.noninteractive, self.noninteractive);
        paint(&mut widgets.inactive, self.inactive);
        paint(&mut widgets.hovered, self.hovered);
        paint(&mut widgets.active, self.active);
        paint(&mut widgets.open, self.open);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_radius_and_foreground_change_borders_and_expansion_do_not() {
        let mut widgets = Widgets::light();
        let border = Stroke::new(2.5, Color32::from_rgb(200, 10, 10));
        widgets.active.bg_stroke = border;
        widgets.hovered.expansion = 1.5;
        let before = widgets.clone();
        let palette = WidgetPalette {
            noninteractive: Color32::from_rgb(1, 2, 3),
            inactive: Color32::from_rgb(4, 5, 6),
            hovered: Color32::from_rgb(7, 8, 9),
            active: Color32::from_rgb(10, 11, 12),
            open: Color32::from_rgb(13, 14, 15),
        };
        let radius = CornerRadius::same(5);
        let fg = Stroke::new(1.25, Color32::from_rgb(90, 90, 90));
        palette.apply_to(&mut widgets, radius, fg);

        let states = [
            (&widgets.noninteractive, &before.noninteractive, palette.noninteractive),
            (&widgets.inactive, &before.inactive, palette.inactive),
            (&widgets.hovered, &before.hovered, palette.hovered),
            (&widgets.active, &before.active, palette.active),
            (&widgets.open, &before.open, palette.open),
        ];
        for (after, was, fill) in states {
            assert_eq!((after.bg_fill, after.weak_bg_fill), (fill, fill));
            assert_eq!((after.corner_radius, after.fg_stroke), (radius, fg));
            assert_eq!((after.bg_stroke, after.expansion), (was.bg_stroke, was.expansion));
        }
        assert_eq!(widgets.active.bg_stroke, border);
        assert_eq!(widgets.hovered.expansion, 1.5);

        widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        palette.apply_to(&mut widgets, radius, fg);
        assert_eq!(widgets.inactive.weak_bg_fill, palette.inactive);
        assert_eq!(widgets.active.bg_stroke, border);
    }
}
