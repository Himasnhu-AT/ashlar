//! Portable keyboard shortcuts.
//!
//! One syntax everywhere: `Mod+Shift+S`, where `Mod` is ⌘ on macOS and Ctrl elsewhere. `Ctrl` is the
//! literal Control key (⌃ on macOS; the same key as `Mod` elsewhere). Legacy spellings are
//! accepted: `Cmd+Z`, `Command+Z`, `CmdOrCtrl+Z`, `Option+X`, glyphs (`⌘⇧S`). Several shortcuts for
//! one command are separated by ` / ` (`Mod+Y / Mod+Shift+Z`).
//!
//! [`Chord`] is the UI-independent key chord; the UI layer maps its key events onto it.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Longest shortcut text parsed.
const MAX_LEN: usize = 64;
/// At most this many alternatives in one shortcut list.
const MAX_ALTERNATIVES: usize = 8;

/// The platform shortcuts are shown and matched for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Mac,
    Other,
}

impl Platform {
    /// The platform this build runs on (wasm reports `Other`; a web app can pick from the user agent).
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") || cfg!(target_os = "ios") { Platform::Mac } else { Platform::Other }
    }
}

/// Modifier keys. `command` is `Mod` (⌘ on macOS, Ctrl elsewhere); `ctrl` is the literal Control key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Mods {
    pub command: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { command: false, ctrl: false, alt: false, shift: false };
    pub const COMMAND: Mods = Mods { command: true, ctrl: false, alt: false, shift: false };
    pub const SHIFT: Mods = Mods { command: false, ctrl: false, alt: false, shift: true };
}

/// Keys with a name rather than a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Named {
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    Space,
    Insert,
}

const NAMED: &[(Named, &str, &[&str])] = &[
    (Named::Left, "Left", &["arrowleft"]),
    (Named::Right, "Right", &["arrowright"]),
    (Named::Up, "Up", &["arrowup"]),
    (Named::Down, "Down", &["arrowdown"]),
    (Named::Home, "Home", &[]),
    (Named::End, "End", &[]),
    (Named::PageUp, "PageUp", &["pgup"]),
    (Named::PageDown, "PageDown", &["pgdn", "pgdown"]),
    (Named::Enter, "Enter", &["return"]),
    (Named::Tab, "Tab", &[]),
    (Named::Backspace, "Backspace", &[]),
    (Named::Delete, "Delete", &["del", "forwarddelete"]),
    (Named::Escape, "Escape", &["esc"]),
    (Named::Space, "Space", &["spacebar"]),
    (Named::Insert, "Insert", &["ins"]),
];

impl Named {
    /// The canonical spelling (`PageUp`).
    pub fn name(self) -> &'static str {
        NAMED.iter().find(|(n, ..)| *n == self).map(|(_, s, _)| *s).unwrap_or_default()
    }

    fn mac_glyph(self) -> &'static str {
        match self {
            Named::Left => "←",
            Named::Right => "→",
            Named::Up => "↑",
            Named::Down => "↓",
            Named::Home => "↖",
            Named::End => "↘",
            Named::PageUp => "⇞",
            Named::PageDown => "⇟",
            Named::Enter => "↩",
            Named::Tab => "⇥",
            Named::Backspace => "⌫",
            Named::Delete => "⌦",
            Named::Escape => "⎋",
            Named::Space => "Space",
            Named::Insert => "Insert",
        }
    }
}

/// The key of a chord.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// A character key, letters upper-cased (`S`, `1`, `/`, `]`).
    Char(char),
    Named(Named),
    /// `F1`…`F24`.
    F(u8),
}

