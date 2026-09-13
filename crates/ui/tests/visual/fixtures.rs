//! Fixtures for the paired visual comparisons: palettes, sizes, the production cases (which use
//! the library) and the reference painters (which never do).
//!
//! Every reference draws from literal numbers or plain egui painter calls, so a change in a
//! component's painting shows up as a pixel and mesh difference.

use ashlar_ui::buttons::{ButtonVisuals, IconButton, IconButtonStyle};
use ashlar_ui::layout::{ResizeHandle, ResizeHandleStyle, SplitAxis, SplitSize, fit_toolbar, split_rect};
use ashlar_ui::tabs::Tab;
use ashlar_ui::theme::WidgetPalette;
use egui::{Color32, CornerRadius, Id, Painter, Rect, Response, Stroke, StrokeKind, Theme, Ui, UiBuilder, Vec2, pos2, vec2};

/// Canvas size in points.
pub const CANVAS: Vec2 = Vec2::new(128.0, 72.0);
/// Where the first control sits.
pub const ORIGIN: f32 = 12.0;
/// Space between the two controls.
pub const GAP: f32 = 8.0;

pub fn surface(theme: Theme) -> Color32 {
    match theme {
        Theme::Light => Color32::from_rgb(248, 250, 252),
        Theme::Dark => Color32::from_rgb(17, 24, 39),
    }
}

/// The nine colours every fixture draws with.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub normal: Color32,
    pub hovered: Color32,
    pub pressed: Color32,
    pub selected: Color32,
    pub border: Color32,
    pub icon: Color32,
    pub hover_icon: Color32,
    pub press_icon: Color32,
    pub focus: Color32,
}

pub fn palette(theme: Theme) -> Palette {
    let c = |r, g, b| Color32::from_rgb(r, g, b);
    match theme {
        Theme::Light => Palette {
            normal: c(226, 232, 240),
            hovered: c(186, 230, 253),
            pressed: c(125, 211, 252),
            selected: c(37, 99, 235),
            border: c(148, 163, 184),
            icon: c(15, 23, 42),
            hover_icon: c(12, 74, 110),
            press_icon: c(8, 47, 73),
            focus: c(220, 38, 38),
        },
        Theme::Dark => Palette {
            normal: c(31, 41, 55),
            hovered: c(55, 65, 81),
            pressed: c(75, 85, 99),
            selected: c(67, 56, 202),
            border: c(100, 116, 139),
            icon: c(226, 232, 240),
            hover_icon: c(243, 244, 246),
            press_icon: c(255, 255, 255),
            focus: c(56, 189, 248),
        },
    }
}

pub const RADIUS: u8 = 3;

pub fn button_style(p: Palette) -> IconButtonStyle {
    let border = Stroke::new(1.0, p.border);
    IconButtonStyle {
        normal: ButtonVisuals { fill: p.normal, stroke: border, icon: p.icon },
        hovered: ButtonVisuals { fill: p.hovered, stroke: border, icon: p.hover_icon },
        pressed: Some(ButtonVisuals { fill: p.pressed, stroke: border, icon: p.press_icon }),
        selected: ButtonVisuals { fill: p.selected, stroke: border, icon: Color32::WHITE },
        corner_radius: CornerRadius::same(RADIUS),
        focus_stroke: Stroke::new(1.0, p.focus),
    }
}

/// The interaction state a case puts its first control in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Normal,
    Hover,
    Press,
    Selected,
    SelectedPress,
    Disabled,
    Focus,
}

impl State {
    pub const ALL: [State; 7] = [State::Normal, State::Hover, State::Press, State::Selected, State::SelectedPress, State::Disabled, State::Focus];

    pub fn name(self) -> &'static str {
        match self {
            State::Normal => "normal",
            State::Hover => "hover",
            State::Press => "press",
            State::Selected => "selected",
            State::SelectedPress => "selected-press",
            State::Disabled => "disabled",
            State::Focus => "focus",
        }
    }

    pub fn selected(self) -> bool {
        matches!(self, State::Selected | State::SelectedPress)
    }

    /// Pointer over the control at frame 2.
    pub fn hovers(self) -> bool {
        matches!(self, State::Hover | State::Press | State::SelectedPress | State::Disabled)
    }

    /// Primary button pressed on the control at frame 2.
    pub fn presses(self) -> bool {
        matches!(self, State::Press | State::SelectedPress | State::Disabled)
    }

    /// Keyboard focus requested at frame 2.
    pub fn focuses(self) -> bool {
        matches!(self, State::Focus | State::Disabled)
    }

    pub fn interactive(self) -> bool {
        self.hovers() || self.presses() || self.focuses()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    Button,
    SingleShapeButton,
    Tab,
    HorizontalSplit,
    VerticalSplit,
}

