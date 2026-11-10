//! The physical key a [`Chord`] names, as a `keyboard-types` code name: shared by menu key
//! equivalents and global shortcuts.

use ashlar_command::shortcut::Chord;

/// The `keyboard-types` code name of the physical key `chord` names (`KeyS`, `Digit0`,
/// `Semicolon`, `ArrowLeft`, `F5`); `None` for keys a menu cannot show (`é`).
pub(crate) fn code_name(chord: &Chord) -> Option<String> {
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
