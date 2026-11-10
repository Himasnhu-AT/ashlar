//! Small static labels: badges (a count, a status) and keycaps (a shortcut).

use ashlar_command::shortcut::{Chord, Platform};
use egui::{Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo, WidgetType, pos2, vec2};

use super::text_galley;
use crate::automation::{self, Details, Role};
use crate::tokens::{self, Tokens};

/// The colour family of a badge (and of a banner, in [`crate::content`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tone {
    #[default]
    Neutral,
    Accent,
    Info,
    Success,
    Warning,
    Danger,
}

impl Tone {
    /// The tone's strong colour (text on a tinted fill, icons, borders).
    pub fn color(self, t: &Tokens) -> Color32 {
        match self {
            Tone::Neutral => t.text.secondary,
            Tone::Accent => t.accent,
            Tone::Info => t.status.info,
            Tone::Success => t.status.success,
            Tone::Warning => t.status.warning,
            Tone::Danger => t.status.danger,
        }
    }

    /// Fill and text colours of a badge.
    pub(crate) fn badge_colors(self, t: &Tokens) -> (Color32, Color32) {
        match self {
            Tone::Neutral => (t.surface.hover, t.text.primary),
            Tone::Accent => (t.accent, t.text.on_accent),
            tone => (super::mix(t.surface.raised, tone.color(t), 0.25), t.text.primary),
        }
    }
}

/// A pill with short text: a count, a state ("Beta", "Offline").
pub struct Badge<'a> {
    id: &'a str,
    text: &'a str,
    tone: Tone,
}

impl<'a> Badge<'a> {
    pub fn new(id: &'a str, text: &'a str) -> Self {
        Badge { id, text, tone: Tone::Neutral }
    }
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let galley = text_galley(ui, ("ashlar_ui::controls::badge", self.id), self.text, t.type_scale.small);
        let h = (galley.size().y + 2.0 * t.space.xxs).max(t.type_scale.small + 4.0);
        let w = (galley.size().x + 2.0 * t.space.sm).max(h);
        let (rect, response) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        if ui.is_rect_visible(rect) {
            let (fill, text) = self.tone.badge_colors(&t);
            ui.painter().rect_filled(rect, CornerRadius::same((h / 2.0).round().clamp(0.0, 255.0) as u8), fill);
            ui.painter().galley(Align2::CENTER_CENTER.align_size_within_rect(galley.size(), rect).min, galley, text);
        }
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, self.text));
        automation::register_with(ui, &response, self.id, self.text, Role::Label, Details { value: Some(self.text), selected: None });
        response
    }
}

/// Most keys one keycap row shows.
const MAX_KEYS: usize = 8;

/// Keys drawn as caps: `⌘` `⇧` `P` on macOS, `Ctrl` `Shift` `P` elsewhere.
pub struct Keycap<'a> {
    id: &'a str,
    keys: KeySource<'a>,
}

enum KeySource<'a> {
    Text(&'a str),
    Chord(Chord, Platform),
}

impl<'a> Keycap<'a> {
    /// One cap with this text.
    pub fn new(id: &'a str, text: &'a str) -> Self {
        Keycap { id, keys: KeySource::Text(text) }
    }
    /// A cap per key of a shortcut, written the way `platform` writes it.
    pub fn chord(id: &'a str, chord: Chord, platform: Platform) -> Self {
        Keycap { id, keys: KeySource::Chord(chord, platform) }
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens::current(ui.ctx());
        let (caps, name): (Vec<String>, String) = match &self.keys {
            KeySource::Text(s) => (vec![(*s).to_owned()], (*s).to_owned()),
            KeySource::Chord(chord, platform) => {
                let shown = chord.display(*platform);
                let caps = match platform {
                    Platform::Mac => mac_caps(&shown),
                    Platform::Other => other_caps(&shown),
                };
                (caps, shown)
            }
        };
        let size = t.type_scale.small;
        let galleys: Vec<_> =
            caps.iter().take(MAX_KEYS).enumerate().map(|(i, c)| text_galley(ui, ("ashlar_ui::controls::keycap", self.id, i), c, size)).collect();
        let h = (size + 2.0 * t.space.xs).round();
        let gap = t.space.xxs;
        let widths: Vec<f32> = galleys.iter().map(|g| (g.size().x + 2.0 * t.space.xs).max(h)).collect();
        let total = widths.iter().sum::<f32>() + gap * widths.len().saturating_sub(1) as f32;
        let (rect, response) = ui.allocate_exact_size(vec2(total, h + 1.0), Sense::hover());
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let radius = CornerRadius::same(t.radius.sm);
            let mut x = rect.left();
            for (g, w) in galleys.into_iter().zip(widths) {
                let cap = Rect::from_min_size(pos2(x, rect.top()), vec2(w, h));
                // a 1-point "edge" below the cap
                painter.rect_filled(cap.translate(vec2(0.0, 1.0)), radius, t.border);
                painter.rect(cap, radius, t.surface.raised, Stroke::new(1.0, t.border), StrokeKind::Inside);
                painter.galley(Align2::CENTER_CENTER.align_size_within_rect(g.size(), cap).min, g, t.text.secondary);
                x += w + gap;
            }
        }
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &name));
        automation::register_with(ui, &response, self.id, &name, Role::Label, Details { value: Some(&name), selected: None });
        response
    }
}

/// `⌃⇧K` → `⌃` `⇧` `K`: each modifier glyph is a cap, the rest is the key.
fn mac_caps(shown: &str) -> Vec<String> {
    let mut caps = Vec::new();
    let mut rest = shown;
    while let Some(c) = rest.chars().next() {
        if !matches!(c, '⌃' | '⌥' | '⇧' | '⌘') || rest.len() == c.len_utf8() {
            break;
        }
        caps.push(c.to_string());
        rest = rest.get(c.len_utf8()..).unwrap_or_default();
    }
    if !rest.is_empty() {
        caps.push(rest.to_owned());
    }
    caps
}

/// `Ctrl+Shift++` → `Ctrl` `Shift` `+`.
fn other_caps(shown: &str) -> Vec<String> {
    let mut caps: Vec<String> = Vec::new();
    let (mods, key) = match shown.strip_suffix("++") {
        _ if shown == "+" => ("", "+"),
        Some(m) => (m, "+"),
        None => match shown.rsplit_once('+') {
            Some((m, k)) => (m, k),
            None => ("", shown),
        },
    };
    caps.extend(mods.split('+').filter(|m| !m.is_empty()).map(str::to_owned));
    caps.push(key.to_owned());
    caps
}

#[cfg(test)]
mod caps_tests {
    use super::*;

    #[test]
    fn chords_split_into_caps() {
        assert_eq!(mac_caps("⌃⇧⌘K"), ["⌃", "⇧", "⌘", "K"]);
        assert_eq!(mac_caps("⌘"), ["⌘"]);
        assert_eq!(mac_caps("F5"), ["F5"]);
        assert_eq!(other_caps("Ctrl+Shift+P"), ["Ctrl", "Shift", "P"]);
        assert_eq!(other_caps("Ctrl++"), ["Ctrl", "+"]);
        assert_eq!(other_caps("+"), ["+"]);
        assert_eq!(other_caps("F5"), ["F5"]);
    }
}
