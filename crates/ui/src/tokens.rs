//! Design tokens: the colours, radii, spacing and type sizes every ashlar component reads.
//!
//! An app picks (or builds) one [`Tokens`] value per egui theme, installs them with [`apply`] or
//! [`install`], and chooses [`ThemeMode`] (light, dark or follow the system). Components read the
//! active set with [`current`], so a theme switch restyles everything on the next frame.
//!
//! Apps extend the shared tokens with their own domain colours (a timeline's playhead, a meter's
//! clip colour) by storing a token struct of their own next to them:
//!
//! ```
//! # use ashlar_ui::tokens;
//! #[derive(Clone, Copy)]
//! struct EditorTokens { playhead: egui::Color32 }
//!
//! let ctx = egui::Context::default();
//! tokens::install(&ctx, &tokens::Tokens::light(), &tokens::Tokens::dark(), tokens::ThemeMode::System);
//! tokens::set_extension(&ctx, egui::Theme::Dark, EditorTokens { playhead: egui::Color32::from_rgb(88, 149, 236) });
//! tokens::set_extension(&ctx, egui::Theme::Light, EditorTokens { playhead: egui::Color32::from_rgb(30, 90, 200) });
//! let _playhead = tokens::extension::<EditorTokens>(&ctx).map(|t| t.playhead);
//! ```
//!
//! [`apply`] writes egui's own `Visuals`, spacing and text styles from the tokens (through
//! [`crate::theme::WidgetPalette`] for the widget states), so stock egui widgets match too.

use ashlar_motion::Pair;
use ashlar_motion::color::{self, Rgba};
use ashlar_motion::tokens as motion_tokens;
use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle, Theme, Vec2};

use crate::theme::WidgetPalette;

/// Light, dark, or whatever the operating system uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    Light,
    Dark,
    #[default]
    System,
}

impl ThemeMode {
    fn preference(self) -> egui::ThemePreference {
        match self {
            ThemeMode::Light => egui::ThemePreference::Light,
            ThemeMode::Dark => egui::ThemePreference::Dark,
            ThemeMode::System => egui::ThemePreference::System,
        }
    }
}

/// Background colours, from the window behind everything to the pressed state of a control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Surfaces {
    /// Behind and between panels.
    pub app: Color32,
    /// Panel bodies.
    pub panel: Color32,
    /// Windows, popups, menus, dialogs, toasts.
    pub raised: Color32,
    /// Text fields, list backgrounds, wells.
    pub sunken: Color32,
    /// Alternate rows of lists and tables.
    pub row_alt: Color32,
    /// A hovered control or row.
    pub hover: Color32,
    /// A pressed control.
    pub pressed: Color32,
}

/// Text colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextColors {
    pub primary: Color32,
    /// Labels, captions, secondary information.
    pub secondary: Color32,
    /// Disabled controls and placeholders.
    pub disabled: Color32,
    /// Text on an accent fill (primary buttons, selected rows).
    pub on_accent: Color32,
}

/// Status colours for messages, validation and toasts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StatusColors {
    pub info: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub danger: Color32,
}

/// Corner radii in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Radii {
    /// Small controls: checkboxes, chips, swatches.
    pub sm: u8,
    /// Buttons, fields, rows.
    pub md: u8,
    /// Windows, dialogs, popups.
    pub lg: u8,
}

/// The spacing scale in points; components use these instead of literals.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpaceScale {
    pub xxs: f32,
    pub xs: f32,
    pub sm: f32,
    pub md: f32,
    pub lg: f32,
    pub xl: f32,
}

/// The type scale in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypeScale {
    pub small: f32,
    pub body: f32,
    pub button: f32,
    pub heading: f32,
    pub title: f32,
    pub mono: f32,
}

/// One complete set of design tokens (one per egui theme).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    /// Which egui theme this set styles.
    pub dark: bool,
    pub surface: Surfaces,
    pub text: TextColors,
    pub status: StatusColors,
    /// Primary buttons, active toggles, slider fills, links.
    pub accent: Color32,
    pub accent_hover: Color32,
    /// Background of selected rows, text and items.
    pub selection: Color32,
    /// Text on a selection.
    pub selection_text: Color32,
    /// Keyboard focus outline.
    pub focus: Color32,
    /// Field and control borders.
    pub border: Color32,
    /// Borders of hovered or emphasised controls.
    pub border_strong: Color32,
    /// Divider lines between regions.
    pub separator: Color32,
    pub radius: Radii,
    pub space: SpaceScale,
    pub type_scale: TypeScale,
    /// Height of one-line controls (fields, buttons, list rows).
    pub control_height: f32,
    /// Which spring each kind of change uses.
    pub motion: MotionTokens,
    /// Shadows of floating surfaces.
    pub elevation: Elevation,
    /// Line widths.
    pub stroke: Strokes,
}

impl Default for Tokens {
    fn default() -> Self {
        Self::dark()
    }
}

