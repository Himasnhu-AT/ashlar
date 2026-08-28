//! Portable accelerator text (`Mod+Shift+S`) parsed, checked and converted to the menu backend's
//! notation. `Mod` is ⌘ on macOS and Ctrl elsewhere.

use crate::{NativeError, Result};

/// Longest accelerator text accepted.
const MAX_LEN: usize = 64;

/// A parsed accelerator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    /// `Mod`: ⌘ on macOS, Ctrl elsewhere.
    pub primary: bool,
    /// `Cmd` / `Super` / `Meta` written explicitly.
    pub cmd: bool,
    /// `Ctrl` written explicitly.
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Accelerator {
    pub modifiers: Modifiers,
    /// The key in canonical form: `S`, `0`, `;`, `F5`, `Enter`, `ArrowLeft`, `+`.
    pub key: String,
}

const NAMED: &[&str] = &[
    "Enter",
    "Escape",
    "Space",
    "Tab",
    "Backspace",
    "Delete",
    "Insert",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "ArrowLeft",
    "ArrowRight",
    "ArrowUp",
    "ArrowDown",
];
const PUNCTUATION: &str = "`\\[],-=./;'+";

fn function_key(k: &str) -> bool {
    k.len() >= 2 && (k.starts_with('F') || k.starts_with('f')) && k.get(1..).and_then(|n| n.parse::<u8>().ok()).is_some_and(|n| (1..=24).contains(&n))
}

fn canonical_key(k: &str) -> Option<String> {
    let mut chars = k.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphanumeric() {
            return Some(c.to_ascii_uppercase().to_string());
        }
        return PUNCTUATION.contains(c).then(|| c.to_string());
    }
    if function_key(k) {
        return Some(format!("F{}", k.get(1..).unwrap_or_default()));
    }
    let alias = match k.to_ascii_lowercase().as_str() {
        "return" => "Enter",
        "esc" => "Escape",
        "del" => "Delete",
        "left" => "ArrowLeft",
        "right" => "ArrowRight",
        "up" => "ArrowUp",
        "down" => "ArrowDown",
        "plus" => "+",
        "minus" => "-",
        _ => "",
    };
    if !alias.is_empty() {
        return Some(alias.to_string());
    }
    NAMED.iter().find(|n| n.eq_ignore_ascii_case(k)).map(|n| n.to_string())
}

impl Accelerator {
    /// Parse `Mod+Shift+S`, `Ctrl+Alt+Delete`, `F5`, `Mod++`. Case-insensitive modifiers.
    pub fn parse(text: &str) -> Result<Self> {
        let bad = || NativeError::Invalid(format!("not an accelerator: `{}`", text.chars().take(MAX_LEN).collect::<String>()));
        let text = text.trim();
        if text.is_empty() || text.len() > MAX_LEN {
            return Err(bad());
        }
        let parts: Vec<&str> = match text.strip_suffix("++") {
            Some(head) => head.split('+').chain(std::iter::once("+")).collect(),
            None if text == "+" => vec!["+"],
            None => text.split('+').collect(),
        };
        let (key, mods) = parts.split_last().ok_or_else(bad)?;
        let mut m = Modifiers::default();
        for raw in mods {
            let flag = match raw.trim().to_ascii_lowercase().as_str() {
                "mod" | "cmdorctrl" | "commandorcontrol" => &mut m.primary,
                "cmd" | "command" | "super" | "meta" => &mut m.cmd,
                "ctrl" | "control" => &mut m.ctrl,
                "alt" | "option" => &mut m.alt,
                "shift" => &mut m.shift,
                _ => return Err(bad()),
            };
            if *flag {
                return Err(bad());
            }
            *flag = true;
        }
        let key = canonical_key(key.trim()).ok_or_else(bad)?;
        Ok(Self { modifiers: m, key })
    }

