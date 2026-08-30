//! egui key events ↔ portable [`Chord`]s, and keyboard dispatch through a [`Keymap`].

use ashlar_command::Keymap;
use ashlar_command::shortcut::{Chord, Key, Mods, Named, Platform};
use egui::{Key as EKey, KeyboardShortcut, Modifiers};

const NAMED: &[(EKey, Named)] = &[
    (EKey::ArrowLeft, Named::Left),
    (EKey::ArrowRight, Named::Right),
    (EKey::ArrowUp, Named::Up),
    (EKey::ArrowDown, Named::Down),
    (EKey::Home, Named::Home),
    (EKey::End, Named::End),
    (EKey::PageUp, Named::PageUp),
    (EKey::PageDown, Named::PageDown),
    (EKey::Enter, Named::Enter),
    (EKey::Tab, Named::Tab),
    (EKey::Backspace, Named::Backspace),
    (EKey::Delete, Named::Delete),
    (EKey::Escape, Named::Escape),
    (EKey::Space, Named::Space),
    (EKey::Insert, Named::Insert),
];

const CHARS: &[(EKey, char)] = &[
    (EKey::Colon, ':'),
    (EKey::Comma, ','),
    (EKey::Backslash, '\\'),
    (EKey::Slash, '/'),
    (EKey::Pipe, '|'),
    (EKey::Questionmark, '?'),
    (EKey::Exclamationmark, '!'),
    (EKey::OpenBracket, '['),
    (EKey::CloseBracket, ']'),
    (EKey::OpenCurlyBracket, '{'),
    (EKey::CloseCurlyBracket, '}'),
    (EKey::Backtick, '`'),
    (EKey::Minus, '-'),
    (EKey::Period, '.'),
    (EKey::Plus, '+'),
    (EKey::Equals, '='),
    (EKey::Semicolon, ';'),
    (EKey::Quote, '\''),
];

const LETTERS: [EKey; 26] = [
    EKey::A,
    EKey::B,
    EKey::C,
    EKey::D,
    EKey::E,
    EKey::F,
    EKey::G,
    EKey::H,
    EKey::I,
    EKey::J,
    EKey::K,
    EKey::L,
    EKey::M,
    EKey::N,
    EKey::O,
    EKey::P,
    EKey::Q,
    EKey::R,
    EKey::S,
    EKey::T,
    EKey::U,
    EKey::V,
    EKey::W,
    EKey::X,
    EKey::Y,
    EKey::Z,
];

const DIGITS: [EKey; 10] = [EKey::Num0, EKey::Num1, EKey::Num2, EKey::Num3, EKey::Num4, EKey::Num5, EKey::Num6, EKey::Num7, EKey::Num8, EKey::Num9];

const FKEYS: [EKey; 24] = [
    EKey::F1,
    EKey::F2,
    EKey::F3,
    EKey::F4,
    EKey::F5,
    EKey::F6,
    EKey::F7,
    EKey::F8,
    EKey::F9,
    EKey::F10,
    EKey::F11,
    EKey::F12,
    EKey::F13,
    EKey::F14,
    EKey::F15,
    EKey::F16,
    EKey::F17,
    EKey::F18,
    EKey::F19,
    EKey::F20,
    EKey::F21,
    EKey::F22,
    EKey::F23,
    EKey::F24,
];

/// The portable key for an egui key (`None` for modifier keys and keys a chord cannot name).
pub fn key_from_egui(key: EKey) -> Option<Key> {
    if let Some((_, n)) = NAMED.iter().find(|(k, _)| *k == key) {
        return Some(Key::Named(*n));
    }
    if let Some((_, c)) = CHARS.iter().find(|(k, _)| *k == key) {
        return Some(Key::Char(*c));
    }
    if let Some(i) = LETTERS.iter().position(|k| *k == key) {
        return Some(Key::Char((b'A' + i as u8) as char));
    }
    if let Some(i) = DIGITS.iter().position(|k| *k == key) {
        return Some(Key::Char((b'0' + i as u8) as char));
    }
    FKEYS.iter().position(|k| *k == key).map(|i| Key::F(i as u8 + 1))
}