impl Key {
    /// Parse a key name: one character, a named key (`PageUp`, `Esc`, `Return`…), `F1`–`F24`,
    /// `Plus`/`Minus`.
    pub fn parse(name: &str) -> Option<Key> {
        let name = name.trim();
        let mut chars = name.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if c.is_whitespace() {
                return (c == ' ').then_some(Key::Named(Named::Space));
            }
            if c.is_control() {
                return None;
            }
            let mut up = c.to_uppercase();
            return Some(Key::Char(match (up.next(), up.next()) {
                (Some(u), None) => u,
                _ => c,
            }));
        }
        let lower = name.to_ascii_lowercase();
        if let Some((n, ..)) = NAMED.iter().find(|(_, canon, alts)| canon.eq_ignore_ascii_case(name) || alts.contains(&lower.as_str())) {
            return Some(Key::Named(*n));
        }
        match lower.as_str() {
            "plus" => return Some(Key::Char('+')),
            "minus" => return Some(Key::Char('-')),
            _ => {}
        }
        // `F` followed by 1..=24 (and nothing else: `F1x` is not a key)
        let digits = lower.strip_prefix('f')?;
        if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: u8 = digits.parse().ok()?;
        (1..=24).contains(&n).then_some(Key::F(n))
    }

    fn portable(self) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::Named(n) => n.name().to_string(),
            Key::F(n) => format!("F{n}"),
        }
    }
}

/// One key chord: modifiers plus a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub mods: Mods,
    pub key: Key,
}

/// Text that is not a shortcut.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a keyboard shortcut: `{0}`")]
pub struct ParseError(pub String);

fn err(s: &str) -> ParseError {
    ParseError(s.chars().take(MAX_LEN).collect())
}

impl Chord {
    pub const fn new(mods: Mods, key: Key) -> Self {
        Self { mods, key }
    }

    /// Parse one shortcut (`Mod+Shift+S`, `Cmd+Z`, `Ctrl+Alt+Delete`, `⌘⇧S`, `Mod++`, `F5`).
    pub fn parse(text: &str) -> Result<Chord, ParseError> {
        let s = text.trim();
        if s.is_empty() || s.len() > MAX_LEN {
            return Err(err(text));
        }
        let mut mods = Mods::NONE;
        // macOS glyph prefix: ⌃⌥⇧⌘ in any order, then the key (or `+`-separated text)
        let mut rest = s;
        while let Some(c) = rest.chars().next() {
            match c {
                '⌘' => mods.command = true,
                '⌃' => mods.ctrl = true,
                '⌥' => mods.alt = true,
                '⇧' => mods.shift = true,
                _ => break,
            }
            rest = rest.get(c.len_utf8()..).unwrap_or_default();
        }
        if rest.len() != s.len() {
            let key = glyph_key(rest).or_else(|| Key::parse(rest)).ok_or_else(|| err(text))?;
            return Ok(Chord { mods, key });
        }
        // `Mod++` is the plus key with Mod; a lone `+` is the plus key
        let (mod_part, name) = if s == "+" {
            ("", "+")
        } else if let Some(m) = s.strip_suffix("++") {
            (m, "+")
        } else {
            match s.rsplit_once('+') {
                Some((m, n)) => (m, n),
                None => ("", s),
            }
        };
        for m in mod_part.split('+') {
            match m.trim().to_ascii_lowercase().as_str() {
                "" if mod_part.is_empty() => {}
                "mod" | "cmd" | "command" | "cmdorctrl" | "commandorcontrol" | "primary" | "meta" | "super" => mods.command = true,
                "ctrl" | "control" | "macctrl" => mods.ctrl = true,
                "alt" | "option" | "opt" => mods.alt = true,
                "shift" => mods.shift = true,
                _ => return Err(err(text)),
            }
        }
        let key = glyph_key(name).or_else(|| Key::parse(name)).ok_or_else(|| err(text))?;
        Ok(Chord { mods, key })
    }

    /// The portable form (`Mod+Ctrl+Alt+Shift+K`), what settings files and discovery carry.
    pub fn portable(&self) -> String {
        let mut out = String::new();
        for (on, m) in [(self.mods.command, "Mod+"), (self.mods.ctrl, "Ctrl+"), (self.mods.alt, "Alt+"), (self.mods.shift, "Shift+")] {
            if on {
                out.push_str(m);
            }
        }
        out.push_str(&self.key.portable());
        out
    }

