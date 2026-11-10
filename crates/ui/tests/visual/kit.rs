//! Fixtures for the controls kit (`ashlar_ui::controls`): production cases that use the library
//! with the built-in token sets, and reference painters that draw the same controls from literal
//! colours and geometry.
//!
//! The controls are drawn without labels (the comparison renders without fonts), so what is
//! compared is the frame, track, knob, circle and fill of each control in each state.

use ashlar_ui::controls::{Button, Progress, RadioGroup, Switch, Variant};
use egui::{Color32, CornerRadius, Painter, Rect, Stroke, StrokeKind, Theme, Ui, UiBuilder, pos2, vec2};

use super::fixtures::{ORIGIN, Probe, State};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kit {
    Button(Variant),
    Switch,
    Radio,
    /// A determinate bar at this fraction, or an indeterminate one under reduced motion.
    Bar(Option<f32>),
}

impl Kit {
    pub const ALL: [Kit; 9] = [
        Kit::Button(Variant::Primary),
        Kit::Button(Variant::Secondary),
        Kit::Button(Variant::Ghost),
        Kit::Button(Variant::Destructive),
        Kit::Switch,
        Kit::Radio,
        Kit::Bar(Some(0.25)),
        Kit::Bar(Some(1.0)),
        Kit::Bar(None),
    ];

    pub fn name(self) -> String {
        match self {
            Kit::Button(v) => format!("button-{}", format!("{v:?}").to_lowercase()),
            Kit::Switch => "switch".into(),
            Kit::Radio => "radio".into(),
            Kit::Bar(Some(f)) => format!("bar-{}", (f * 100.0) as u32),
            Kit::Bar(None) => "bar-indeterminate".into(),
        }
    }

    pub fn states(self) -> Vec<State> {
        match self {
            Kit::Button(_) => vec![State::Normal, State::Hover, State::Press, State::Disabled, State::Focus],
            Kit::Switch | Kit::Radio => State::ALL.to_vec(),
            Kit::Bar(_) => vec![State::Normal],
        }
    }