/// The egui key for a portable key.
pub fn key_to_egui(key: Key) -> Option<EKey> {
    match key {
        Key::Named(n) => NAMED.iter().find(|(_, x)| *x == n).map(|(k, _)| *k),
        Key::F(n) => FKEYS.get((n as usize).checked_sub(1)?).copied(),
        Key::Char(c) => {
            let up = c.to_ascii_uppercase();
            if up.is_ascii_uppercase() {
                LETTERS.get((up as u8 - b'A') as usize).copied()
            } else if c.is_ascii_digit() {
                DIGITS.get((c as u8 - b'0') as usize).copied()
            } else {
                CHARS.iter().find(|(_, x)| *x == c).map(|(k, _)| *k)
            }
        }
    }
}

/// The chord a key press makes on `platform` (`Mod` is ⌘ on macOS, Ctrl elsewhere).
pub fn chord_from_egui(key: EKey, modifiers: Modifiers, platform: Platform) -> Option<Chord> {
    let key = key_from_egui(key)?;
    let mods = match platform {
        // on macOS egui sets `command` only together with ⌘ (`mac_cmd`); synthetic input may set one
        Platform::Mac => Mods { command: modifiers.mac_cmd || modifiers.command, ctrl: modifiers.ctrl, alt: modifiers.alt, shift: modifiers.shift },
        Platform::Other => Mods { command: modifiers.ctrl || modifiers.command, ctrl: false, alt: modifiers.alt, shift: modifiers.shift },
    };
    Some(Chord::new(mods, key))
}

/// The egui shortcut for a chord (for menus and `consume_shortcut`).
pub fn chord_to_egui(chord: &Chord) -> Option<KeyboardShortcut> {
    let key = key_to_egui(chord.key)?;
    let mut m = Modifiers::NONE;
    if chord.mods.command {
        m |= Modifiers::COMMAND;
    }
    if chord.mods.ctrl {
        m |= Modifiers::CTRL;
    }
    if chord.mods.alt {
        m |= Modifiers::ALT;
    }
    if chord.mods.shift {
        m |= Modifiers::SHIFT;
    }
    Some(KeyboardShortcut::new(m, key))
}

/// Whether the chord is safe to run while a text field has the keyboard (it has ⌘/Ctrl, or is a
/// function key): a plain `V` or `Space` belongs to the text field.
pub fn works_while_typing(chord: &Chord) -> bool {
    chord.mods.command || chord.mods.ctrl || matches!(chord.key, Key::F(_))
}

/// Find this frame's key press bound in `keymap`, consume it and return the command id.
///
/// Call once per frame before drawing panels. Chords without ⌘/Ctrl are skipped while a text
/// field has the keyboard; key repeats are ignored.
pub fn dispatch(ctx: &egui::Context, keymap: &Keymap) -> Option<String> {
    let platform = keymap.platform();
    let typing = ctx.egui_wants_keyboard_input();
    ctx.input_mut(|i| {
        let mut hit = None;
        for (n, event) in i.events.iter().enumerate() {
            if let egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } = event
                && let Some(chord) = chord_from_egui(*key, *modifiers, platform)
                && (!typing || works_while_typing(&chord))
                && let Some(id) = keymap.lookup(&chord)
            {
                hit = Some((n, id.to_owned()));
                break;
            }
        }
        let (n, id) = hit?;
        if n < i.events.len() {
            i.events.remove(n);
        }
        Some(id)
    })
}

/// Shortcut text as shown on `platform`, spelled out where the installed fonts lack a macOS
/// glyph (`⇧⌘E` becomes `Shift+⌘E` when there is no `⇧`).
pub fn shortcut_label(ctx: &egui::Context, text: &str, platform: Platform, font: &egui::FontId) -> Option<String> {
    let shown = ashlar_command::shortcut::display(text, platform)?;
    Some(fit_glyphs(ctx, shown, font))
}