/// The spring each kind of change uses, from [`ashlar_motion::tokens`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionTokens {
    /// A control being pressed.
    pub press: Pair,
    /// Hover highlights.
    pub hover: Pair,
    /// State changes: checked, selected, expanded.
    pub state: Pair,
    /// Layout moving: rows reordering, panels resizing.
    pub reflow: Pair,
    /// Surfaces moving: sheets, drawers, scrolling to a target.
    pub surface: Pair,
    /// Things appearing and disappearing: popups, toasts, dialogs.
    pub enter_exit: Pair,
}

/// The shadows of floating surfaces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Elevation {
    /// Menus, combo boxes and tooltips.
    pub popup: Shadow,
    /// Windows and dialogs.
    pub window: Shadow,
}

/// Line widths in points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strokes {
    /// Borders, separators and outlines.
    pub hairline: f32,
    /// Focus rings and the text cursor.
    pub focus: f32,
}

const MOTION: MotionTokens = MotionTokens {
    press: motion_tokens::INSTANT,
    hover: motion_tokens::SNAP,
    state: motion_tokens::SETTLE,
    reflow: motion_tokens::REFLOW,
    surface: motion_tokens::GLIDE,
    enter_exit: motion_tokens::DEFAULT,
};

const STROKES: Strokes = Strokes { hairline: 1.0, focus: 2.0 };

const fn elevation(shadow_alpha: u8) -> Elevation {
    Elevation {
        popup: Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(shadow_alpha) },
        window: Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(shadow_alpha) },
    }
}

/// Smallest WCAG contrast ratio for body text.
pub const MIN_TEXT_CONTRAST: f32 = 4.5;
/// Smallest WCAG contrast ratio for secondary text, focus rings and strong borders.
pub const MIN_UI_CONTRAST: f32 = 3.0;

/// The OKLCH source `(name, lightness, chroma, hue°)` of every colour in [`Tokens::dark`]
/// (`dark.*`) and [`Tokens::light`] (`light.*`). Those are `const fn`s and keep hex values (OKLCH
/// to sRGB needs `cbrt`, which isn't const); a test recomputes each hex from this table.
pub const TOKEN_SPEC: &[(&str, f32, f32, f32)] = &[
    ("dark.surface.app", 0.1831, 0.004, 286.0),       // #121214
    ("dark.surface.panel", 0.2363, 0.0057, 286.0),    // #1e1e21
    ("dark.surface.raised", 0.2785, 0.0073, 285.9),   // #28282c
    ("dark.surface.sunken", 0.1966, 0.004, 286.0),    // #151517
    ("dark.surface.row_alt", 0.2532, 0.0056, 286.0),  // #222225
    ("dark.surface.hover", 0.3111, 0.0089, 285.9),    // #303035
    ("dark.surface.pressed", 0.3584, 0.0103, 285.9),  // #3c3c42
    ("dark.text.primary", 0.9197, 0.004, 286.3),      // #e4e4e7
    ("dark.text.secondary", 0.7118, 0.0129, 286.1),   // #a1a1aa
    ("dark.text.disabled", 0.5025, 0.0126, 285.9),    // #63636b
    ("dark.text.on_accent", 1.0, 0.0, 0.0),           // #ffffff
    ("dark.status.info", 0.7137, 0.1434, 254.6),      // #60a5fa
    ("dark.status.success", 0.8003, 0.1821, 151.7),   // #4ade80
    ("dark.status.warning", 0.8369, 0.1644, 84.4),    // #fbbf24
    ("dark.status.danger", 0.7106, 0.1661, 22.2),     // #f87171
    ("dark.accent", 0.5601, 0.1878, 259.8),           // #276ee0
    ("dark.accent_hover", 0.6292, 0.1878, 259.8),     // #3d84f8
    ("dark.selection", 0.4404, 0.1358, 259.9),        // #204f9c
    ("dark.selection_text", 1.0, 0.0, 0.0),           // #ffffff
    ("dark.focus", 0.7606, 0.1228, 259.6),            // #82b2ff
    ("dark.border", 0.3507, 0.0103, 285.9),           // #3a3a40
    ("dark.border_strong", 0.4632, 0.0128, 285.9),    // #585860
    ("dark.separator", 0.3032, 0.0089, 285.9),        // #2e2e33
    ("light.surface.app", 0.9197, 0.004, 286.3),      // #e4e4e7
    ("light.surface.panel", 0.9674, 0.0013, 286.4),   // #f4f4f5
    ("light.surface.raised", 1.0, 0.0, 0.0),          // #ffffff
    ("light.surface.sunken", 1.0, 0.0, 0.0),          // #ffffff
    ("light.surface.row_alt", 0.9439, 0.004, 286.3),  // #ececef
    ("light.surface.hover", 0.9139, 0.0054, 286.3),   // #e2e2e6
    ("light.surface.pressed", 0.8594, 0.0082, 286.2), // #d0d0d6
    ("light.text.primary", 0.2103, 0.0059, 285.9),    // #18181b
    ("light.text.secondary", 0.4419, 0.0146, 285.8),  // #52525b
    ("light.text.disabled", 0.7118, 0.0129, 286.1),   // #a1a1aa
    ("light.text.on_accent", 1.0, 0.0, 0.0),          // #ffffff
    ("light.status.info", 0.5461, 0.2152, 262.9),     // #2563eb
    ("light.status.success", 0.6271, 0.1699, 149.2),  // #16a34a
    ("light.status.warning", 0.5553, 0.1455, 49.0),   // #b45309
    ("light.status.danger", 0.5771, 0.2152, 27.3),    // #dc2626
    ("light.accent", 0.5461, 0.2152, 262.9),          // #2563eb
    ("light.accent_hover", 0.4858, 0.2153, 262.9),    // #124fd6
    ("light.selection", 0.8756, 0.0592, 261.9),       // #c1d7fe
    ("light.selection_text", 0.2504, 0.0791, 262.9),  // #0c1f47
    ("light.focus", 0.5461, 0.2152, 262.9),           // #2563eb
    ("light.border", 0.8349, 0.0097, 286.2),          // #c8c8cf
    ("light.border_strong", 0.6891, 0.013, 286.1),    // #9a9aa3
    ("light.separator", 0.8868, 0.0068, 286.3),       // #d9d9de
];

