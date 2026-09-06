//! Status bar: a thin strip along the bottom of the window with left and right sections.
//!
//! ```
//! # use ashlar_ui::status_bar::{self, StatusBar};
//! # fn ui(ui: &mut egui::Ui) {
//! StatusBar::new("status").show(ui, |bar| {
//!     status_bar::text(bar, "status.doc", "3 layers");
//! }, |bar| {
//!     status_bar::progress(bar, "status.export", "Exporting", Some(0.4));
//!     if status_bar::button(bar, "status.zoom", "100 %").clicked() { /* open zoom menu */ }
//! });
//! # }
//! ```

use egui::{Align, Layout, Rect, Sense, Stroke, Ui, pos2, text::LayoutJob, vec2};

use crate::automation::{self, Details, Role};
use crate::cache;
use crate::tokens::{self, Tokens};

/// The bar. Shown as a bottom panel of the given `ui` (usually the root ui).
pub struct StatusBar<'a> {
    id: &'a str,
}

impl<'a> StatusBar<'a> {
    pub fn new(id: &'a str) -> Self {
        StatusBar { id }
    }

    /// Show the bar; `left` and `right` add items to the two sections (right-to-left order for
    /// the right section, so the first item added sits at the far right).
    pub fn show(self, ui: &mut Ui, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) -> egui::Response {
        let t = tokens::current(ui.ctx());
        let height = (t.control_height * 0.9).round();
        let frame = egui::Frame::new().fill(t.surface.panel).inner_margin(egui::Margin::symmetric(t.space.md as i8, 0));
        let inner = egui::Panel::bottom(egui::Id::new(("ashlar_ui::status_bar", self.id)))
            .exact_size(height)
            .resizable(false)
            .show_separator_line(false)
            .frame(frame)
            .show(ui, |ui| {
                let rect = ui.max_rect();
                ui.painter().hline(rect.x_range().expand(t.space.md), rect.top(), Stroke::new(1.0, t.separator));
                ui.spacing_mut().item_spacing.x = t.space.lg;
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    left(ui);
                    ui.with_layout(Layout::right_to_left(Align::Center), right);
                });
            });
        automation::register(ui, &inner.response, self.id, "Status bar", Role::Status);
        inner.response
    }
}

fn label_galley(ui: &Ui, id: &str, text: &str, color: egui::Color32, t: &Tokens) -> std::sync::Arc<egui::Galley> {
    let font = Tokens::font(t.type_scale.small);
    cache::galley(ui, ("ashlar_ui::status", id), cache::hash_of(&(text, color, font.size.to_bits())), || {
        LayoutJob::simple_singleline(text.to_owned(), font, color)
    })
}

/// A passive text item.
pub fn text(ui: &mut Ui, id: &str, text: &str) -> egui::Response {
    let t = tokens::current(ui.ctx());
    let galley = label_galley(ui, id, text, t.text.secondary, &t);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, t.text.secondary);
    automation::register_with(ui, &resp, id, text, Role::Status, Details { value: Some(text), selected: None });
    resp
}

/// A clickable item (opens a menu, toggles a mode).
pub fn button(ui: &mut Ui, id: &str, text: &str) -> egui::Response {
    let t = tokens::current(ui.ctx());
    let galley = label_galley(ui, id, text, t.text.secondary, &t);
    let size = galley.size() + vec2(2.0 * t.space.sm, 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let color = if resp.hovered() { t.text.primary } else { t.text.secondary };
    if resp.hovered() {
        ui.painter().rect_filled(rect, t.radius.sm, t.surface.hover);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(rect, t.radius.sm, Stroke::new(1.5, t.focus), egui::StrokeKind::Inside);
    }
    ui.painter().galley(egui::Align2::CENTER_CENTER.align_size_within_rect(galley.size(), rect).min, galley, color);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, text));
    automation::register(ui, &resp, id, text, Role::Button);
    resp
}

