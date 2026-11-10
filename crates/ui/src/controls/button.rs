//! Text buttons with variants, sizes, an optional icon and a loading state.

use egui::{Align2, Color32, CornerRadius, Painter, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, pos2, vec2};

use super::{dimmed, focus_ring, mix, text_galley};
use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};

/// How much a button stands out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Variant {
    /// The main action of a view or dialog: accent fill.
    Primary,
    /// Any other action: a sunken fill with a border.
    #[default]
    Secondary,
    /// A quiet action in a toolbar or list: no fill until hovered.
    Ghost,
    /// Deleting or discarding: the danger colour.
    Destructive,
}

/// Button height and type size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Size {
    /// 4 points shorter than a one-line control, small type.
    Sm,
    /// The height of a one-line control (`Tokens::control_height`).
    #[default]
    Md,
    /// 4 points taller than a one-line control, with wider padding (dialog buttons).
    Lg,
}

impl Size {
    /// Height in points.
    pub fn height(self, t: &Tokens) -> f32 {
        match self {
            Size::Sm => (t.control_height - 4.0).max(12.0),
            Size::Md => t.control_height,
            Size::Lg => t.control_height + 4.0,
        }
    }
    /// Font size in points.
    pub fn font_size(self, t: &Tokens) -> f32 {
        match self {
            Size::Sm => t.type_scale.small,
            Size::Md | Size::Lg => t.type_scale.button,
        }
    }
    /// Horizontal padding on each side, in points.
    pub fn padding(self, t: &Tokens) -> f32 {
        match self {
            Size::Sm => t.space.sm,
            Size::Md => t.space.md,
            Size::Lg => t.space.lg,
        }
    }
}

/// Fill, border and text colours of a variant in a pointer state.
pub(crate) fn colors(t: &Tokens, variant: Variant, hovered: bool, pressed: bool) -> (Color32, Stroke, Color32) {
    let pick = |normal: Color32, hover: Color32, press: Color32| {
        if pressed {
            press
        } else if hovered {
            hover
        } else {
            normal
        }
    };
    match variant {
        Variant::Primary => {
            let fill = pick(t.accent, t.accent_hover, mix(t.accent, Color32::BLACK, 0.25));
            (fill, Stroke::new(1.0, fill), t.text.on_accent)
        }
        Variant::Destructive => {
            let d = t.status.danger;
            let fill = pick(d, mix(d, Color32::BLACK, 0.125), mix(d, Color32::BLACK, 0.25));
            (fill, Stroke::new(1.0, fill), Color32::WHITE)
        }
        Variant::Secondary => {
            let fill = pick(t.surface.sunken, t.surface.hover, t.surface.pressed);
            let border = if hovered || pressed { t.border_strong } else { t.border };
            (fill, Stroke::new(1.0, border), t.text.primary)
        }
        Variant::Ghost => (pick(Color32::TRANSPARENT, t.surface.hover, t.surface.pressed), Stroke::NONE, t.text.primary),
    }
}

/// Paints an icon in the square it is given, in the colour it is given.
pub type IconPainter<'a> = &'a dyn Fn(&Painter, Rect, Color32);

/// A text button. The label is shown and is the accessible name; the id is the automation id
/// (and keeps the button's identity while the label changes, as when it is translated).
///
/// While [`Button::loading`] is set the button shows a spinner instead of its icon, ignores
/// clicks and reports itself busy; it keeps its width, so the row does not move.
pub struct Button<'a> {
    id: &'a str,
    suffix: Option<(&'static str, Option<usize>)>,
    label: &'a str,
    variant: Variant,
    size: Size,
    loading: bool,
    enabled: bool,
    reduced_motion: bool,
    min_width: f32,
    icon: Option<IconPainter<'a>>,
}