const SPACE: SpaceScale = SpaceScale { xxs: 2.0, xs: 4.0, sm: 6.0, md: 8.0, lg: 12.0, xl: 16.0 };
const TYPE: TypeScale = TypeScale { small: 11.0, body: 13.0, button: 13.0, heading: 15.0, title: 18.0, mono: 12.5 };
const RADII: Radii = Radii { sm: 3, md: 5, lg: 8 };

impl Tokens {
    /// The neutral dark set.
    pub const fn dark() -> Self {
        Tokens {
            dark: true,
            surface: Surfaces {
                app: Color32::from_rgb(0x12, 0x12, 0x14),
                panel: Color32::from_rgb(0x1e, 0x1e, 0x21),
                raised: Color32::from_rgb(0x28, 0x28, 0x2c),
                sunken: Color32::from_rgb(0x15, 0x15, 0x17),
                row_alt: Color32::from_rgb(0x22, 0x22, 0x25),
                hover: Color32::from_rgb(0x30, 0x30, 0x35),
                pressed: Color32::from_rgb(0x3c, 0x3c, 0x42),
            },
            text: TextColors {
                primary: Color32::from_rgb(0xe4, 0xe4, 0xe7),
                secondary: Color32::from_rgb(0xa1, 0xa1, 0xaa),
                disabled: Color32::from_rgb(0x63, 0x63, 0x6b),
                on_accent: Color32::WHITE,
            },
            status: StatusColors {
                info: Color32::from_rgb(0x60, 0xa5, 0xfa),
                success: Color32::from_rgb(0x4a, 0xde, 0x80),
                warning: Color32::from_rgb(0xfb, 0xbf, 0x24),
                danger: Color32::from_rgb(0xf8, 0x71, 0x71),
            },
            accent: Color32::from_rgb(0x27, 0x6e, 0xe0),
            accent_hover: Color32::from_rgb(0x3d, 0x84, 0xf8),
            selection: Color32::from_rgb(0x20, 0x4f, 0x9c),
            selection_text: Color32::WHITE,
            focus: Color32::from_rgb(0x82, 0xb2, 0xff),
            border: Color32::from_rgb(0x3a, 0x3a, 0x40),
            border_strong: Color32::from_rgb(0x58, 0x58, 0x60),
            separator: Color32::from_rgb(0x2e, 0x2e, 0x33),
            radius: RADII,
            space: SPACE,
            type_scale: TYPE,
            control_height: 24.0,
            motion: MOTION,
            elevation: elevation(140),
            stroke: STROKES,
        }
    }

    /// The neutral light set.
    pub const fn light() -> Self {
        Tokens {
            dark: false,
            surface: Surfaces {
                app: Color32::from_rgb(0xe4, 0xe4, 0xe7),
                panel: Color32::from_rgb(0xf4, 0xf4, 0xf5),
                raised: Color32::WHITE,
                sunken: Color32::WHITE,
                row_alt: Color32::from_rgb(0xec, 0xec, 0xef),
                hover: Color32::from_rgb(0xe2, 0xe2, 0xe6),
                pressed: Color32::from_rgb(0xd0, 0xd0, 0xd6),
            },
            text: TextColors {
                primary: Color32::from_rgb(0x18, 0x18, 0x1b),
                secondary: Color32::from_rgb(0x52, 0x52, 0x5b),
                disabled: Color32::from_rgb(0xa1, 0xa1, 0xaa),
                on_accent: Color32::WHITE,
            },
            status: StatusColors {
                info: Color32::from_rgb(0x25, 0x63, 0xeb),
                success: Color32::from_rgb(0x16, 0xa3, 0x4a),
                warning: Color32::from_rgb(0xb4, 0x53, 0x09),
                danger: Color32::from_rgb(0xdc, 0x26, 0x26),
            },
            accent: Color32::from_rgb(0x25, 0x63, 0xeb),
            accent_hover: Color32::from_rgb(0x12, 0x4f, 0xd6),
            selection: Color32::from_rgb(0xc1, 0xd7, 0xfe),
            selection_text: Color32::from_rgb(0x0c, 0x1f, 0x47),
            focus: Color32::from_rgb(0x25, 0x63, 0xeb),
            border: Color32::from_rgb(0xc8, 0xc8, 0xcf),
            border_strong: Color32::from_rgb(0x9a, 0x9a, 0xa3),
            separator: Color32::from_rgb(0xd9, 0xd9, 0xde),
            radius: RADII,
            space: SPACE,
            type_scale: TYPE,
            control_height: 24.0,
            motion: MOTION,
            elevation: elevation(40),
            stroke: STROKES,
        }
    }

