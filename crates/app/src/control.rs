//! Agent control of the window: `ui.*` requests become injected egui input, answered once the
//! frame that consumed the input has run; screenshots go through `ViewportCommand::Screenshot`.

use std::collections::VecDeque;

use ashlar_control::{Request, UI_METHODS, UiHandler};
use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, Vec2, pos2, vec2};
use serde_json::Value;

/// Extra methods the windowed runtime answers.
pub const RUNTIME_METHODS: &[&str] = &["ui.move", "app.stats", "app.logs"];

/// Most input frames queued at once (a hostile `steps` cannot grow memory without bound).
const MAX_SCRIPT: usize = 4096;

/// Collects requests while `ashlar_control::handle` runs; the runtime answers them right after
/// with access to the whole app.
#[derive(Default)]
pub(crate) struct UiQueue {
    pub pending: Vec<Request>,
}

impl UiHandler for UiQueue {
    fn methods(&self) -> Vec<String> {
        UI_METHODS.iter().chain(RUNTIME_METHODS).map(|m| (*m).to_string()).collect()
    }
    fn handle(&mut self, req: Request) {
        self.pending.push(req);
    }
}

/// One frame of injected input.
#[derive(Clone, Debug, Default)]
pub(crate) struct InputFrame {
    pub events: Vec<Event>,
    pub modifiers: Modifiers,
}

/// Injected input, one frame at a time, and the requests waiting for it to be processed.
#[derive(Default)]
pub(crate) struct Script {
    frames: VecDeque<InputFrame>,
    /// Frames handed to egui so far.
    injected: u64,
    /// Frames queued so far (the index a new request waits for).
    queued: u64,
    /// Requests answered `{}` once `injected` reaches their mark (after that frame's pass).
    waiting: Vec<(u64, Request)>,
}

impl Script {
    pub fn is_busy(&self) -> bool {
        !self.frames.is_empty() || !self.waiting.is_empty()
    }

    /// Queue `frames` and answer `req` once they have all been processed.
    pub fn push(&mut self, frames: Vec<InputFrame>, req: Request) {
        if self.frames.len().saturating_add(frames.len()) > MAX_SCRIPT {
            req.fail("too much queued input; wait for earlier ui.* requests to finish");
            return;
        }
        for f in frames {
            self.frames.push_back(f);
            self.queued = self.queued.saturating_add(1);
        }
        self.waiting.push((self.queued, req));
    }

    /// Hand the next frame of input to egui (from `raw_input_hook`).
    pub fn inject(&mut self, raw: &mut egui::RawInput) -> bool {
        let Some(f) = self.frames.pop_front() else { return false };
        raw.events.push(Event::ModifiersChanged(f.modifiers));
        raw.events.extend(f.events);
        self.injected = self.injected.saturating_add(1);
        true
    }