impl<'a> Button<'a> {
    pub fn new(id: &'a str, label: &'a str) -> Self {
        Button {
            id,
            suffix: None,
            label,
            variant: Variant::Secondary,
            size: Size::Md,
            loading: false,
            enabled: true,
            reduced_motion: false,
            min_width: 0.0,
            icon: None,
        }
    }
    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }
    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }
    /// Show a spinner, ignore clicks and report busy.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    /// Hold the loading spinner still and ask for no frames.
    pub fn reduced_motion(mut self, reduced: bool) -> Self {
        self.reduced_motion = reduced;
        self
    }
    /// The narrowest the button gets, in points (clamped to 0–2000).
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = if width.is_finite() { width.clamp(0.0, 2000.0) } else { 0.0 };
        self
    }
    /// An icon left of the label, painted in the label's colour.
    pub fn icon(mut self, paint: IconPainter<'a>) -> Self {
        self.icon = Some(paint);
        self
    }
    /// Register under `<id>.<part>.<index>` (`dialog.button.2`) without formatting a string each
    /// frame: for buttons made in a loop.
    pub fn indexed(mut self, part: &'static str, index: usize) -> Self {
        self.suffix = Some((part, Some(index)));
        self
    }
    /// Register under `<id>.<part>` (`banner.action`): for a button that is part of a larger
    /// component with its own id.
    pub fn part(mut self, part: &'static str) -> Self {
        self.suffix = Some((part, None));
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let height = self.size.height(&t);
        let font_size = self.size.font_size(&t);
        let pad = self.size.padding(&t);
        let galley = (!self.label.is_empty()).then(|| text_galley(ui, ("ashlar_ui::controls::button", self.id, self.suffix), self.label, font_size));
        let text_w = galley.as_ref().map_or(0.0, |g| g.size().x);
        let has_icon = self.icon.is_some() || self.loading;
        let icon_side = font_size.round();
        let gap = if has_icon && galley.is_some() { t.space.sm } else { 0.0 };
        let content_w = text_w + gap + if has_icon { icon_side } else { 0.0 };
        let width = (content_w + 2.0 * pad).max(self.min_width).max(height);

        let (auto_id, rect) = ui.allocate_space(vec2(width, height));
        let id = if self.id.is_empty() { auto_id } else { ui.make_persistent_id(("ashlar_ui::controls::button", self.id, self.suffix)) };
        let active = self.enabled && !self.loading;
        let sense = if active { Sense::click() } else { Sense::hover() };
        let response = ui.interact(rect, id, sense);
        let enabled = self.enabled && ui.is_enabled();
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, self.label));
        if self.loading {
            ui.ctx().accesskit_node_builder(response.id, |node| node.set_busy());
        }

        if ui.is_rect_visible(rect) {
            let live = active && ui.is_enabled();
            let pressed = live && response.is_pointer_button_down_on();
            let hovered = live && response.hovered();
            let (fill, stroke, text) = colors(&t, self.variant, hovered, pressed);
            let radius = CornerRadius::same(t.radius.md);
            let painter = dimmed(ui, !self.enabled);
            painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
            let mut x = rect.center().x - content_w / 2.0;
            if has_icon {
                let icon_rect = Rect::from_min_size(pos2(x, rect.center().y - icon_side / 2.0), vec2(icon_side, icon_side));
                if self.loading {
                    super::progress::spinner(ui, &painter, icon_rect, text, self.reduced_motion);
                } else if let Some(paint) = self.icon {
                    paint(&painter, icon_rect, text);
                }
                x += icon_side + gap;
            }
            if let Some(galley) = galley {
                let pos = Align2::LEFT_CENTER.align_size_within_rect(galley.size(), Rect::from_x_y_ranges(x..=x + text_w, rect.y_range())).min;
                painter.galley(pos, galley, text);
            }
            if response.has_focus() {
                focus_ring(&painter, rect, radius, &t);
            }
        }
        let details = Details { value: None, selected: None };
        if self.id.is_empty() {
            return response;
        }
        match self.suffix {
            Some((part, Some(index))) => {
                automation::register_fmt(ui, rect, format_args!("{}.{part}.{index}", self.id), self.label, Role::Button, enabled && active, details)
            }
            Some((part, None)) => {
                automation::register_fmt(ui, rect, format_args!("{}.{part}", self.id), self.label, Role::Button, enabled && active, details)
            }
            None => automation::register_fmt(ui, rect, format_args!("{}", self.id), self.label, Role::Button, enabled && active, details),
        }
        response
    }
}