    /// The default set for an egui theme.
    pub const fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Dark => Self::dark(),
            Theme::Light => Self::light(),
        }
    }

    /// The egui theme this set styles.
    pub const fn theme(&self) -> Theme {
        if self.dark { Theme::Dark } else { Theme::Light }
    }

    /// Replace the accent and derive its hover shade, focus ring, selection and the text on it.
    ///
    /// The derived colours are computed in OKLCH: they keep the accent's hue, move only
    /// lightness and cap chroma, so a hover never drifts towards another hue the way sRGB mixing
    /// does. `text.on_accent` stays white while white reads at 4.5:1 on the accent and turns dark
    /// otherwise; the focus ring is pushed until it reaches 3:1 against the surfaces.
    pub fn with_accent(mut self, accent: Color32) -> Self {
        let [r, g, b, _] = accent.to_srgba_unmultiplied();
        let accent = Color32::from_rgb(r, g, b);
        let [l, c, h] = oklch_of(accent);
        self.accent = accent;
        self.text.on_accent = readable_on(accent);
        if self.dark {
            self.accent_hover = from_oklch(l + 0.07, c, h);
            self.focus = from_oklch((l + 0.2).max(0.76), c.min(0.13), h);
            self.selection = from_oklch(0.44, c.min(0.136), h);
            self.selection_text = Color32::WHITE;
        } else {
            self.accent_hover = from_oklch(l - 0.06, c, h);
            self.focus = accent;
            self.selection = from_oklch(0.875, c.min(0.06), h);
            self.selection_text = from_oklch(0.25, c.min(0.08), h);
        }
        let s = self.surface;
        self.focus = push_from(self.focus, &[s.app, s.panel, s.raised, s.sunken], MIN_UI_CONTRAST);
        self
    }

    /// A higher-contrast version of this set, for the OS "increase contrast" setting: secondary
    /// and disabled text move towards primary, borders get stronger. Lightness steps in OKLCH,
    /// hues unchanged.
    pub fn high_contrast(mut self) -> Self {
        let [lp, _, _] = oklch_of(self.text.primary);
        let toward = |c: Color32, t: f32| {
            let [l, ch, h] = oklch_of(c);
            from_oklch(l + (lp - l) * t, ch, h)
        };
        self.text.secondary = toward(self.text.secondary, 0.5);
        self.text.disabled = toward(self.text.disabled, 0.35);
        let s = self.surface;
        let surfaces = [s.app, s.panel, s.raised, s.sunken, s.row_alt, s.hover, s.pressed];
        self.text.secondary = push_from(self.text.secondary, &surfaces, MIN_TEXT_CONTRAST);
        self.separator = self.border;
        self.border = self.border_strong;
        let step = if self.dark { 0.15 } else { -0.15 };
        let [l, ch, h] = oklch_of(self.border_strong);
        self.border_strong = push_from(from_oklch(l + step, ch, h), &[s.app, s.panel, s.raised, s.sunken], MIN_UI_CONTRAST);
        self.focus = push_from(self.focus, &[s.app, s.panel, s.raised, s.sunken], MIN_TEXT_CONTRAST);
        self
    }

    /// Proportional font at a type-scale size.
    pub fn font(size: f32) -> FontId {
        FontId::new(size, FontFamily::Proportional)
    }

    /// Monospace font at the scale's mono size.
    pub fn mono(&self) -> FontId {
        FontId::new(self.type_scale.mono, FontFamily::Monospace)
    }

    /// Fill for a state of an interactive surface.
    pub fn interact_fill(&self, response: &egui::Response) -> Color32 {
        if response.is_pointer_button_down_on() {
            self.surface.pressed
        } else if response.hovered() {
            self.surface.hover
        } else {
            self.surface.sunken
        }
    }

    /// egui `Visuals` built from these tokens.
    pub fn visuals(&self) -> egui::Visuals {
        let mut v = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        let s = &self.surface;
        let hair = self.stroke.hairline;
        v.panel_fill = s.panel;
        v.window_fill = s.raised;
        v.extreme_bg_color = s.sunken;
        v.text_edit_bg_color = Some(s.sunken);
        v.faint_bg_color = s.row_alt;
        v.code_bg_color = s.sunken;
        v.override_text_color = Some(self.text.primary);
        v.weak_text_color = Some(self.text.secondary);
        v.hyperlink_color = self.accent;
        v.warn_fg_color = self.status.warning;
        v.error_fg_color = self.status.danger;
        v.selection.bg_fill = self.selection;
        v.selection.stroke = Stroke::new(hair, self.selection_text);
        v.window_stroke = Stroke::new(hair, self.border);
        v.window_corner_radius = CornerRadius::same(self.radius.lg);
        v.menu_corner_radius = CornerRadius::same(self.radius.md);
        v.popup_shadow = self.elevation.popup;
        v.window_shadow = self.elevation.window;
        WidgetPalette { noninteractive: s.panel, inactive: s.sunken, hovered: s.hover, active: s.pressed, open: s.hover }.apply_to(
            &mut v.widgets,
            CornerRadius::same(self.radius.md),
            Stroke::new(hair, self.text.primary),
        );
        v.widgets.noninteractive.bg_stroke = Stroke::new(hair, self.separator);
        v.widgets.noninteractive.fg_stroke = Stroke::new(hair, self.text.primary);
        v.widgets.inactive.bg_stroke = Stroke::new(hair, self.border);
        v.widgets.hovered.bg_stroke = Stroke::new(hair, self.border_strong);
        v.widgets.active.bg_stroke = Stroke::new(hair, self.focus);
        v.widgets.open.bg_stroke = Stroke::new(hair, self.border_strong);
        v.text_cursor.stroke = Stroke::new(self.stroke.focus, self.accent);
        v
    }

    /// Write spacing and text styles into an egui `Style`.
    pub fn style_into(&self, style: &mut egui::Style) {
        let sp = &self.space;
        style.spacing.item_spacing = Vec2::new(sp.md, sp.xs);
        style.spacing.button_padding = Vec2::new(sp.md, sp.xxs + 1.0);
        style.spacing.interact_size.y = self.control_height;
        style.spacing.menu_margin = Margin::same(sp.xs as i8);
        style.spacing.window_margin = Margin::same(sp.lg as i8);
        style.spacing.indent = sp.xl;
        let t = &self.type_scale;
        style.text_styles.insert(TextStyle::Small, Self::font(t.small));
        style.text_styles.insert(TextStyle::Body, Self::font(t.body));
        style.text_styles.insert(TextStyle::Button, Self::font(t.button));
        style.text_styles.insert(TextStyle::Heading, Self::font(t.heading));
        style.text_styles.insert(TextStyle::Monospace, FontId::new(t.mono, FontFamily::Monospace));
        style.visuals = self.visuals();
    }
}