    /// After a pass: answer requests whose input has been consumed.
    pub fn settle(&mut self) {
        let injected = self.injected;
        let (done, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting).into_iter().partition(|(mark, _)| *mark <= injected);
        self.waiting = keep;
        for (_, req) in done {
            req.ok(serde_json::json!({}));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Parameter parsing
// ---------------------------------------------------------------------------------------------

pub(crate) fn modifiers(p: &Value) -> Modifiers {
    let m = p.get("modifiers");
    let f = |k: &str| m.and_then(|m| m.get(k)).and_then(Value::as_bool).unwrap_or(false);
    let command = f("command");
    let mac = cfg!(target_os = "macos");
    Modifiers {
        alt: f("alt"),
        ctrl: f("ctrl") || (command && !mac),
        shift: f("shift"),
        mac_cmd: command && mac,
        command: command || (f("ctrl") && !mac),
    }
}

pub(crate) fn button(p: &Value) -> PointerButton {
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
pub(crate) fn point(p: &Value) -> Option<Pos2> {
    Some(pos2(finite(p.get("x"))?, finite(p.get("y"))?))
}

pub(crate) fn delta(p: &Value) -> Vec2 {
    vec2(finite(p.get("dx")).unwrap_or(0.0), finite(p.get("dy")).unwrap_or(0.0))
}

pub(crate) fn key(name: &str) -> Option<Key> {
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

// ---------------------------------------------------------------------------------------------
// Input frames
// ---------------------------------------------------------------------------------------------

pub(crate) fn click_frames(at: Pos2, button: PointerButton, count: u32, mods: Modifiers) -> Vec<InputFrame> {
    let mut v = vec![InputFrame { events: vec![Event::PointerMoved(at)], modifiers: mods }];
    for _ in 0..count.clamp(1, 3) {
        v.push(InputFrame { events: vec![Event::PointerButton { pos: at, button, pressed: true, modifiers: mods }], modifiers: mods });
        v.push(InputFrame { events: vec![Event::PointerButton { pos: at, button, pressed: false, modifiers: mods }], modifiers: mods });
    }
    v
}

pub(crate) fn drag_frames(from: Pos2, to: Pos2, steps: u32, mods: Modifiers) -> Vec<InputFrame> {
    let steps = steps.clamp(1, 240);
    let b = PointerButton::Primary;
    let mut v = vec![
        InputFrame { events: vec![Event::PointerMoved(from)], modifiers: mods },
        InputFrame { events: vec![Event::PointerButton { pos: from, button: b, pressed: true, modifiers: mods }], modifiers: mods },
    ];
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        v.push(InputFrame { events: vec![Event::PointerMoved(from.lerp(to, t))], modifiers: mods });
    }
    v.push(InputFrame { events: vec![Event::PointerButton { pos: to, button: b, pressed: false, modifiers: mods }], modifiers: mods });
    v
}

pub(crate) fn scroll_frames(at: Pos2, delta: Vec2, mods: Modifiers) -> Vec<InputFrame> {
    vec![
        InputFrame { events: vec![Event::PointerMoved(at)], modifiers: mods },
        InputFrame {
            events: vec![Event::MouseWheel { unit: MouseWheelUnit::Point, delta, phase: TouchPhase::Move, modifiers: mods }],
            modifiers: mods,
        },
    ]
}

pub(crate) fn key_frames(key: Key, mods: Modifiers) -> Vec<InputFrame> {
    vec![
        InputFrame { events: vec![Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: mods }], modifiers: mods },
        InputFrame {
            events: vec![Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: mods }],
            modifiers: Modifiers::NONE,
        },
    ]
}

pub(crate) fn type_frames(text: &str) -> Vec<InputFrame> {
    vec![InputFrame { events: vec![Event::Text(text.chars().take(100_000).collect())], modifiers: Modifiers::NONE }]
}

// ---------------------------------------------------------------------------------------------
// Screenshots
// ---------------------------------------------------------------------------------------------

/// Token passed through `ViewportCommand::Screenshot` to match the image to its request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShotToken(pub u64);

/// Crop `image` to `rect` (in points) and encode it as PNG.
pub(crate) fn encode_png(image: &egui::ColorImage, crop: Option<egui::Rect>, pixels_per_point: f32) -> Result<(Vec<u8>, u32, u32), String> {
    let [w, h] = image.size;
    let (x0, y0, x1, y1) = match crop {
        Some(r) if r.is_finite() => {
            let s = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
            let c = |v: f32, max: usize| ((v * s).round().max(0.0) as usize).min(max);
            (c(r.min.x, w), c(r.min.y, h), c(r.max.x, w), c(r.max.y, h))
        }
        _ => (0, 0, w, h),
    };
    if x1 <= x0 || y1 <= y0 {
        return Err("the element is not on screen".into());
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut rgba = Vec::with_capacity(cw.saturating_mul(ch).saturating_mul(4));
    for y in y0..y1 {
        let row = image.pixels.get(y * w + x0..y * w + x1).ok_or("screenshot smaller than its size")?;
        for p in row {
            rgba.extend_from_slice(&p.to_srgba_unmultiplied());
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, cw as u32, ch as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
    }
    Ok((out, cw as u32, ch as u32))
}

/// Standard base64 with padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, k) in idx.iter().enumerate() {
            if i <= chunk.len() {
                s.push(char::from(T.get(*k as usize).copied().unwrap_or(b'A')));
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn png_crops_in_pixels() {
        let img = egui::ColorImage::new([20, 10], vec![egui::Color32::RED; 200]);
        let (png, w, h) = encode_png(&img, Some(egui::Rect::from_min_size(pos2(1.0, 1.0), vec2(4.0, 3.0))), 2.0).unwrap();
        assert_eq!((w, h), (8, 6));
        assert_eq!(&png[1..4], b"PNG");
        assert!(encode_png(&img, Some(egui::Rect::from_min_size(pos2(50.0, 50.0), vec2(4.0, 3.0))), 1.0).is_err());
    }

    #[test]
    fn hostile_params_are_tamed() {
        assert_eq!(point(&json!({"x": "a", "y": 1})), None);
        assert_eq!(point(&json!({"x": 1e30, "y": 1})).map(|p| p.x), Some(1.0e6));
        assert_eq!(key("enter"), Some(Key::Enter));
        assert_eq!(key("a"), Some(Key::A));
        assert_eq!(key("nonsense-key"), None);
        assert_eq!(drag_frames(Pos2::ZERO, Pos2::ZERO, u32::MAX, Modifiers::NONE).len(), 243);
        assert!(modifiers(&json!({"modifiers": {"command": true}})).command);
    }

    #[test]
    fn script_answers_after_its_frames() {
        let mut s = Script::default();
        let (req, rx) = Request::new("ui.click", json!({}));
        s.push(click_frames(pos2(1.0, 1.0), PointerButton::Primary, 1, Modifiers::NONE), req);
        let mut raw = egui::RawInput::default();
        for _ in 0..2 {
            assert!(s.inject(&mut raw));
            s.settle();
            assert!(rx.try_recv().is_err());
        }
        assert!(s.inject(&mut raw));
        s.settle();
        assert!(rx.try_recv().unwrap().is_ok());
        assert!(!s.is_busy());
    }
}