    /// Whether the accelerator may become a native menu key equivalent. Only ones with ⌘ / Ctrl
    /// (or a function key) do: a bare `V` or an Alt/Shift-only key would be swallowed by the menu
    /// while the user types into a text field, so those stay the app's own window shortcuts.
    pub fn is_menu_safe(&self) -> bool {
        let m = self.modifiers;
        m.primary || m.cmd || m.ctrl || function_key(&self.key)
    }

    /// The portable text again, in canonical order: `Mod+Ctrl+Alt+Shift+S`.
    pub fn portable(&self) -> String {
        let m = self.modifiers;
        let mut out: Vec<&str> = vec![];
        for (on, name) in [(m.primary, "Mod"), (m.cmd, "Cmd"), (m.ctrl, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift")] {
            if on {
                out.push(name);
            }
        }
        out.push(&self.key);
        out.join("+")
    }

    /// The text `muda` parses (`CmdOrCtrl+Shift+S`).
    pub fn to_muda(&self) -> String {
        let m = self.modifiers;
        let mut out: Vec<String> = vec![];
        for (on, name) in [(m.primary, "CmdOrCtrl"), (m.cmd, "Super"), (m.ctrl, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift")] {
            if on {
                out.push(name.into());
            }
        }
        out.push(
            match self.key.as_str() {
                "+" => "Equal",
                "Insert" => "Insert",
                k => k,
            }
            .to_string(),
        );
        out.join("+")
    }
}

/// The native accelerator for portable text: `None` when it does not parse or is not menu-safe
/// ([`Accelerator::is_menu_safe`]). Alternatives (`Mod+Z / Mod+Shift+Z`) use the first.
pub fn menu_accelerator(text: &str) -> Option<Accelerator> {
    let first = text.split(" / ").next()?;
    Accelerator::parse(first).ok().filter(Accelerator::is_menu_safe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_text_parses_and_converts() {
        let a = Accelerator::parse("Mod+Shift+S").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(a.key, "S");
        assert_eq!(a.portable(), "Mod+Shift+S");
        assert_eq!(a.to_muda(), "CmdOrCtrl+Shift+S");
        let cases = [
            ("mod+shift+s", "Mod+Shift+S", "CmdOrCtrl+Shift+S"),
            ("Ctrl+Cmd+F", "Cmd+Ctrl+F", "Super+Ctrl+F"),
            ("Mod++", "Mod++", "CmdOrCtrl+Equal"),
            ("Mod+Alt+;", "Mod+Alt+;", "CmdOrCtrl+Alt+;"),
            ("F5", "F5", "F5"),
            ("Mod+Left", "Mod+ArrowLeft", "CmdOrCtrl+ArrowLeft"),
            ("Option+Return", "Alt+Enter", "Alt+Enter"),
            ("Mod+`", "Mod+`", "CmdOrCtrl+`"),
        ];
        for (text, portable, muda) in cases {
            let a = Accelerator::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!((a.portable().as_str(), a.to_muda().as_str()), (portable, muda), "{text}");
        }
    }

    #[test]
    fn junk_is_refused_without_panicking() {
        for bad in ["", "+Mod", "Mod+", "Mod+Mod+S", "Hyper+S", "Mod+Shift+Banana", "Mod+é", &"Mod+".repeat(40), "Mod+F0", "Mod+F25", "S+Mod"] {
            assert!(Accelerator::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn only_command_keys_become_menu_key_equivalents() {
        assert!(menu_accelerator("Mod+S").is_some());
        assert!(menu_accelerator("Ctrl+Alt+Delete").is_some());
        assert!(menu_accelerator("F3").is_some());
        assert!(menu_accelerator("Mod+Z / Mod+Shift+Z").is_some_and(|a| !a.modifiers.shift));
        for typing in ["V", "Space", "Shift+=", "Alt+[", "Escape"] {
            assert!(menu_accelerator(typing).is_none(), "{typing}");
        }
    }
}