impl Component {
    pub const ALL: [Component; 5] =
        [Component::Button, Component::SingleShapeButton, Component::Tab, Component::HorizontalSplit, Component::VerticalSplit];

    pub fn name(self) -> &'static str {
        match self {
            Component::Button => "button",
            Component::SingleShapeButton => "single-shape-button",
            Component::Tab => "tab",
            Component::HorizontalSplit => "horizontal-split",
            Component::VerticalSplit => "vertical-split",
        }
    }

    pub fn is_split(self) -> bool {
        matches!(self, Component::HorizontalSplit | Component::VerticalSplit)
    }

    pub fn states(self) -> Vec<State> {
        State::ALL.into_iter().filter(|s| !(self.is_split() && s.selected())).collect()
    }

    pub fn size(self, compact: bool) -> Vec2 {
        match (self, compact) {
            (Component::Tab, false) => vec2(44.0, 28.0),
            (Component::Tab, true) => vec2(22.0, 12.0),
            (_, false) => vec2(36.0, 28.0),
            (_, true) => vec2(18.0, 12.0),
        }
    }
}

/// The two control rects of a button or tab case.
pub fn control_rects(component: Component, compact: bool, extra_spacing: f32) -> [Rect; 2] {
    let size = component.size(compact);
    let first = Rect::from_min_size(pos2(ORIGIN, ORIGIN), size);
    let second = Rect::from_min_size(pos2(ORIGIN + size.x + GAP + extra_spacing, ORIGIN), size);
    [first, second]
}

/// Where the pointer goes for interactive states: the first control's centre (literal for the
/// split dividers).
pub fn control_center(component: Component, compact: bool) -> egui::Pos2 {
    match (component, compact) {
        (Component::HorizontalSplit, false) => pos2(51.5, 34.0),
        (Component::HorizontalSplit, true) => pos2(38.0, 22.0),
        (Component::VerticalSplit, false) => pos2(64.0, 29.0),
        (Component::VerticalSplit, true) => pos2(38.0, 22.0),
        _ => control_rects(component, compact, 0.0)[0].center(),
    }
}

/// What the production case reports about its first control.
#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub target: Option<Id>,
    pub rect: Option<Rect>,
    pub enabled: bool,
    pub hovered: bool,
    pub down: bool,
    pub dragged: bool,
    pub focused: bool,
}

impl Probe {
    pub fn record(&mut self, r: &Response) {
        self.target = Some(r.id);
        self.rect = Some(r.rect);
        self.enabled = r.enabled();
        self.hovered = r.hovered();
        self.down = r.is_pointer_button_down_on();
        self.dragged = r.dragged();
        self.focused = r.has_focus();
    }
}

/// An X of two 1-point lines.
pub fn paint_icon(painter: &Painter, rect: Rect, color: Color32, compact: bool) {
    let half = if compact { vec2(3.0, 2.0) } else { vec2(6.0, 4.0) };
    let c = rect.center();
    let stroke = Stroke::new(1.0, color);
    painter.line_segment([c - half, c + half], stroke);
    painter.line_segment([pos2(c.x - half.x, c.y + half.y), pos2(c.x + half.x, c.y - half.y)], stroke);
}

// ---------------------------------------------------------------------------------------------
// Production cases: these use the library.

