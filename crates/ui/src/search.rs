//! Search field: a single-line editor with a search glyph, a clear button and Escape to clear.
//!
//! Escape clears a non-empty query (and keeps the focus); a second Escape gives the focus up.
//! Enter reports `submitted`. [`SearchField::focus`] moves the keyboard focus to it (bind it to
//! the app's Find shortcut).
//!
//! The look follows the tokens; an app with its own house style can set the height, fully rounded
//! ends, the editor's margins and paint the search glyph from its own icon set.

use egui::{Color32, Key, Margin, Painter, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};

/// What the field reports.
pub struct SearchResponse {
    /// The text editor's response.
    pub response: egui::Response,
    pub changed: bool,
    /// Enter pressed in the field.
    pub submitted: bool,
    /// The query was cleared (clear button or Escape).
    pub cleared: bool,
}

/// The field. See the [module docs](self).
pub struct SearchField<'a> {
    id: &'a str,
    text: &'a mut String,
    hint: &'a str,
    width: Option<f32>,
    max_len: usize,
    height: Option<f32>,
    rounded: bool,
    text_margin: Option<Margin>,
    icon: Option<&'a dyn Fn(&Painter, Rect, Color32)>,
}

impl<'a> SearchField<'a> {
    pub fn new(id: &'a str, text: &'a mut String) -> Self {
        SearchField { id, text, hint: "Search", width: None, max_len: 512, height: None, rounded: false, text_margin: None, icon: None }
    }
    /// Placeholder shown while empty (localized by the app).
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }
    /// Longest accepted query in characters.
    pub fn max_len(mut self, n: usize) -> Self {
        self.max_len = n.clamp(1, 65_536);
        self
    }

    /// Height in points (default: the tokens' control height), 12–96.
    pub fn height(mut self, height: f32) -> Self {
        self.height = height.is_finite().then(|| height.clamp(12.0, 96.0));
        self
    }
    /// Fully rounded (pill) ends instead of the tokens' control radius.
    pub fn rounded(mut self, rounded: bool) -> Self {
        self.rounded = rounded;
        self
    }
    /// Margins of the text inside the area right of the glyph (default: none at the sides,
    /// vertically centred).
    pub fn text_margin(mut self, margin: Margin) -> Self {
        self.text_margin = Some(margin);
        self
    }
    /// Paint the search glyph with the app's own icon: called with the glyph's square cell (the
    /// left end of the field) and the colour to use.
    pub fn icon(mut self, paint: &'a dyn Fn(&Painter, Rect, Color32)) -> Self {
        self.icon = Some(paint);
        self
    }

    /// Request keyboard focus for the field `id` in `ui`'s scope (takes effect next frame).
    pub fn focus(ui: &Ui, id: &str) {
        let text_id = ui.make_persistent_id(id).with("text");
        ui.memory_mut(|m| m.request_focus(text_id));
    }

    pub fn show(self, ui: &mut Ui) -> SearchResponse {
        let t = tokens::current(ui.ctx());
        let id = ui.make_persistent_id(self.id);
        let width = self.width.filter(|w| w.is_finite() && *w > 0.0).unwrap_or_else(|| ui.available_width()).clamp(60.0, 4096.0);
        let h = self.height.unwrap_or(t.control_height);
        let (rect, _) = ui.allocate_exact_size(vec2(width, h), Sense::hover());
        let icon_w = h;
        let clear_w = if self.text.is_empty() { 0.0 } else { h };
        let edit_rect = Rect::from_min_max(pos2(rect.left() + icon_w, rect.top()), pos2(rect.right() - clear_w, rect.bottom()));

        // background first so the editor paints over it
        let bg_idx = ui.painter().add(egui::Shape::Noop);
        let before = self.text.len();
        let font = Tokens::font(t.type_scale.body);
        let row_h = ui.fonts_mut(|f| f.row_height(&font));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(edit_rect).id_salt("ashlar_search"));
        let response = child.add(
            egui::TextEdit::singleline(self.text)
                .id(id.with("text"))
                .hint_text(self.hint)
                .char_limit(self.max_len)
                .desired_width(edit_rect.width())
                .font(font)
                // egui ignores `margin` once a frame is set: the margin goes on the frame
                .frame(egui::Frame::NONE.inner_margin(self.text_margin.unwrap_or(Margin::symmetric(0, ((h - row_h) / 2.0).max(0.0).floor() as i8)))),
        );
        let mut changed = response.changed() || self.text.len() != before;
        let mut cleared = false;
        let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if response.has_focus() || response.lost_focus() {
            let escape = ui.input(|i| i.key_pressed(Key::Escape));
            if escape && !self.text.is_empty() {
                self.text.clear();
                changed = true;
                cleared = true;
                // keep typing: Escape only cleared the query
                response.request_focus();
            }
        }
        if clear_w > 0.0 {
            let clear_rect = Rect::from_center_size(pos2(rect.right() - clear_w / 2.0, rect.center().y), Vec2::splat(h * 0.7));
            let clear = ui.interact(clear_rect, id.with("clear"), Sense::click());
            let col = if clear.hovered() { t.text.primary } else { t.text.secondary };
            if clear.hovered() {
                ui.painter().rect_filled(clear_rect, t.radius.sm, t.surface.hover);
            }
            let c = clear_rect.center();
            let k = h * 0.14;
            ui.painter().line_segment([c - vec2(k, k), c + vec2(k, k)], Stroke::new(1.5, col));
            ui.painter().line_segment([c + vec2(-k, k), c + vec2(k, -k)], Stroke::new(1.5, col));
            clear.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Clear search"));
            let sid = self.id;
            automation::register_fmt(ui, clear_rect, format_args!("{sid}.clear"), "Clear search", Role::Button, true, Details::default());
            if clear.clicked() {
                self.text.clear();
                changed = true;
                cleared = true;
                response.request_focus();
            }
        }
        let focused = response.has_focus();
        let border = if focused { Stroke::new(1.5, t.focus) } else { Stroke::new(1.0, t.border) };
        let radius = if self.rounded { egui::CornerRadius::same((h / 2.0).min(127.0) as u8) } else { egui::CornerRadius::same(t.radius.md) };
        let bg = egui::epaint::RectShape::new(rect, radius, t.surface.sunken, border, StrokeKind::Inside);
        ui.painter().set(bg_idx, bg);
        let cell = Rect::from_min_size(rect.min, Vec2::splat(icon_w));
        if let Some(paint) = self.icon {
            paint(ui.painter(), cell, t.text.secondary);
        } else {
            // magnifier
            let c = pos2(cell.center().x - 1.0, cell.center().y - 1.0);
            let r = h * 0.17;
            let glyph = Stroke::new(1.5, t.text.secondary);
            ui.painter().circle_stroke(c, r, glyph);
            let d = Vec2::splat(std::f32::consts::FRAC_1_SQRT_2);
            ui.painter().line_segment([c + d * r, c + d * (r + h * 0.16)], glyph);
        }
        automation::register_with(ui, &response, self.id, self.hint, Role::Search, Details { value: Some(self.text.as_str()), selected: None });
        SearchResponse { response, changed, submitted, cleared }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;

    #[derive(Default)]
    struct State {
        text: String,
        submitted: u32,
        cleared: u32,
        focus: bool,
    }

    fn harness() -> Harness<'static, State> {
        let mut h = Harness::builder().with_size(vec2(300.0, 60.0)).build_ui_state(
            |ui, s: &mut State| {
                if std::mem::take(&mut s.focus) {
                    SearchField::focus(ui, "files.search");
                }
                let out = SearchField::new("files.search", &mut s.text).hint("Filter files").show(ui);
                s.submitted += out.submitted as u32;
                s.cleared += out.cleared as u32;
            },
            State::default(),
        );
        automation::install(&h.ctx);
        h.state_mut().focus = true;
        h.step();
        h.step();
        h
    }

    #[test]
    fn typing_escape_clears_then_unfocuses() {
        let mut h = harness();
        h.event(egui::Event::Text("report".into()));
        h.step();
        h.step();
        assert_eq!(h.state().text, "report");
        assert_eq!(automation::find(&h.ctx, "files.search").map(|e| e.value), Some("report".into()));
        assert!(automation::find(&h.ctx, "files.search.clear").is_some());
        h.key_press(Key::Escape);
        h.step();
        h.step();
        assert_eq!(h.state().text, "");
        assert_eq!(h.state().cleared, 1);
        h.event(egui::Event::Text("x".into()));
        h.step();
        assert_eq!(h.state().text, "x", "focus kept after clearing");
        h.key_press(Key::Enter);
        h.step();
        assert_eq!(h.state().submitted, 1);
    }

    #[test]
    fn clear_button_empties_the_query() {
        let mut h = harness();
        h.event(egui::Event::Text("abc".into()));
        h.step();
        h.step();
        let clear = automation::find(&h.ctx, "files.search.clear").unwrap();
        h.hover_at(clear.center());
        h.step();
        h.drag_at(clear.center());
        h.step();
        h.drop_at(clear.center());
        h.step();
        assert_eq!(h.state().text, "");
        assert_eq!(h.state().cleared, 1);
    }

    #[test]
    fn house_style_options() {
        let painted = std::cell::Cell::new(None);
        let mut text = String::new();
        let mut h = Harness::builder().with_size(vec2(300.0, 60.0)).build_ui(|ui| {
            let icon = |_: &Painter, cell: Rect, _: Color32| painted.set(Some(cell));
            SearchField::new("files.search", &mut text).height(22.0).rounded(true).text_margin(Margin::same(3)).icon(&icon).width(180.0).show(ui);
        });
        h.step();
        drop(h);
        let cell = painted.get().unwrap();
        assert_eq!(cell.size(), vec2(22.0, 22.0));
        // hostile sizes fall back or clamp
        let mut t = String::new();
        assert_eq!(SearchField::new("x", &mut t).height(f32::NAN).height, None);
        assert_eq!(SearchField::new("x", &mut t).height(1e9).height, Some(96.0));
    }

    #[test]
    fn length_is_capped() {
        let mut h = harness();
        h.event(egui::Event::Text("y".repeat(10_000)));
        h.step();
        assert_eq!(h.state().text.chars().count(), 512);
    }
}
