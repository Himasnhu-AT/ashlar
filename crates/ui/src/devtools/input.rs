//! Injected input: one [`InputFrame`] per frame, built from request parameters. Every number that
//! comes from a request is hostile: non-finite values are refused, the rest clamped.

use ashlar_command::shortcut::{Chord, Platform};
use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Vec2, pos2, vec2};
use serde_json::Value;

use crate::commands::keys::key_to_egui;

/// One frame of injected input.
#[derive(Clone, Debug, Default)]
pub struct InputFrame {
    pub events: Vec<Event>,
    pub modifiers: Modifiers,
}

impl InputFrame {
    fn one(e: Event, modifiers: Modifiers) -> Self {
        InputFrame { events: vec![e], modifiers }
    }
}

/// `{command, shift, alt, ctrl}` (`command` = ⌘ on macOS, Ctrl elsewhere) from `p.modifiers`.
pub fn modifiers(p: &Value) -> Modifiers {
    let m = p.get("modifiers");
    let f = |k: &str| m.and_then(|m| m.get(k)).and_then(Value::as_bool).unwrap_or(false);
    egui_modifiers(f("command"), f("ctrl"), f("alt"), f("shift"))
}

/// egui modifiers as a real keyboard on this platform reports them.
pub fn egui_modifiers(command: bool, ctrl: bool, alt: bool, shift: bool) -> Modifiers {
    let mac = Platform::current() == Platform::Mac;
    Modifiers { alt, ctrl: ctrl || (command && !mac), shift, mac_cmd: command && mac, command: command || (ctrl && !mac) }
}

pub fn button(p: &Value) -> PointerButton {
    match p.get("button").and_then(Value::as_str) {
        Some("secondary" | "right") => PointerButton::Secondary,
        Some("middle") => PointerButton::Middle,
        _ => PointerButton::Primary,
    }
}

fn finite(v: Option<&Value>) -> Option<f32> {
    let f = v?.as_f64()? as f32;
    f.is_finite().then_some(f.clamp(-1.0e6, 1.0e6))
}

/// `{x, y}` from params.
pub fn point(p: &Value) -> Option<Pos2> {
    Some(pos2(finite(p.get("x"))?, finite(p.get("y"))?))
}

/// `{dx, dy}` from params (missing = 0).
pub fn delta(p: &Value) -> Vec2 {
    vec2(finite(p.get("dx")).unwrap_or(0.0), finite(p.get("dy")).unwrap_or(0.0))
}

/// An egui key by name, forgiving about case and common aliases (`return`, `esc`, `up`).
pub fn key(name: &str) -> Option<Key> {
    Key::from_name(name).or_else(|| {
        let lower = name.to_ascii_lowercase();
        let mut cap = lower.clone();
        if let Some(f) = cap.get_mut(0..1) {
            f.make_ascii_uppercase();
        }
        if let Some(k) = Key::from_name(&cap) {
            return Some(k);
        }
        match lower.as_str() {
            "return" => Some(Key::Enter),
            "esc" => Some(Key::Escape),
            "del" => Some(Key::Delete),
            "space" | " " => Some(Key::Space),
            "up" => Some(Key::ArrowUp),
            "down" => Some(Key::ArrowDown),
            "left" => Some(Key::ArrowLeft),
            "right" => Some(Key::ArrowRight),
            _ if lower.chars().count() == 1 => Key::from_name(&lower.to_ascii_uppercase()),
            _ => None,
        }
    })
}

/// Most chords in one `ui.press` sequence.
pub const MAX_CHORDS: usize = 64;

/// `"Mod+Shift+S"` or a sequence `"G G"` (the `ashlar_command::shortcut` syntax) as egui keys.
pub fn chords(text: &str) -> Result<Vec<(Key, Modifiers)>, String> {
    let mut out = Vec::new();
    for part in text.split_whitespace() {
        if out.len() >= MAX_CHORDS {
            return Err(format!("at most {MAX_CHORDS} chords per ui.press"));
        }
        let chord = Chord::parse(part).map_err(|e| e.to_string())?;
        let k = key_to_egui(chord.key).ok_or_else(|| format!("`{part}`: egui has no such key"))?;
        let m = chord.mods;
        out.push((k, egui_modifiers(m.command, m.ctrl, m.alt, m.shift)));
    }
    if out.is_empty() {
        return Err("ui.press needs `keys`, like \"Mod+S\" or \"Escape\"".into());
    }
    Ok(out)
}

pub fn hover_frames(at: Pos2, mods: Modifiers) -> Vec<InputFrame> {
    vec![InputFrame::one(Event::PointerMoved(at), mods)]
}

