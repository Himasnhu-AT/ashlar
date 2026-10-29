//! A grid of colour swatches (a palette, recent colours, label colours).
//!
//! The app owns the colours and the selection; the grid reports which swatch was clicked or
//! activated with Enter/Space. Arrow keys move the keyboard focus between swatches.

use egui::{Color32, Key, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};

use crate::automation::{self, Details, Role};
use crate::tokens;

/// The grid. Colours with transparency are drawn over a checkerboard.
pub struct SwatchGrid<'a> {
    id: &'a str,
    colors: &'a [Color32],
    names: &'a [&'a str],
    selected: Option<usize>,
    columns: usize,
    size: Option<f32>,
}

/// What the grid reports.
pub struct SwatchResponse {
    /// The whole grid.
    pub response: egui::Response,
    /// The swatch clicked (or activated from the keyboard) this frame.
    pub clicked: Option<usize>,
    /// The swatch under the pointer.
    pub hovered: Option<usize>,
}

/// More swatches than this are not shown (a palette is not a list).
pub const MAX_SWATCHES: usize = 4096;

impl<'a> SwatchGrid<'a> {
    pub fn new(id: &'a str, colors: &'a [Color32]) -> Self {
        SwatchGrid { id, colors, names: &[], selected: None, columns: 8, size: None }
    }
    /// Accessible names, parallel to the colours (missing names fall back to the hex value).
    pub fn names(mut self, names: &'a [&'a str]) -> Self {
        self.names = names;
        self
    }
    pub fn selected(mut self, index: Option<usize>) -> Self {
        self.selected = index;
        self
    }
    pub fn columns(mut self, columns: usize) -> Self {
        self.columns = columns.clamp(1, 64);
        self
    }
    pub fn swatch_size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }

    pub fn show(self, ui: &mut Ui) -> SwatchResponse {
        let t = tokens::current(ui.ctx());
        let colors = self.colors.get(..self.colors.len().min(MAX_SWATCHES)).unwrap_or(&[]);
        let size = self.size.filter(|s| s.is_finite() && *s > 0.0).unwrap_or(t.control_height * 0.9).clamp(6.0, 256.0);
        let gap = t.space.xs;
        let cols = self.columns.max(1);
        let rows = colors.len().div_ceil(cols);
        let total = vec2(cols.min(colors.len().max(1)) as f32 * (size + gap) - gap, rows as f32 * (size + gap) - gap).max(Vec2::ZERO);
        let (rect, grid) = ui.allocate_exact_size(total, Sense::hover());
        let base = ui.make_persistent_id(self.id);
        let mut clicked = None;
        let mut hovered = None;
        let mut move_focus: Option<usize> = None;
        let painter = ui.painter_at(rect.expand(3.0));
        let mut hex = [0u8; 9];
        for (i, &color) in colors.iter().enumerate() {
            let (r, c) = (i / cols, i % cols);
            let cell = Rect::from_min_size(rect.min + vec2(c as f32 * (size + gap), r as f32 * (size + gap)), Vec2::splat(size));
            let id = base.with(i);
            let resp = ui.interact(cell, id, Sense::click());
            if resp.clicked() {
                clicked = Some(i);
            }
            if resp.hovered() {
                hovered = Some(i);
            }
            if crate::edit::has_keyboard(&resp) {
                let (dx, dy) = ui.input(|inp| {
                    (
                        inp.key_pressed(Key::ArrowRight) as i64 - inp.key_pressed(Key::ArrowLeft) as i64,
                        inp.key_pressed(Key::ArrowDown) as i64 - inp.key_pressed(Key::ArrowUp) as i64,
                    )
                });
                let target = i as i64 + dx + dy * cols as i64;
                if (dx, dy) != (0, 0) && target >= 0 && (target as usize) < colors.len() {
                    move_focus = Some(target as usize);
                }
            }
            if color.a() < 255 {
                checker(&painter, cell, t.radius.sm);
            }
            painter.rect_filled(cell, t.radius.sm, color);
            let selected = self.selected == Some(i);
            let outline = if selected {
                Stroke::new(2.0, t.text.primary)
            } else if resp.hovered() {
                Stroke::new(1.0, t.border_strong)
            } else {
                Stroke::new(1.0, t.border)
            };
            painter.rect_stroke(cell, t.radius.sm, outline, StrokeKind::Inside);
            if selected {
                painter.rect_stroke(cell.shrink(2.0), t.radius.sm, Stroke::new(1.0, t.surface.panel), StrokeKind::Inside);
            }
            if resp.has_focus() {
                painter.rect_stroke(cell.expand(2.0), t.radius.sm, Stroke::new(1.5, t.focus), StrokeKind::Outside);
            }
            let name = match self.names.get(i) {
                Some(n) => *n,
                None => hex_name(color, &mut hex),
            };
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, resp.enabled(), selected, name));
            let id_str = self.id;
            automation::register_fmt(
                ui,
                cell,
                format_args!("{id_str}.{i}"),
                name,
                Role::Swatch,
                resp.enabled(),
                Details { value: None, selected: Some(selected) },
            );
        }
        if let Some(target) = move_focus {
            ui.memory_mut(|m| m.request_focus(base.with(target)));
        }
        SwatchResponse { response: grid, clicked, hovered }
    }
}