/// Replace modifier and key glyphs the fonts cannot draw with their names.
pub fn fit_glyphs(ctx: &egui::Context, shown: String, font: &egui::FontId) -> String {
    const NAMES: &[(char, &str)] = &[
        ('⌃', "Ctrl+"),
        ('⌥', "Opt+"),
        ('⇧', "Shift+"),
        ('⌘', "Cmd+"),
        ('←', "Left"),
        ('→', "Right"),
        ('↑', "Up"),
        ('↓', "Down"),
        ('↖', "Home"),
        ('↘', "End"),
        ('⇞', "PageUp"),
        ('⇟', "PageDown"),
        ('↩', "Enter"),
        ('⇥', "Tab"),
        ('⌫', "Backspace"),
        ('⌦', "Delete"),
        ('⎋', "Esc"),
    ];
    if shown.is_ascii() {
        return shown;
    }
    let missing: Vec<char> = ctx.fonts_mut(|f| shown.chars().filter(|c| !c.is_ascii() && !f.has_glyph(font, *c)).collect());
    if missing.is_empty() {
        return shown;
    }
    let mut out = String::with_capacity(shown.len() + 8);
    for c in shown.chars() {
        match NAMES.iter().find(|(g, _)| *g == c && missing.contains(g)) {
            Some((_, name)) => out.push_str(name),
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mapped_key_round_trips() {
        for key in EKey::ALL {
            if let Some(k) = key_from_egui(*key) {
                assert_eq!(key_to_egui(k), Some(*key), "{key:?}");
            }
        }
        assert_eq!(key_from_egui(EKey::ShiftLeft), None);
        assert_eq!(key_to_egui(Key::F(0)), None);
        assert_eq!(key_to_egui(Key::Char('é')), None);
    }

    #[test]
    fn modifiers_follow_the_platform() {
        let cmd_s = Modifiers { mac_cmd: true, command: true, ..Default::default() };
        let mac = chord_from_egui(EKey::S, cmd_s, Platform::Mac).unwrap();
        assert_eq!(mac, Chord::parse("Mod+S").unwrap());
        let ctrl_s = Modifiers { ctrl: true, command: true, ..Default::default() };
        assert_eq!(chord_from_egui(EKey::S, ctrl_s, Platform::Other).unwrap(), Chord::parse("Mod+S").unwrap());
        assert_eq!(chord_from_egui(EKey::S, Modifiers::CTRL, Platform::Mac).unwrap(), Chord::parse("Ctrl+S").unwrap());
        let ks = chord_to_egui(&Chord::parse("Mod+Shift+Z").unwrap()).unwrap();
        assert_eq!(ks, KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, EKey::Z));
        assert!(works_while_typing(&Chord::parse("Mod+Z").unwrap()));
        assert!(works_while_typing(&Chord::parse("F5").unwrap()));
        assert!(!works_while_typing(&Chord::parse("Space").unwrap()));
    }

    #[test]
    fn missing_glyphs_are_spelled_out() {
        let ctx = egui::Context::default();
        let font = egui::FontId::proportional(13.0);
        let mut out = ctx.run_ui(egui::RawInput::default(), |_| {});
        out.textures_delta.clear();
        let label = shortcut_label(&ctx, "Mod+Shift+E", Platform::Mac, &font).unwrap();
        let has = |c: char| ctx.fonts_mut(|f| f.has_glyph(&font, c));
        assert_eq!(label.contains('⇧'), has('⇧'), "{label}");
        assert!(label.ends_with('E'));
        assert_eq!(shortcut_label(&ctx, "Mod+Shift+E", Platform::Other, &font).as_deref(), Some("Ctrl+Shift+E"));
        assert_eq!(shortcut_label(&ctx, "not a key+++", Platform::Other, &font), None);
    }

    #[test]
    fn dispatch_consumes_the_bound_key_only() {
        let keymap = Keymap::new(Platform::Other, [("file.save", Some("Mod+S")), ("view.play", Some("Space"))]);
        let ctx = egui::Context::default();
        let key = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        let mut got = Vec::new();
        let input =
            egui::RawInput { events: vec![key(EKey::A, Modifiers::NONE), key(EKey::S, Modifiers::CTRL | Modifiers::COMMAND)], ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            got.push(dispatch(ui.ctx(), &keymap));
            got.push(dispatch(ui.ctx(), &keymap));
            assert!(ui.input(|i| i.key_pressed(EKey::A)), "unbound keys stay");
        });
        out.textures_delta.clear();
        assert_eq!(got, [Some("file.save".to_owned()), None]);
    }
}