pub fn click_frames(at: Pos2, button: PointerButton, count: u32, mods: Modifiers) -> Vec<InputFrame> {
    let mut v = hover_frames(at, mods);
    for _ in 0..count.clamp(1, 3) {
        v.push(InputFrame::one(Event::PointerButton { pos: at, button, pressed: true, modifiers: mods }, mods));
        v.push(InputFrame::one(Event::PointerButton { pos: at, button, pressed: false, modifiers: mods }, mods));
    }
    v
}

pub fn drag_frames(from: Pos2, to: Pos2, steps: u32, mods: Modifiers) -> Vec<InputFrame> {
    let steps = steps.clamp(1, 240);
    let b = PointerButton::Primary;
    let mut v = vec![
        InputFrame::one(Event::PointerMoved(from), mods),
        InputFrame::one(Event::PointerButton { pos: from, button: b, pressed: true, modifiers: mods }, mods),
    ];
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        v.push(InputFrame::one(Event::PointerMoved(from.lerp(to, t)), mods));
    }
    v.push(InputFrame::one(Event::PointerButton { pos: to, button: b, pressed: false, modifiers: mods }, mods));
    v
}

pub fn scroll_frames(at: Pos2, delta: Vec2, mods: Modifiers) -> Vec<InputFrame> {
    vec![
        InputFrame::one(Event::PointerMoved(at), mods),
        InputFrame::one(Event::MouseWheel { unit: MouseWheelUnit::Point, delta, phase: TouchPhase::Move, modifiers: mods }, mods),
    ]
}

/// Press and release `key`: two frames.
pub fn key_frames(key: Key, mods: Modifiers) -> Vec<InputFrame> {
    vec![
        InputFrame::one(Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: mods }, mods),
        InputFrame::one(Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: mods }, Modifiers::NONE),
    ]
}

/// Most characters one `ui.type` / `ui.fill` sends.
pub const MAX_TEXT: usize = 100_000;

pub fn type_frames(text: &str) -> Vec<InputFrame> {
    vec![InputFrame::one(Event::Text(text.chars().take(MAX_TEXT).collect()), Modifiers::NONE)]
}

/// Replace the focused field's text: select all, then type (or delete when `text` is empty),
/// then Enter when `submit`.
pub fn replace_frames(text: &str, submit: bool) -> Vec<InputFrame> {
    let mut v = key_frames(Key::A, egui_modifiers(true, false, false, false));
    if text.is_empty() {
        v.extend(key_frames(Key::Backspace, Modifiers::NONE));
    } else {
        v.extend(type_frames(text));
    }
    if submit {
        v.extend(key_frames(Key::Enter, Modifiers::NONE));
    }
    v
}

/// An AccessKit action for the egui widget whose `Id` value is `node` (focus, scroll into view).
pub fn action_frame(node: u64, action: egui::accesskit::Action) -> InputFrame {
    InputFrame::one(
        Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action,
            target_tree: egui::accesskit::TreeId::ROOT,
            target_node: egui::accesskit::NodeId(node),
            data: None,
        }),
        Modifiers::NONE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hostile_params_are_tamed() {
        assert_eq!(point(&json!({"x": "a", "y": 1})), None);
        assert_eq!(point(&json!({"x": 1e30, "y": 1})).map(|p| p.x), Some(1.0e6));
        assert_eq!(key("enter"), Some(Key::Enter));
        assert_eq!(key("a"), Some(Key::A));
        assert_eq!(key("nonsense-key"), None);
        assert_eq!(drag_frames(Pos2::ZERO, Pos2::ZERO, u32::MAX, Modifiers::NONE).len(), 243);
        assert!(modifiers(&json!({"modifiers": {"command": true}})).command);
        assert_eq!(click_frames(Pos2::ZERO, PointerButton::Primary, 99, Modifiers::NONE).len(), 7);
    }

    #[test]
    fn chords_follow_the_shortcut_syntax() {
        let c = chords("Mod+Shift+S").unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].0, Key::S);
        assert!(c[0].1.command && c[0].1.shift);
        let seq = chords("G  G Escape").unwrap();
        assert_eq!(seq.iter().map(|(k, _)| *k).collect::<Vec<_>>(), vec![Key::G, Key::G, Key::Escape]);
        assert!(chords("").is_err());
        assert!(chords("Mod+Nope").is_err());
        assert!(chords(&"A ".repeat(MAX_CHORDS + 1)).is_err());
    }

    #[test]
    fn replacing_text_selects_all_first() {
        let f = replace_frames("hi", true);
        assert!(matches!(f[0].events[0], Event::Key { key: Key::A, pressed: true, .. }));
        assert!(f.iter().any(|f| matches!(&f.events[0], Event::Text(t) if t == "hi")));
        assert!(matches!(f.last().unwrap().events[0], Event::Key { key: Key::Enter, pressed: false, .. }));
        assert!(replace_frames("", false).iter().any(|f| matches!(f.events[0], Event::Key { key: Key::Backspace, .. })));
    }
}