/// WCAG 2 contrast ratio of `fg` on `bg` (1..=21); a translucent `fg` is composited over `bg`.
pub fn contrast_ratio(fg: Color32, bg: Color32) -> f32 {
    color::contrast_ratio(rgba(fg), rgba(bg))
}

fn rgba(c: Color32) -> Rgba {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    Rgba::from_srgb8(r, g, b, a)
}

fn oklch_of(c: Color32) -> [f32; 3] {
    color::to_oklch(rgba(c))
}

/// An opaque colour from OKLCH; out-of-gamut chroma is reduced, lightness and hue kept.
fn from_oklch(l: f32, c: f32, h: f32) -> Color32 {
    let [r, g, b, _] = color::oklch(l, c, h).to_srgb8();
    Color32::from_rgb(r, g, b)
}

/// White if it reads as body text on `bg`, else the darkest ink that does.
fn readable_on(bg: Color32) -> Color32 {
    [Color32::WHITE, Color32::from_rgb(0x18, 0x18, 0x1b)]
        .into_iter()
        .find(|&ink| contrast_ratio(ink, bg) >= MIN_TEXT_CONTRAST)
        .unwrap_or(Color32::BLACK)
}

/// `c` with its OKLCH lightness pushed away from `surfaces` until it reaches `min` contrast on
/// every one of them (or lightness runs out); hue and chroma are kept.
fn push_from(c: Color32, surfaces: &[Color32], min: f32) -> Color32 {
    let worst = |c: Color32| surfaces.iter().map(|&s| contrast_ratio(c, s)).fold(f32::INFINITY, f32::min);
    if worst(c) >= min || surfaces.is_empty() {
        return c;
    }
    let [l, ch, h] = oklch_of(c);
    let mean = surfaces.iter().map(|&s| oklch_of(s)[0]).sum::<f32>() / surfaces.len() as f32;
    let dir = if l >= mean { 0.02 } else { -0.02 };
    let mut best = c;
    for i in 1..=50 {
        let next = from_oklch(l + dir * i as f32, ch, h);
        best = next;
        if worst(next) >= min {
            break;
        }
    }
    best
}

fn tokens_id(theme: Theme) -> egui::Id {
    egui::Id::new(("ashlar_ui::tokens", theme))
}

