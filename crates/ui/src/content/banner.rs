//! An inline message across a view: a notice, a warning, an error, with an optional action.

use egui::accesskit::{Live, Role as AkRole};
use egui::{Color32, CornerRadius, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, WidgetInfo, WidgetType, pos2, vec2};

use crate::automation::{self, Details, Role};
use crate::controls::{Button, Size, Tone, Variant, mix};
use crate::tokens::{self, Tokens};

/// Width of the coloured stripe on the leading edge.
const STRIPE: f32 = 3.0;

/// A message with a tone, an optional title, action and close button. It is a live region:
/// screen readers announce it when it appears (assertively for [`Tone::Danger`], politely
/// otherwise). The app keeps whether it is shown.
pub struct Banner<'a> {
    id: &'a str,
    tone: Tone,
    text: &'a str,
    title: Option<&'a str>,
    action: Option<&'a str>,
    dismiss: Option<&'a str>,
}

/// What [`Banner::show`] reports.
pub struct BannerOutput {
    /// The whole banner.
    pub response: Response,
    /// The action button was clicked.
    pub action: bool,
    /// The close button was clicked: hide the banner.
    pub dismissed: bool,
}

/// Fill, border and stripe colours of a banner.
pub(crate) fn colors(t: &Tokens, tone: Tone) -> (Color32, Color32, Color32) {
    let strong = tone.color(t);
    (mix(t.surface.raised, strong, 0.125), mix(t.border, strong, 0.375), strong)
}

impl<'a> Banner<'a> {
    pub fn new(id: &'a str, tone: Tone, text: &'a str) -> Self {
        Banner { id, tone, text, title: None, action: None, dismiss: None }
    }
    /// A bold first line.
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }
    /// A button on the trailing side (registered as `<id>.action`).
    pub fn action(mut self, label: &'a str) -> Self {
        self.action = Some(label);
        self
    }
    /// A close button with this accessible name (registered as `<id>.dismiss`).
    pub fn dismissible(mut self, label: &'a str) -> Self {
        self.dismiss = Some(label);
        self
    }

    pub fn show(self, ui: &mut Ui) -> BannerOutput {
        let t = tokens::current(ui.ctx());
        let pad = t.space.md;
        let width = ui.available_width().clamp(4.0 * t.control_height, 8000.0);
        let origin = ui.available_rect_before_wrap().min;
        // the background goes under everything drawn below; its height is known at the end
        let background = ui.painter().add(Shape::Noop);
        let stripe = ui.painter().add(Shape::Noop);

        // trailing buttons first: the text wraps in what they leave
        let mut action = false;
        let mut dismissed = false;
        let row = Rect::from_min_size(pos2(origin.x + STRIPE + pad, origin.y + pad), vec2(width - STRIPE - 2.0 * pad, t.control_height));
        let mut buttons_w = 0.0;
        if self.action.is_some() || self.dismiss.is_some() {
            let mut child = ui.new_child(UiBuilder::new().max_rect(row).layout(egui::Layout::right_to_left(egui::Align::Center)));
            child.spacing_mut().item_spacing.x = t.space.xs;
            if let Some(label) = self.dismiss {
                dismissed = dismiss_button(&mut child, &t, self.id, label).clicked();
            }
            if let Some(label) = self.action {
                action = Button::new(self.id, label).part("action").size(Size::Sm).variant(Variant::Secondary).show(&mut child).clicked();
            }
            buttons_w = child.min_rect().width() + t.space.md;
        }

        let text_w = (row.width() - buttons_w).max(t.control_height);
        let text_x = row.left();
        let mut y = origin.y + pad;
        let size = t.type_scale.body;
        let title_galley = self.title.map(|title| wrapped(ui, (self.id, "title"), title, size, text_w));
        let body_galley = wrapped(ui, (self.id, "text"), self.text, size, text_w);
        let text_h = title_galley.as_ref().map_or(0.0, |g| g.size().y + t.space.xxs) + body_galley.size().y;
        let height = text_h.max(t.control_height) + 2.0 * pad;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());

        let (fill, border, strong) = colors(&t, self.tone);
        let radius = CornerRadius::same(t.radius.md);
        ui.painter().set(background, Shape::rect_filled(rect, radius, fill));
        let stripe_rect = Rect::from_min_size(rect.min, vec2(STRIPE, rect.height()));
        ui.painter().set(stripe, Shape::rect_filled(stripe_rect, CornerRadius { nw: t.radius.md, sw: t.radius.md, ne: 0, se: 0 }, strong));
        ui.painter().rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);
        // centre a single line on the buttons' row
        if text_h < t.control_height {
            y += (t.control_height - text_h) / 2.0;
        }
        if let Some(g) = title_galley {
            let h = g.size().y;
            ui.painter().galley(pos2(text_x, y), g, t.text.primary);
            y += h + t.space.xxs;
        }
        ui.painter().galley(pos2(text_x, y), body_galley, t.text.primary);

        let (label, text) = (self.title.unwrap_or(self.text), self.text);
        let assertive = self.tone == Tone::Danger;
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label));
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_role(if assertive { AkRole::Alert } else { AkRole::Status });
            node.set_live(if assertive { Live::Assertive } else { Live::Polite });
            node.set_label(label);
            // the name carries the text: a value would make readers say it twice
            node.clear_value();
            if label != text {
                node.set_description(text);
            }
        });
        automation::register_with(ui, &response, self.id, label, Role::Alert, Details { value: Some(text), selected: None });
        BannerOutput { response, action, dismissed }
    }
}

/// `text` wrapped at `width`, from the shared galley cache.
fn wrapped(ui: &Ui, key: (&str, &str), text: &str, size: f32, width: f32) -> std::sync::Arc<egui::Galley> {
    let revision = crate::cache::hash_of(&(text, size.to_bits(), width.to_bits()));
    crate::cache::galley(ui, ("ashlar_ui::content::banner", key), revision, || {
        egui::text::LayoutJob::simple(text.to_owned(), egui::FontId::proportional(size), Color32::PLACEHOLDER, width)
    })
}

/// A small × button.
fn dismiss_button(ui: &mut Ui, t: &Tokens, id: &str, label: &str) -> Response {
    let side = (t.control_height - 4.0).max(12.0);
    let (rect, response) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = CornerRadius::same(t.radius.sm);
        if response.is_pointer_button_down_on() {
            painter.rect_filled(rect, radius, t.surface.pressed);
        } else if response.hovered() {
            painter.rect_filled(rect, radius, t.surface.hover);
        }
        let c = rect.center();
        let h = (side * 0.2).round();
        let stroke = Stroke::new(1.5, t.text.secondary);
        painter.line_segment([pos2(c.x - h, c.y - h), pos2(c.x + h, c.y + h)], stroke);
        painter.line_segment([pos2(c.x - h, c.y + h), pos2(c.x + h, c.y - h)], stroke);
        if response.has_focus() {
            crate::controls::focus_ring(painter, rect, radius, t);
        }
    }
    automation::register_fmt(ui, rect, format_args!("{id}.dismiss"), label, Role::Button, ui.is_enabled(), Details::default());
    response
}