    /// The chord as the user sees it: `⌘⇧S` on macOS, `Ctrl+Shift+S` elsewhere.
    pub fn display(&self, platform: Platform) -> String {
        match platform {
            Platform::Mac => {
                let mut out = String::new();
                // Apple's order: Control, Option, Shift, Command
                for (on, g) in [(self.mods.ctrl, '⌃'), (self.mods.alt, '⌥'), (self.mods.shift, '⇧'), (self.mods.command, '⌘')] {
                    if on {
                        out.push(g);
                    }
                }
                match self.key {
                    Key::Named(n) => out.push_str(n.mac_glyph()),
                    k => out.push_str(&k.portable()),
                }
                out
            }
            Platform::Other => {
                let mut out = String::new();
                for (on, m) in [(self.mods.command || self.mods.ctrl, "Ctrl+"), (self.mods.alt, "Alt+"), (self.mods.shift, "Shift+")] {
                    if on {
                        out.push_str(m);
                    }
                }
                out.push_str(&self.key.portable());
                out
            }
        }
    }

    /// The chord as the keyboard of `platform` produces it: off macOS, `Mod` and `Ctrl` are the same
    /// key, so both fold into `command`. Compare chords in this form.
    pub fn on(self, platform: Platform) -> Chord {
        match platform {
            Platform::Mac => self,
            Platform::Other => Chord { mods: Mods { command: self.mods.command || self.mods.ctrl, ctrl: false, ..self.mods }, key: self.key },
        }
    }
}

fn glyph_key(s: &str) -> Option<Key> {
    let n = match s.trim() {
        "←" => Named::Left,
        "→" => Named::Right,
        "↑" => Named::Up,
        "↓" => Named::Down,
        "↖" => Named::Home,
        "↘" => Named::End,
        "⇞" => Named::PageUp,
        "⇟" => Named::PageDown,
        "↩" | "⏎" => Named::Enter,
        "⇥" => Named::Tab,
        "⌫" => Named::Backspace,
        "⌦" => Named::Delete,
        "⎋" => Named::Escape,
        _ => return None,
    };
    Some(Key::Named(n))
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.portable())
    }
}

impl std::str::FromStr for Chord {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Chord::parse(s)
    }
}

impl Serialize for Chord {
    fn serialize<Ser: Serializer>(&self, s: Ser) -> Result<Ser::Ok, Ser::Error> {
        s.serialize_str(&self.portable())
    }
}

impl<'de> Deserialize<'de> for Chord {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Chord::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// Parse a list of alternatives (`Mod+Y / Mod+Shift+Z`). Every alternative must parse.
pub fn parse_list(text: &str) -> Result<Vec<Chord>, ParseError> {
    if text.len() > MAX_LEN * MAX_ALTERNATIVES {
        return Err(err(text));
    }
    let chords = text.split(" / ").filter(|p| !p.trim().is_empty()).map(Chord::parse).collect::<Result<Vec<_>, _>>()?;
    if chords.is_empty() || chords.len() > MAX_ALTERNATIVES {
        return Err(err(text));
    }
    Ok(chords)
}

/// Portable form of a list: `Mod+Y / Mod+Shift+Z`.
pub fn format_list(chords: &[Chord]) -> String {
    chords.iter().map(Chord::portable).collect::<Vec<_>>().join(" / ")
}

/// Display form of a list for `platform`.
pub fn display_list(chords: &[Chord], platform: Platform) -> String {
    chords.iter().map(|c| c.display(platform)).collect::<Vec<_>>().join(" / ")
}

/// A shortcut text in canonical portable form (`cmd+shift+s` → `Mod+Shift+S`); `None` when it is
/// not a shortcut.
pub fn normalize(text: &str) -> Option<String> {
    parse_list(text).ok().map(|c| format_list(&c))
}

/// A shortcut text as shown on `platform` (`Mod+Shift+S` → `⌘⇧S`); `None` when it is not a shortcut.
pub fn display(text: &str, platform: Platform) -> Option<String> {
    parse_list(text).ok().map(|c| display_list(&c, platform))
}