fn extension_id<T: 'static>(theme: Theme) -> egui::Id {
    egui::Id::new(("ashlar_ui::tokens::extension", theme, std::any::TypeId::of::<T>()))
}

/// Install `tokens` as the style of its egui theme (`tokens.dark`) and remember them for
/// [`current`]. Does not change which theme is active; see [`set_mode`].
pub fn apply(ctx: &egui::Context, tokens: &Tokens) {
    let theme = tokens.theme();
    ctx.style_mut_of(theme, |style| tokens.style_into(style));
    ctx.data_mut(|d| d.insert_temp(tokens_id(theme), *tokens));
}

/// Install a light and a dark set and choose the mode, in one call.
pub fn install(ctx: &egui::Context, light: &Tokens, dark: &Tokens, mode: ThemeMode) {
    apply(ctx, &Tokens { dark: false, ..*light });
    apply(ctx, &Tokens { dark: true, ..*dark });
    set_mode(ctx, mode);
}

/// Follow light, dark, or the operating system's setting.
pub fn set_mode(ctx: &egui::Context, mode: ThemeMode) {
    ctx.set_theme(mode.preference());
}

/// The mode last chosen with [`set_mode`].
pub fn mode(ctx: &egui::Context) -> ThemeMode {
    match ctx.options(|o| o.theme_preference) {
        egui::ThemePreference::Light => ThemeMode::Light,
        egui::ThemePreference::Dark => ThemeMode::Dark,
        egui::ThemePreference::System => ThemeMode::System,
    }
}

/// Interface scale bounds accepted by [`set_interface_scale`].
pub const INTERFACE_SCALE: std::ops::RangeInclusive<f32> = 0.5..=4.0;

/// Scale the whole interface (on top of the display's own pixels-per-point). Values outside
/// [`INTERFACE_SCALE`] are clamped; non-finite values are ignored.
pub fn set_interface_scale(ctx: &egui::Context, scale: f32) {
    if scale.is_finite() {
        ctx.set_zoom_factor(scale.clamp(*INTERFACE_SCALE.start(), *INTERFACE_SCALE.end()));
    }
}

/// The tokens of the active theme: those installed with [`apply`], else the built-in set.
pub fn current(ctx: &egui::Context) -> Tokens {
    let theme = ctx.theme();
    ctx.data(|d| d.get_temp::<Tokens>(tokens_id(theme))).unwrap_or(Tokens::for_theme(theme))
}

/// Store an app's own token struct for one theme.
pub fn set_extension<T: Clone + Send + Sync + 'static>(ctx: &egui::Context, theme: Theme, value: T) {
    ctx.data_mut(|d| d.insert_temp(extension_id::<T>(theme), value));
}