    /// Where the pointer goes: the control's centre (literal).
    pub fn center(self) -> egui::Pos2 {
        match self {
            Kit::Button(_) => pos2(32.0, 24.0),
            Kit::Switch => pos2(28.0, 21.0),
            Kit::Radio => pos2(19.0, 24.0),
            Kit::Bar(_) => pos2(52.0, 15.0),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Production: the library with the built-in tokens of the harness theme.

pub fn production(ui: &mut Ui, kit: Kit, state: State, probe: &mut Probe) {
    let area = Rect::from_min_size(pos2(ORIGIN, ORIGIN), vec2(100.0, 40.0));
    let mut builder = UiBuilder::new().max_rect(area).id_salt("kit");
    if state == State::Disabled {
        builder = builder.disabled();
    }
    ui.scope_builder(builder, |ui| match kit {
        Kit::Button(variant) => {
            let r = Button::new("", "").variant(variant).min_width(40.0).show(ui);
            probe.record(&r);
        }
        Kit::Switch => {
            let mut on = state.selected();
            let r = Switch::new("", "", &mut on).show(ui);
            probe.record(&r);
        }
        Kit::Radio => {
            // one option; out of range means none is chosen
            let mut chosen = if state.selected() { 0 } else { 1 };
            let group = RadioGroup::new("", "", &[""], &mut chosen).show(ui);
            if let Some(item) = ui.ctx().read_response(group.id.with(0)) {
                probe.record(&item);
            }
        }
        Kit::Bar(fraction) => {
            let r = Progress::bar("", "", fraction).width(80.0).reduced_motion(true).show(ui);
            probe.record(&r);
        }
    });
}

// ---------------------------------------------------------------------------------------------
// References: literal colours (the built-in light and dark token values) and literal geometry.

struct Colors {
    accent: Color32,
    accent_hover: Color32,
    /// accent a quarter of the way to black
    accent_press: Color32,
    danger: Color32,
    /// danger an eighth and a quarter of the way to black
    danger_hover: Color32,
    danger_press: Color32,
    sunken: Color32,
    hover: Color32,
    pressed: Color32,
    border: Color32,
    border_strong: Color32,
    focus: Color32,
    text: Color32,
    text_secondary: Color32,
}

fn colors(theme: Theme) -> Colors {
    let c = Color32::from_rgb;
    match theme {
        Theme::Light => Colors {
            accent: c(37, 99, 235),
            accent_hover: c(29, 78, 216),
            accent_press: c(28, 74, 176),
            danger: c(220, 38, 38),
            danger_hover: c(193, 33, 33),
            danger_press: c(165, 29, 29),
            sunken: c(255, 255, 255),
            hover: c(226, 226, 230),
            pressed: c(208, 208, 214),
            border: c(200, 200, 207),
            border_strong: c(154, 154, 163),
            focus: c(37, 99, 235),
            text: c(24, 24, 27),
            text_secondary: c(82, 82, 91),
        },
        Theme::Dark => Colors {
            accent: c(59, 130, 246),
            accent_hover: c(96, 156, 248),
            accent_press: c(44, 98, 185),
            danger: c(248, 113, 113),
            danger_hover: c(217, 99, 99),
            danger_press: c(186, 85, 85),
            sunken: c(21, 21, 23),
            hover: c(48, 48, 53),
            pressed: c(60, 60, 66),
            border: c(58, 58, 64),
            border_strong: c(88, 88, 96),
            focus: c(124, 180, 255),
            text: c(228, 228, 231),
            text_secondary: c(161, 161, 170),
        },
    }
}

fn painter(ui: &Ui, state: State) -> Painter {
    let mut painter = ui.painter().clone();
    if state == State::Disabled {
        painter.multiply_opacity(0.4);
    }
    painter
}

/// Paint what `kit` in `state` should look like. `mutate` swaps the first fill for a wrong colour.
pub fn reference(ui: &mut Ui, kit: Kit, state: State, theme: Theme, mutate: bool) {
    let k = colors(theme);
    let p = painter(ui, state);
    let hovered = matches!(state, State::Hover);
    let pressed = matches!(state, State::Press | State::SelectedPress);
    let wrong = |c: Color32| if mutate { Color32::from_rgb(200, 60, 130) } else { c };
    let ring = Stroke::new(1.5, k.focus);
    match kit {
        Kit::Button(variant) => {
            // 40 × 24 at the origin, radius 5
            let rect = Rect::from_min_size(pos2(12.0, 12.0), vec2(40.0, 24.0));
            let r = CornerRadius::same(5);
            let (fill, stroke) = match (variant, hovered, pressed) {
                (Variant::Primary, _, true) => (k.accent_press, Stroke::new(1.0, k.accent_press)),
                (Variant::Primary, true, _) => (k.accent_hover, Stroke::new(1.0, k.accent_hover)),
                (Variant::Primary, ..) => (k.accent, Stroke::new(1.0, k.accent)),
                (Variant::Destructive, _, true) => (k.danger_press, Stroke::new(1.0, k.danger_press)),
                (Variant::Destructive, true, _) => (k.danger_hover, Stroke::new(1.0, k.danger_hover)),
                (Variant::Destructive, ..) => (k.danger, Stroke::new(1.0, k.danger)),
                (Variant::Secondary, _, true) => (k.pressed, Stroke::new(1.0, k.border_strong)),
                (Variant::Secondary, true, _) => (k.hover, Stroke::new(1.0, k.border_strong)),
                (Variant::Secondary, ..) => (k.sunken, Stroke::new(1.0, k.border)),
                (Variant::Ghost, _, true) => (k.pressed, Stroke::NONE),
                (Variant::Ghost, true, _) => (k.hover, Stroke::NONE),
                (Variant::Ghost, ..) => (Color32::TRANSPARENT, Stroke::NONE),
            };
            p.rect(rect, r, wrong(fill), stroke, StrokeKind::Inside);
            if state == State::Focus {
                p.rect_stroke(Rect::from_min_size(pos2(10.0, 10.0), vec2(44.0, 28.0)), r, ring, StrokeKind::Outside);
            }
        }
        Kit::Switch => {
            // a 32 × 18 track, radius 9; a knob of radius 6 at x 21 (off) or 35 (on)
            let track = Rect::from_min_size(pos2(12.0, 12.0), vec2(32.0, 18.0));
            let (fill, stroke, knob, x) = if state.selected() {
                let fill = if pressed { k.accent_press } else { k.accent };
                (fill, Stroke::new(1.0, fill), Color32::WHITE, 35.0)
            } else if pressed {
                (k.pressed, Stroke::new(1.0, k.border_strong), k.text, 21.0)
            } else if hovered {
                (k.hover, Stroke::new(1.0, k.border_strong), k.text, 21.0)
            } else {
                (k.sunken, Stroke::new(1.0, k.border), k.text_secondary, 21.0)
            };
            p.rect(track, CornerRadius::same(9), wrong(fill), stroke, StrokeKind::Inside);
            p.circle_filled(pos2(x, 21.0), 6.0, knob);
            if state == State::Focus {
                p.rect_stroke(Rect::from_min_size(pos2(10.0, 10.0), vec2(36.0, 22.0)), CornerRadius::same(11), ring, StrokeKind::Outside);
            }
        }
        Kit::Radio => {
            // a circle of radius 7 centred at (19, 24); a dot of radius 3 when chosen
            let center = pos2(19.0, 24.0);
            if state.selected() {
                let fill = if pressed || hovered { k.accent_hover } else { k.accent };
                p.circle(center, 7.0, wrong(fill), Stroke::new(1.0, fill));
                p.circle_filled(center, 3.0, Color32::WHITE);
            } else {
                let (fill, border) = if pressed {
                    (k.pressed, k.border_strong)
                } else if hovered {
                    (k.hover, k.border_strong)
                } else {
                    (k.sunken, k.border)
                };
                p.circle(center, 7.0, wrong(fill), Stroke::new(1.0, border));
            }
            if state == State::Focus {
                p.circle_stroke(center, 9.0, ring);
            }
        }
        Kit::Bar(fraction) => {
            // an 80 × 6 track, radius 3
            let track = Rect::from_min_size(pos2(12.0, 12.0), vec2(80.0, 6.0));
            let r = CornerRadius::same(3);
            p.rect(track, r, wrong(k.sunken), Stroke::new(1.0, k.border), StrokeKind::Inside);
            let (a, b) = match fraction {
                Some(f) => (12.0, 12.0 + 80.0 * f),
                // the static pose: 35 % to 65 %
                None => (12.0 + 80.0 * 0.35, 12.0 + 80.0 * 0.65),
            };
            p.rect_filled(Rect::from_min_max(pos2(a, 12.0), pos2(b, 18.0)), r, k.accent);
        }
    }
}