/// Draw the production version of `component` in `state`.
pub fn production(ui: &mut Ui, component: Component, state: State, theme: Theme, compact: bool, probe: &mut Probe) {
    let p = palette(theme);
    match component {
        Component::Button | Component::SingleShapeButton => {
            let style = button_style(p);
            for (i, rect) in control_rects(component, compact, 0.0).into_iter().enumerate() {
                let first = i == 0;
                let mut builder = UiBuilder::new().max_rect(rect).id_salt(("control", i));
                if first && state == State::Disabled {
                    builder = builder.disabled();
                }
                ui.scope_builder(builder, |ui| {
                    let mut button = IconButton::new("", rect.size(), &style).selected(first && state.selected());
                    if component == Component::SingleShapeButton {
                        button = button.single_shape_frame();
                    }
                    let r = button.show(ui, |ui, rect, color| paint_icon(ui.painter(), rect, color, compact));
                    if first {
                        probe.record(&r);
                    }
                });
            }
        }
        Component::Tab => {
            for (i, rect) in control_rects(component, compact, 0.0).into_iter().enumerate() {
                let first = i == 0;
                let selected = first && state.selected();
                let mut builder = UiBuilder::new().max_rect(rect).id_salt(("control", i));
                if first && state == State::Disabled {
                    builder = builder.disabled();
                }
                ui.scope_builder(builder, |ui| {
                    let r = Tab::new(Id::new(("tab", i)), "", selected).focus_stroke(Stroke::new(1.0, p.focus)).show_at(ui, rect, |ui, r| {
                        let visuals = tab_visuals(p, selected, r.is_pointer_button_down_on(), r.hovered());
                        paint_tab(ui.painter(), rect, visuals, compact);
                    });
                    if first {
                        probe.record(&r);
                    }
                });
            }
        }
        Component::HorizontalSplit | Component::VerticalSplit => {
            let axis = if component == Component::HorizontalSplit { SplitAxis::Horizontal } else { SplitAxis::Vertical };
            let (area, ratio, gap) = split_area(compact);
            let mut builder = UiBuilder::new().max_rect(area).id_salt("split");
            if state == State::Disabled {
                builder = builder.disabled();
            }
            ui.scope_builder(builder, |ui| {
                let Some(layout) = split_rect(area, axis, SplitSize::Ratio(ratio), gap, 4.0) else { return };
                ui.painter().rect_filled(layout.first, 0.0, p.normal);
                ui.painter().rect_filled(layout.second, 0.0, p.hovered);
                let style = ResizeHandleStyle { hovered: p.hovered, dragged: p.pressed, focused: p.focus };
                let r = ResizeHandle::new(Id::new("divider"), axis, layout.divider, "", style).hit_margin(0.0).show(ui);
                probe.record(&r.response);
            });
        }
    }
}

pub fn tab_visuals(p: Palette, selected: bool, down: bool, hovered: bool) -> (Color32, Color32) {
    if selected {
        (p.selected, Color32::WHITE)
    } else if down {
        (p.pressed, p.press_icon)
    } else if hovered {
        (p.hovered, p.hover_icon)
    } else {
        (p.normal, p.icon)
    }
}

/// The tab look shared by the production case's paint callback and the reference.
pub fn paint_tab(painter: &Painter, rect: Rect, (fill, icon): (Color32, Color32), compact: bool) {
    painter.rect_filled(rect, CornerRadius::same(RADIUS), fill);
    paint_icon(painter, rect, icon, compact);
}

pub fn split_area(compact: bool) -> (Rect, f32, f32) {
    if compact {
        (Rect::from_min_size(pos2(ORIGIN, ORIGIN), vec2(52.0, 20.0)), 0.5, 3.0)
    } else {
        (Rect::from_min_size(pos2(ORIGIN, ORIGIN), vec2(104.0, 44.0)), 0.375, 4.0)
    }
}

// ---------------------------------------------------------------------------------------------
// References: plain painter calls and literal geometry, never the library.

/// A deliberate difference for the negative comparisons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mutation {
    None,
    Fill,
    Spacing,
    StrokeWidth,
}

impl Mutation {
    pub fn name(self) -> &'static str {
        match self {
            Mutation::None => "none",
            Mutation::Fill => "fill",
            Mutation::Spacing => "spacing",
            Mutation::StrokeWidth => "stroke-width",
        }
    }
}

fn faded(ui: &Ui, disabled: bool) -> Painter {
    let mut painter = ui.painter().clone();
    if disabled {
        painter.multiply_opacity(0.4);
    }
    painter
}

