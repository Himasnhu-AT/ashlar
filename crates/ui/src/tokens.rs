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
}

impl Default for Tokens {
    fn default() -> Self {
        Self::dark()
    }
}

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
            accent: Color32::from_rgb(0x3b, 0x82, 0xf6),
            accent_hover: Color32::from_rgb(0x60, 0x9c, 0xf8),
            selection: Color32::from_rgb(0x1e, 0x4f, 0x9c),
            selection_text: Color32::WHITE,
            focus: Color32::from_rgb(0x7c, 0xb4, 0xff),
            border: Color32::from_rgb(0x3a, 0x3a, 0x40),
            border_strong: Color32::from_rgb(0x58, 0x58, 0x60),
            separator: Color32::from_rgb(0x2e, 0x2e, 0x33),
            radius: RADII,
            space: SPACE,
            type_scale: TYPE,
            control_height: 24.0,
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
            accent_hover: Color32::from_rgb(0x1d, 0x4e, 0xd8),
            selection: Color32::from_rgb(0xbf, 0xd7, 0xfe),
            selection_text: Color32::from_rgb(0x0b, 0x1f, 0x45),
            focus: Color32::from_rgb(0x25, 0x63, 0xeb),
            border: Color32::from_rgb(0xc8, 0xc8, 0xcf),
            border_strong: Color32::from_rgb(0x9a, 0x9a, 0xa3),
            separator: Color32::from_rgb(0xd9, 0xd9, 0xde),
            radius: RADII,
            space: SPACE,
            type_scale: TYPE,
            control_height: 24.0,
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

    /// Replace the accent (and derive its hover shade, focus and selection from it).
    pub fn with_accent(mut self, accent: Color32) -> Self {
        let [r, g, b, _] = accent.to_array();
        self.accent = accent;
        self.accent_hover = if self.dark { lerp(accent, Color32::WHITE, 0.18) } else { lerp(accent, Color32::BLACK, 0.15) };
        self.focus = if self.dark { lerp(accent, Color32::WHITE, 0.35) } else { accent };
        self.selection = if self.dark { lerp(Color32::from_rgb(r, g, b), self.surface.panel, 0.45) } else { lerp(accent, Color32::WHITE, 0.72) };
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
        v.selection.stroke = Stroke::new(1.0, self.selection_text);
        v.window_stroke = Stroke::new(1.0, self.border);
        v.window_corner_radius = CornerRadius::same(self.radius.lg);
        v.menu_corner_radius = CornerRadius::same(self.radius.md);
        let shadow_alpha = if self.dark { 140 } else { 40 };
        v.popup_shadow = egui::epaint::Shadow { offset: [0, 4], blur: 14, spread: 0, color: Color32::from_black_alpha(shadow_alpha) };
        v.window_shadow = egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(shadow_alpha) };
        WidgetPalette { noninteractive: s.panel, inactive: s.sunken, hovered: s.hover, active: s.pressed, open: s.hover }.apply_to(
            &mut v.widgets,
            CornerRadius::same(self.radius.md),
            Stroke::new(1.0, self.text.primary),
        );
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.separator);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, self.text.primary);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, self.border);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.widgets.active.bg_stroke = Stroke::new(1.0, self.focus);
        v.widgets.open.bg_stroke = Stroke::new(1.0, self.border_strong);
        v.text_cursor.stroke = Stroke::new(2.0, self.accent);
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

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()), mix(a.a(), b.a()))
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
        assert_eq!(lerp(Color32::BLACK, Color32::WHITE, f32::NAN), Color32::BLACK);
        assert_eq!(lerp(Color32::BLACK, Color32::WHITE, 2.0), Color32::WHITE);
    }
}