/// A progress item: label plus a bar (`None` fraction: indeterminate, animated).
pub fn progress(ui: &mut Ui, id: &str, label: &str, fraction: Option<f32>) -> egui::Response {
    let t = tokens::current(ui.ctx());
    let bar_w = t.control_height * 4.0;
    let galley = label_galley(ui, id, label, t.text.secondary, &t);
    let size = vec2(bar_w + t.space.sm + galley.size().x, galley.size().y.max(6.0));
    let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
    // right-to-left sections: bar on the right of its label either way
    let bar = Rect::from_min_size(pos2(rect.right() - bar_w, rect.center().y - 3.0), vec2(bar_w, 6.0));
    ui.painter().galley(pos2(rect.left(), rect.center().y - galley.size().y / 2.0), galley, t.text.secondary);
    ui.painter().rect_filled(bar, 3.0, t.surface.sunken);
    let mut value_buf = [0u8; 4];
    let value = match fraction.filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0)) {
        Some(f) => {
            let filled = Rect::from_min_size(bar.min, vec2(bar.width() * f, bar.height()));
            ui.painter().rect_filled(filled, 3.0, t.accent);
            percent(f, &mut value_buf)
        }
        None => {
            // indeterminate: a segment sweeping across; only this item asks for repaints
            let time = ui.input(|i| i.time);
            let phase = (time * 0.8).fract() as f32;
            let seg = bar.width() * 0.3;
            let x = bar.left() - seg + (bar.width() + seg) * phase;
            let s = Rect::from_min_max(pos2(x.max(bar.left()), bar.top()), pos2((x + seg).min(bar.right()), bar.bottom()));
            if s.width() > 0.0 {
                ui.painter().rect_filled(s, 3.0, t.accent);
            }
            crate::wake::wake_after(ui.ctx(), crate::wake::Reason::ANIMATION, std::time::Duration::from_millis(33));
            ""
        }
    };
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ProgressIndicator, true, label));
    automation::register_with(ui, &resp, id, label, Role::Status, Details { value: Some(value), selected: None });
    resp
}

/// `0.42` → `"42%"` without allocating.
fn percent(f: f32, buf: &mut [u8; 4]) -> &str {
    let n = (f * 100.0).round().clamp(0.0, 100.0) as u32;
    let mut len = 0;
    let digits = [n / 100, (n / 10) % 10, n % 10];
    let mut started = false;
    for (i, d) in digits.into_iter().enumerate() {
        if d != 0 || started || i == 2 {
            started = true;
            buf[len] = b'0' + d as u8;
            len += 1;
        }
    }
    buf[len] = b'%';
    std::str::from_utf8(&buf[..=len]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    #[test]
    fn sections_register_their_items_and_buttons_click() {
        let mut h = Harness::builder().with_size(vec2(700.0, 300.0)).build_ui_state(
            |ui, clicks: &mut u32| {
                StatusBar::new("status").show(
                    ui,
                    |bar| {
                        text(bar, "status.doc", "12 items");
                    },
                    |bar| {
                        progress(bar, "status.export", "Exporting", Some(0.42));
                        if button(bar, "status.zoom", "100 %").clicked() {
                            *clicks += 1;
                        }
                    },
                );
            },
            0,
        );
        automation::install(&h.ctx);
        h.step();
        h.step();
        let screen = h.ctx.content_rect();
        let bar = automation::find(&h.ctx, "status").unwrap();
        assert!((bar.egui_rect().bottom() - screen.bottom()).abs() <= 8.0, "docked at the bottom: {bar:?} in {screen:?}");
        assert_eq!(automation::find(&h.ctx, "status.export").map(|e| e.value), Some("42%".into()));
        let doc = automation::find(&h.ctx, "status.doc").unwrap();
        let zoom = automation::find(&h.ctx, "status.zoom").unwrap();
        assert!(doc.egui_rect().left() < zoom.egui_rect().left());
        h.hover_at(zoom.center());
        h.step();
        h.drag_at(zoom.center());
        h.step();
        h.drop_at(zoom.center());
        h.step();
        assert_eq!(*h.state(), 1);
    }

    #[test]
    fn percent_text() {
        let mut b = [0u8; 4];
        assert_eq!(percent(0.0, &mut b), "0%");
        assert_eq!(percent(0.071, &mut b), "7%");
        assert_eq!(percent(1.0, &mut b), "100%");
        assert_eq!(percent(7.0, &mut b), "100%");
    }
}