/// Paint what `component` in `state` is expected to look like.
pub fn reference(ui: &mut Ui, component: Component, state: State, theme: Theme, compact: bool, mutation: Mutation) {
    let p = palette(theme);
    let radius = CornerRadius::same(RADIUS);
    match component {
        Component::Button | Component::SingleShapeButton => {
            let spacing = if mutation == Mutation::Spacing { 1.0 } else { 0.0 };
            for (i, rect) in control_rects(component, compact, spacing).into_iter().enumerate() {
                let first = i == 0;
                let painter = faded(ui, first && state == State::Disabled);
                let (mut fill, icon) = match (first, state) {
                    (true, State::Selected | State::SelectedPress) => (p.selected, Color32::WHITE),
                    (true, State::Press) => (p.pressed, p.press_icon),
                    (true, State::Hover) => (p.hovered, p.hover_icon),
                    _ => (p.normal, p.icon),
                };
                if mutation == Mutation::Fill && first {
                    fill = Color32::from_rgb(200, 60, 130);
                }
                let width = if mutation == Mutation::StrokeWidth { 2.0 } else { 1.0 };
                let stroke = Stroke::new(width, p.border);
                if component == Component::SingleShapeButton {
                    painter.rect(rect, radius, fill, stroke, StrokeKind::Inside);
                } else {
                    painter.rect_filled(rect, radius, fill);
                    painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
                }
                paint_icon(&painter, rect, icon, compact);
                if first && state == State::Focus {
                    painter.rect_stroke(rect.shrink(2.0), radius, Stroke::new(1.0, p.focus), StrokeKind::Inside);
                }
            }
        }
        Component::Tab => {
            for (i, rect) in control_rects(component, compact, 0.0).into_iter().enumerate() {
                let first = i == 0;
                let painter = faded(ui, first && state == State::Disabled);
                let visuals = match (first, state) {
                    (true, State::Selected | State::SelectedPress) => tab_visuals(p, true, false, false),
                    (true, State::Press) => tab_visuals(p, false, true, true),
                    (true, State::Hover) => tab_visuals(p, false, false, true),
                    _ => tab_visuals(p, false, false, false),
                };
                paint_tab(&painter, rect, visuals, compact);
                if first && state == State::Focus {
                    painter.rect_stroke(rect.shrink(2.0), 0.0, Stroke::new(1.0, p.focus), StrokeKind::Inside);
                }
            }
        }
        Component::HorizontalSplit | Component::VerticalSplit => {
            let r = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(x, y), vec2(w, h));
            let [first, divider, second] = match (component, compact) {
                (Component::HorizontalSplit, false) => [r(12.0, 12.0, 37.5, 44.0), r(49.5, 12.0, 4.0, 44.0), r(53.5, 12.0, 62.5, 44.0)],
                (Component::HorizontalSplit, true) => [r(12.0, 12.0, 24.5, 20.0), r(36.5, 12.0, 3.0, 20.0), r(39.5, 12.0, 24.5, 20.0)],
                (_, false) => [r(12.0, 12.0, 104.0, 15.0), r(12.0, 27.0, 104.0, 4.0), r(12.0, 31.0, 104.0, 25.0)],
                (_, true) => [r(12.0, 12.0, 52.0, 8.5), r(12.0, 20.5, 52.0, 3.0), r(12.0, 23.5, 52.0, 8.5)],
            };
            let painter = faded(ui, state == State::Disabled);
            painter.rect_filled(first, 0.0, p.normal);
            painter.rect_filled(second, 0.0, p.hovered);
            let fill = match state {
                State::Hover => Some(p.hovered),
                State::Press => Some(p.pressed),
                State::Focus => Some(p.focus),
                _ => None,
            };
            if let Some(fill) = fill {
                painter.rect_filled(divider, 0.0, fill);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Toolbar overflow

pub const TOOLBAR_GROUPS: [f32; 3] = [24.0, 28.0, 36.0];
pub const TOOLBAR_AVAILABLE: [f32; 3] = [112.0, 64.0, 20.0];

pub fn toolbar_production(ui: &Ui, theme: Theme, available: f32) {
    let p = palette(theme);
    let Some(fit) = fit_toolbar(&TOOLBAR_GROUPS, available, 4.0, 20.0) else { return };
    let mut x = ORIGIN;
    for &(_, w) in &fit.shown {
        toolbar_cell(ui.painter(), Rect::from_min_size(pos2(x, ORIGIN), vec2(w, 24.0)), p.normal, p);
        x += w + 4.0;
    }
    if !fit.overflow.is_empty() {
        toolbar_cell(ui.painter(), Rect::from_min_size(pos2(x, ORIGIN), vec2(fit.overflow_width, 24.0)), p.selected, p);
    }
}

pub fn toolbar_reference(ui: &Ui, theme: Theme, available: f32) {
    let p = palette(theme);
    let r = |x: f32, w: f32| Rect::from_min_size(pos2(x, 12.0), vec2(w, 24.0));
    let (shown, overflow): (Vec<Rect>, Option<Rect>) = match available as u32 {
        112 => (vec![r(12.0, 24.0), r(40.0, 28.0), r(72.0, 36.0)], None),
        64 => (vec![r(12.0, 24.0)], Some(r(40.0, 20.0))),
        _ => (vec![], Some(r(12.0, 20.0))),
    };
    for rect in shown {
        toolbar_cell(ui.painter(), rect, p.normal, p);
    }
    if let Some(rect) = overflow {
        toolbar_cell(ui.painter(), rect, p.selected, p);
    }
}

fn toolbar_cell(painter: &Painter, rect: Rect, fill: Color32, p: Palette) {
    painter.rect_filled(rect, 0.0, fill);
    painter.rect_stroke(rect, 0.0, Stroke::new(1.0, p.border), StrokeKind::Inside);
}

// ---------------------------------------------------------------------------------------------
// Widget palette

fn cell_rect(i: usize, compact: bool) -> Rect {
    let size = if compact { vec2(20.0, 12.0) } else { vec2(26.0, 18.0) };
    let (col, row) = ((i % 3) as f32, (i / 3) as f32);
    Rect::from_min_size(pos2(ORIGIN + col * 34.0, ORIGIN + row * 26.0), size)
}

/// Five distinct border colours, one per widget state.
fn borders(p: Palette) -> [Color32; 5] {
    [p.border, p.focus, p.icon, p.hover_icon, p.press_icon]
}

fn fills(p: Palette) -> [Color32; 5] {
    [p.normal, p.hovered, p.pressed, p.selected, p.border]
}

fn paint_cell(painter: &Painter, rect: Rect, radius: CornerRadius, fill: Color32, weak: Color32, border: Stroke, fg: Stroke) {
    painter.rect_filled(rect, radius, fill);
    painter.rect_stroke(rect, radius, border, StrokeKind::Inside);
    painter.rect_filled(rect.shrink(3.0), radius, weak);
    painter.line_segment([pos2(rect.left() + 4.0, rect.center().y), pos2(rect.right() - 4.0, rect.center().y)], fg);
}

pub fn palette_production(ui: &Ui, theme: Theme, compact: bool) {
    let p = palette(theme);
    let mut widgets = match theme {
        Theme::Light => egui::Visuals::light().widgets,
        Theme::Dark => egui::Visuals::dark().widgets,
    };
    let borders = borders(p);
    for (i, w) in
        [&mut widgets.noninteractive, &mut widgets.inactive, &mut widgets.hovered, &mut widgets.active, &mut widgets.open].into_iter().enumerate()
    {
        w.bg_stroke = Stroke::new(1.0, borders[i]);
        w.expansion = if i % 2 == 1 { 0.5 } else { 0.0 };
    }
    let [noninteractive, inactive, hovered, active, open] = fills(p);
    WidgetPalette { noninteractive, inactive, hovered, active, open }.apply_to(&mut widgets, CornerRadius::same(2), Stroke::new(1.5, p.icon));
    for (i, w) in [widgets.noninteractive, widgets.inactive, widgets.hovered, widgets.active, widgets.open].into_iter().enumerate() {
        paint_cell(ui.painter(), cell_rect(i, compact).expand(w.expansion), w.corner_radius, w.bg_fill, w.weak_bg_fill, w.bg_stroke, w.fg_stroke);
    }
}

pub fn palette_reference(ui: &Ui, theme: Theme, compact: bool) {
    let p = palette(theme);
    let (borders, fills) = (borders(p), fills(p));
    for i in 0..5 {
        let rect = if i % 2 == 1 { cell_rect(i, compact).expand(0.5) } else { cell_rect(i, compact) };
        paint_cell(ui.painter(), rect, CornerRadius::same(2), fills[i], fills[i], Stroke::new(1.0, borders[i]), Stroke::new(1.5, p.icon));
    }
}

// ---------------------------------------------------------------------------------------------
// SVG

/// The source as an app would embed it.
pub const SVG_SOURCE: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="4" y="4" width="16" height="16"/><path d="M4 4 L20 20" stroke-width="2"/></svg>"##;

/// The same SVG with the substitutions already made, per theme.
pub fn svg_expected(theme: Theme) -> &'static [u8] {
    match theme {
        Theme::Light => {
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#0f172a" stroke-width="1.75"><rect x="4" y="4" width="16" height="16"/><path d="M4 4 L20 20" stroke-width="1.75"/></svg>"##
        }
        Theme::Dark => {
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#e2e8f0" stroke-width="1.75"><rect x="4" y="4" width="16" height="16"/><path d="M4 4 L20 20" stroke-width="1.75"/></svg>"##
        }
    }
}

pub fn svg_colour(theme: Theme) -> &'static str {
    match theme {
        Theme::Light => "#0f172a",
        Theme::Dark => "#e2e8f0",
    }
}
