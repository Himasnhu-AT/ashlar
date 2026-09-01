//! Menu key equivalents. Shortcut text is parsed by [`ashlar_command::shortcut::Chord`] (the one
//! parser) and [`to_muda`] maps a chord onto the menu backend's accelerator. `Mod` is ⌘ on macOS
//! and Ctrl elsewhere.

use ashlar_command::shortcut::{Chord, Mods};

/// Deprecated: accelerators are [`Chord`]s (`Chord::parse`, `Chord::is_menu_safe`,
/// `Chord::portable`).
#[deprecated(note = "use ashlar_command::shortcut::Chord")]
pub type Accelerator = Chord;

/// Deprecated: modifiers are [`Mods`] (`command` is `Mod`).
#[deprecated(note = "use ashlar_command::shortcut::Mods")]
pub type Modifiers = Mods;

/// Deprecated: use [`ashlar_command::menu::menu_chord`].
#[deprecated(note = "use ashlar_command::menu::menu_chord")]
pub fn menu_accelerator(text: &str) -> Option<Chord> {
    ashlar_command::menu::menu_chord(text)
}

/// The `keyboard-types` code name of the physical key `chord` names (`KeyS`, `Digit0`,
/// `Semicolon`, `ArrowLeft`, `F5`); `None` for keys a menu cannot show (`é`).
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn code_name(chord: &Chord) -> Option<String> {
    use ashlar_command::shortcut::{Key, Named};
    Some(match chord.key {
        Key::Char(c) if c.is_ascii_uppercase() => format!("Key{c}"),
        Key::Char(c) if c.is_ascii_digit() => format!("Digit{c}"),
        Key::Char(c) => {
            let name = match c {
                '`' => "Backquote",
                '\\' => "Backslash",
                '[' => "BracketLeft",
                ']' => "BracketRight",
                ',' => "Comma",
                '-' => "Minus",
                // `Mod++` (zoom in) is the `=` key, as menus show it.
                '=' | '+' => "Equal",
                '.' => "Period",
                '/' => "Slash",
                ';' => "Semicolon",
                '\'' => "Quote",
                _ => return None,
            };
            name.to_string()
        }
        Key::Named(n) => match n {
            Named::Left => "ArrowLeft",
            Named::Right => "ArrowRight",
            Named::Up => "ArrowUp",
            Named::Down => "ArrowDown",
            n => n.name(),
        }
        .to_string(),
        Key::F(n) => format!("F{n}"),
    })
}

/// The menu backend's accelerator for `chord`; `None` for keys it cannot show.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn to_muda(chord: &Chord) -> Option<muda::accelerator::Accelerator> {
    use muda::accelerator::{CMD_OR_CTRL, Code, Modifiers as M};
    let code: Code = code_name(chord)?.parse().ok()?;
    let mut mods = M::empty();
    for (on, m) in [(chord.mods.command, CMD_OR_CTRL), (chord.mods.ctrl, M::CONTROL), (chord.mods.alt, M::ALT), (chord.mods.shift, M::SHIFT)] {
        if on {
            mods |= m;
        }
    }
    Some(muda::accelerator::Accelerator::new(mods, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        Chord::parse(text).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn chords_map_to_physical_key_codes() {
        let cases = [
            ("Mod+Shift+S", "KeyS"),
            ("Mod+0", "Digit0"),
            ("Mod++", "Equal"),
            ("Mod+=", "Equal"),
            ("Mod+Alt+;", "Semicolon"),
            ("F5", "F5"),
            ("Mod+Left", "ArrowLeft"),
            ("Option+Return", "Enter"),
            ("Mod+`", "Backquote"),
            ("Mod+PageDown", "PageDown"),
            ("⌘⇧[", "BracketLeft"),
        ];
        for (text, code) in cases {
            assert_eq!(code_name(&chord(text)).as_deref(), Some(code), "{text}");
        }
        assert_eq!(code_name(&chord("Mod+é")), None);
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn chords_become_muda_accelerators() {
        use muda::accelerator::{Accelerator as A, CMD_OR_CTRL, Code, Modifiers as M};
        assert_eq!(to_muda(&chord("Mod+Shift+S")), Some(A::new(CMD_OR_CTRL | M::SHIFT, Code::KeyS)));
        assert_eq!(to_muda(&chord("Ctrl+Alt+Delete")), Some(A::new(M::CONTROL | M::ALT, Code::Delete)));
        assert_eq!(to_muda(&chord("F12")), Some(A::new(M::empty(), Code::F12)));
        // Every chord the parser accepts either maps or is refused; none panics.
        for text in ["Mod+F24", "Mod+Insert", "Mod+Space", "Mod+Tab", "Mod+Escape", "Mod+/", "Mod+'", "Mod+\\", "Mod+,", "Mod+."] {
            assert!(to_muda(&chord(text)).is_some(), "{text}");
        }
        assert!(to_muda(&chord("Mod+é")).is_none());
    }
}