/// `#rrggbbaa` into a stack buffer.
fn hex_name(c: Color32, buf: &mut [u8; 9]) -> &str {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    buf[0] = b'#';
    for (k, v) in [r, g, b, a].into_iter().enumerate() {
        buf[1 + k * 2] = HEX[(v >> 4) as usize];
        buf[2 + k * 2] = HEX[(v & 15) as usize];
    }
    std::str::from_utf8(buf).unwrap_or("#")
}

fn checker(painter: &egui::Painter, rect: Rect, radius: u8) {
    painter.rect_filled(rect, radius, Color32::from_gray(230));
    let half = rect.size() / 2.0;
    for (dx, dy) in [(0.0, 0.0), (1.0, 1.0)] {
        painter.rect_filled(Rect::from_min_size(rect.min + vec2(dx * half.x, dy * half.y), half), 0, Color32::from_gray(170));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    struct State {
        selected: Option<usize>,
        clicks: Vec<usize>,
    }

    const COLORS: [Color32; 6] = [Color32::RED, Color32::GREEN, Color32::BLUE, Color32::YELLOW, Color32::TRANSPARENT, Color32::WHITE];

    #[test]
    fn click_selects_arrows_move_focus_and_ids_are_registered() {
        let mut h = Harness::builder().with_size(vec2(300.0, 120.0)).build_ui_state(
            |ui, s: &mut State| {
                let out = SwatchGrid::new("labels", &COLORS).names(&["Red", "Green"]).columns(3).selected(s.selected).show(ui);
                if let Some(i) = out.clicked {
                    s.selected = Some(i);
                    s.clicks.push(i);
                }
            },
            State { selected: None, clicks: Vec::new() },
        );
        h.step();
        let green = automation::find(&h.ctx, "labels.1").unwrap();
        assert_eq!(green.label, "Green");
        assert_eq!(automation::find(&h.ctx, "labels.4").unwrap().label, "#00000000");
        h.hover_at(green.center());
        h.step();
        h.drag_at(green.center());
        h.step();
        h.drop_at(green.center());
        h.step();
        h.step();
        assert_eq!(h.state().selected, Some(1));
        assert_eq!(automation::find(&h.ctx, "labels.1").and_then(|e| e.selected), Some(true));
        // keyboard: focus moves down one row, Enter activates
        h.get_by_label("Green").focus();
        h.step();
        h.key_press(Key::ArrowDown);
        h.step();
        h.step();
        h.key_press(Key::Enter);
        h.step();
        assert_eq!(h.state().clicks.last(), Some(&4), "{:?}", h.state().clicks);
    }

    #[test]
    fn empty_and_huge_inputs_are_bounded() {
        let many = vec![Color32::GRAY; MAX_SWATCHES + 100];
        let mut h = Harness::builder().with_size(vec2(300.0, 300.0)).build_ui(|ui| {
            SwatchGrid::new("none", &[]).show(ui);
            egui::ScrollArea::vertical().show(ui, |ui| {
                SwatchGrid::new("many", &many).columns(0).swatch_size(f32::NAN).show(ui);
            });
        });
        h.step();
        assert!(automation::query(&h.ctx, "many.").len() < 200, "only visible swatches are registered");
        let mut buf = [0u8; 9];
        assert_eq!(hex_name(Color32::from_rgb(255, 0, 16), &mut buf), "#ff0010ff");
    }
}