/// The app token struct `T` for the active theme, if one was stored.
pub fn extension<T: Clone + Send + Sync + 'static>(ctx: &egui::Context) -> Option<T> {
    let theme = ctx.theme();
    ctx.data(|d| d.get_temp::<T>(extension_id::<T>(theme)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_styles_its_own_theme_and_current_follows_mode() {
        let ctx = egui::Context::default();
        let dark = Tokens::dark().with_accent(Color32::from_rgb(200, 40, 120));
        install(&ctx, &Tokens::light(), &dark, ThemeMode::Dark);
        assert_eq!(current(&ctx).accent, Color32::from_rgb(200, 40, 120));
        assert_eq!(ctx.style_of(Theme::Dark).visuals.hyperlink_color, Color32::from_rgb(200, 40, 120));
        assert_eq!(ctx.style_of(Theme::Light).visuals.panel_fill, Tokens::light().surface.panel);
        set_mode(&ctx, ThemeMode::Light);
        assert_eq!(mode(&ctx), ThemeMode::Light);
        assert_eq!(current(&ctx), Tokens::light());
        assert_eq!(ctx.global_style().visuals.panel_fill, Tokens::light().surface.panel);
        assert_eq!(ctx.global_style().spacing.interact_size.y, Tokens::light().control_height);
        assert_eq!(ctx.global_style().text_styles.get(&TextStyle::Body), Some(&Tokens::font(13.0)));
    }

    #[test]
    fn install_forces_each_set_onto_its_theme() {
        let ctx = egui::Context::default();
        // a caller passing the same set twice still gets a light and a dark style
        install(&ctx, &Tokens::dark(), &Tokens::dark(), ThemeMode::Light);
        assert!(!current(&ctx).dark);
        assert!(!ctx.style_of(Theme::Light).visuals.dark_mode);
        assert!(ctx.style_of(Theme::Dark).visuals.dark_mode);
    }

    #[test]
    fn system_mode_follows_the_reported_os_theme() {
        let ctx = egui::Context::default();
        install(&ctx, &Tokens::light(), &Tokens::dark(), ThemeMode::System);
        for (os, dark) in [(Theme::Light, false), (Theme::Dark, true)] {
            let mut out = ctx.run_ui(egui::RawInput { system_theme: Some(os), ..Default::default() }, |_| {});
            out.textures_delta.clear();
            assert_eq!(current(&ctx).dark, dark);
        }
    }

    #[test]
    fn extensions_are_per_theme_and_per_type() {
        #[derive(Clone, Copy, PartialEq, Debug)]
        struct Meter(Color32);
        let ctx = egui::Context::default();
        set_mode(&ctx, ThemeMode::Dark);
        assert_eq!(extension::<Meter>(&ctx), None);
        set_extension(&ctx, Theme::Dark, Meter(Color32::RED));
        set_extension(&ctx, Theme::Light, Meter(Color32::GREEN));
        assert_eq!(extension::<Meter>(&ctx), Some(Meter(Color32::RED)));
        set_mode(&ctx, ThemeMode::Light);
        assert_eq!(extension::<Meter>(&ctx), Some(Meter(Color32::GREEN)));
    }

    #[test]
    fn interface_scale_is_bounded() {
        let ctx = egui::Context::default();
        // the zoom factor takes effect at the start of the next pass
        let scaled = |scale: f32| {
            set_interface_scale(&ctx, scale);
            let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
            out.textures_delta.clear();
            ctx.zoom_factor()
        };
        assert_eq!(scaled(1.5), 1.5);
        assert_eq!(scaled(f32::NAN), 1.5);
        assert_eq!(scaled(100.0), 4.0);
        assert_eq!(scaled(0.0), 0.5);
    }

    #[test]
    fn accent_derivations_stay_in_range() {
        for base in [Tokens::light(), Tokens::dark()] {
            let t = base.with_accent(Color32::from_rgb(255, 255, 0));
            assert_eq!(t.accent, Color32::from_rgb(255, 255, 0));
            assert_ne!(t.selection, base.selection);
        }
        assert_eq!(from_oklch(f32::NAN, f32::NAN, f32::NAN), Color32::BLACK);
        assert_eq!(from_oklch(2.0, 0.0, 0.0), Color32::WHITE);
        // a translucent accent is used opaque
        assert!(Tokens::dark().with_accent(Color32::from_rgba_unmultiplied(200, 40, 120, 10)).accent.is_opaque());
    }

    fn named(t: &Tokens) -> Vec<(&'static str, Color32)> {
        let s = t.surface;
        vec![
            ("surface.app", s.app),
            ("surface.panel", s.panel),
            ("surface.raised", s.raised),
            ("surface.sunken", s.sunken),
            ("surface.row_alt", s.row_alt),
            ("surface.hover", s.hover),
            ("surface.pressed", s.pressed),
            ("text.primary", t.text.primary),
            ("text.secondary", t.text.secondary),
            ("text.disabled", t.text.disabled),
            ("text.on_accent", t.text.on_accent),
            ("status.info", t.status.info),
            ("status.success", t.status.success),
            ("status.warning", t.status.warning),
            ("status.danger", t.status.danger),
            ("accent", t.accent),
            ("accent_hover", t.accent_hover),
            ("selection", t.selection),
            ("selection_text", t.selection_text),
            ("focus", t.focus),
            ("border", t.border),
            ("border_strong", t.border_strong),
            ("separator", t.separator),
        ]
    }

    fn max_channel_diff(a: Color32, b: Color32) -> u8 {
        [a.r().abs_diff(b.r()), a.g().abs_diff(b.g()), a.b().abs_diff(b.b())].into_iter().max().unwrap_or(0)
    }

    fn surfaces(t: &Tokens) -> [Color32; 7] {
        let s = t.surface;
        [s.app, s.panel, s.raised, s.sunken, s.row_alt, s.hover, s.pressed]
    }

    /// 64 accents from a fixed-seed generator, so failures reproduce.
    fn random_accents() -> Vec<Color32> {
        let mut seed: u32 = 0x9e37_79b9;
        (0..64)
            .map(|_| {
                let mut next = || {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed >> 24) as u8
                };
                Color32::from_rgb(next(), next(), next())
            })
            .collect()
    }

    #[test]
    fn every_hex_is_its_documented_oklch() {
        let mut seen = 0;
        for (theme, t) in [("dark", Tokens::dark()), ("light", Tokens::light())] {
            for (name, hex) in named(&t) {
                let key = format!("{theme}.{name}");
                let Some(&(_, l, c, h)) = TOKEN_SPEC.iter().find(|(n, ..)| *n == key) else {
                    panic!("{key} has no OKLCH spec");
                };
                let from_spec = from_oklch(l, c, h);
                assert!(max_channel_diff(from_spec, hex) <= 1, "{key}: spec gives {from_spec:?}, hex is {hex:?}");
                seen += 1;
            }
        }
        assert_eq!(seen, TOKEN_SPEC.len(), "the spec lists a colour no token has");
    }

    #[test]
    fn built_in_accent_states_are_the_oklch_derivation() {
        for t in [Tokens::dark(), Tokens::light()] {
            let d = t.with_accent(t.accent);
            for (name, want, got) in [
                ("accent_hover", t.accent_hover, d.accent_hover),
                ("focus", t.focus, d.focus),
                ("selection", t.selection, d.selection),
                ("selection_text", t.selection_text, d.selection_text),
                ("on_accent", t.text.on_accent, d.text.on_accent),
            ] {
                assert!(max_channel_diff(want, got) <= 1, "dark={} {name}: {want:?} vs derived {got:?}", t.dark);
            }
        }
    }

    fn assert_contrast_gate(t: &Tokens, label: &str) {
        for bg in surfaces(t) {
            let p = contrast_ratio(t.text.primary, bg);
            assert!(p >= MIN_TEXT_CONTRAST, "{label}: primary on {bg:?} is {p:.2}");
            let s = contrast_ratio(t.text.secondary, bg);
            assert!(s >= MIN_UI_CONTRAST, "{label}: secondary on {bg:?} is {s:.2}");
        }
        let a = contrast_ratio(t.text.on_accent, t.accent);
        assert!(a >= MIN_TEXT_CONTRAST, "{label}: on_accent on {:?} is {a:.2}", t.accent);
        let sel = contrast_ratio(t.selection_text, t.selection);
        assert!(sel >= MIN_TEXT_CONTRAST, "{label}: selection text is {sel:.2}");
        let s = t.surface;
        for bg in [s.app, s.panel, s.raised, s.sunken] {
            let f = contrast_ratio(t.focus, bg);
            assert!(f >= MIN_UI_CONTRAST, "{label}: focus on {bg:?} is {f:.2}");
        }
    }

    #[test]
    fn contrast_gate_holds_for_both_themes_and_random_accents() {
        for t in [Tokens::dark(), Tokens::light()] {
            assert_contrast_gate(&t, &format!("dark={}", t.dark));
            assert_contrast_gate(&t.high_contrast(), &format!("high contrast dark={}", t.dark));
            for accent in random_accents() {
                let d = t.with_accent(accent);
                assert_contrast_gate(&d, &format!("dark={} accent {accent:?}", t.dark));
                assert_contrast_gate(&d.high_contrast(), &format!("high contrast dark={} accent {accent:?}", t.dark));
            }
        }
        // a light accent flips the text on it to dark
        let yellow = Tokens::dark().with_accent(Color32::from_rgb(255, 230, 0));
        assert_ne!(yellow.text.on_accent, Color32::WHITE);
    }

    #[test]
    fn derived_hover_keeps_the_accent_hue() {
        let mut checked = 0;
        for base in [Tokens::dark(), Tokens::light()] {
            for accent in random_accents().into_iter().chain([base.accent, Color32::from_rgb(200, 40, 120)]) {
                let [_, c, h] = oklch_of(accent);
                // below this chroma 8-bit rounding alone moves the hue by degrees: a grey has none
                if c < 0.04 {
                    continue;
                }
                let t = base.with_accent(accent);
                let [_, hc, hh] = oklch_of(t.accent_hover);
                if hc < 0.04 {
                    continue; // lightness pushed to the gamut edge, where no chroma is left
                }
                let diff = (hh - h + 540.0).rem_euclid(360.0) - 180.0;
                assert!(diff.abs() <= 2.0, "dark={} accent {accent:?}: hover hue {hh:.1} vs {h:.1}", base.dark);
                checked += 1;
            }
        }
        assert!(checked > 64, "only {checked} accents were chromatic enough to check");
    }

    #[test]
    fn high_contrast_strengthens_text_and_borders() {
        for t in [Tokens::dark(), Tokens::light()] {
            let hc = t.high_contrast();
            for bg in surfaces(&t) {
                assert!(contrast_ratio(hc.text.secondary, bg) >= MIN_TEXT_CONTRAST);
                assert!(contrast_ratio(hc.text.secondary, bg) > contrast_ratio(t.text.secondary, bg));
            }
            assert!(contrast_ratio(hc.border_strong, t.surface.panel) >= MIN_UI_CONTRAST);
            assert!(contrast_ratio(hc.border, t.surface.panel) > contrast_ratio(t.border, t.surface.panel));
            assert_eq!(hc.text.primary, t.text.primary);
            assert_eq!(hc.accent, t.accent);
        }
    }

    #[test]
    fn motion_elevation_and_stroke_tokens_reach_egui() {
        let t = Tokens::dark();
        assert_eq!(t.motion.press, ashlar_motion::tokens::INSTANT);
        assert_eq!(Some(t.motion.surface), ashlar_motion::tokens::by_name("glide"));
        let v = t.visuals();
        assert_eq!(v.popup_shadow, t.elevation.popup);
        assert_eq!(v.window_shadow, t.elevation.window);
        let thick = Tokens { stroke: Strokes { hairline: 2.0, focus: 3.0 }, ..t }.visuals();
        assert_eq!(thick.window_stroke.width, 2.0);
        assert_eq!(thick.text_cursor.stroke.width, 3.0);
        assert!(Tokens::light().elevation.popup.color.a() < t.elevation.popup.color.a());
    }
}
