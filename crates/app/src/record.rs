//! Record and replay a session's input: `ASHLAR_RECORD=path` (or [`crate::Runtime::record_to`])
//! appends one JSON line per frame with its time, screen and input events, and one per control
//! request served, and [`crate::Driver::replay`] plays such a file into a fresh driver.
//!
//! ```text
//! {"ashlar_record":1}
//! {"events":[{"b":0,"down":true,"k":"button","m":0,"x":10.0,"y":20.0}],"screen":[800.0,600.0],"t":0.5}
//! {"req":{"method":"engine.execute","params":{"command":"app.undo"}}}
//! ```
//!
//! Lines are written by a background thread; if it falls behind, lines are dropped and the next
//! frame line counts them (`"dropped"`). The file is a bounded ring: when it passes half of [`MAX_BYTES`] it becomes `<path>.old`
//! (replacing an older one) and a new file starts, so the pair never holds more than
//! [`MAX_BYTES`]; replay reads `<path>.old` first when it is there. Events that cannot be replayed
//! (screenshots, IME, touch, AccessKit actions) are left out; an agent's UI input arrives as
//! control requests, which are recorded. Write errors stop the recording with a log warning.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, TouchPhase, pos2, vec2};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use serde_json::{Map, Value, json};

/// The most a recording (the file and its `.old` half) holds.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// The longest line replay reads; longer lines are skipped.
const MAX_LINE: usize = 4 * 1024 * 1024;

/// Lines queued for the writer thread before new ones are dropped (and counted).
const QUEUE: usize = 8192;

/// Appends frames and requests to a recording: lines are built on the UI thread and written by a
/// background thread, so recording does no file I/O on the UI thread.
pub(crate) struct Recorder {
    tx: Option<SyncSender<String>>,
    writer: Option<std::thread::JoinHandle<()>>,
    /// Lines lost because the writer fell behind (reported in the next frame line).
    dropped: u64,
}

impl Recorder {
    /// The recorder `ASHLAR_RECORD` asks for, if any.
    pub(crate) fn from_env() -> Option<Recorder> {
        let path = std::env::var_os("ASHLAR_RECORD").filter(|v| !v.is_empty())?;
        match Recorder::create(Path::new(&path), MAX_BYTES) {
            Ok(r) => Some(r),
            Err(e) => {
                log::warn!("ASHLAR_RECORD: cannot write {}: {e}", Path::new(&path).display());
                None
            }
        }
    }

    /// Start a recording at `path` (replacing it) holding at most `cap` bytes.
    pub(crate) fn create(path: &Path, cap: u64) -> std::io::Result<Recorder> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut sink = Sink { path: path.to_path_buf(), out: BufWriter::new(File::create(path)?), written: 0, half: (cap / 2).max(1) };
            // an `.old` half from an earlier recording is not part of this one
            let _ = std::fs::remove_file(old_path(path));
            sink.header()?;
            let (tx, rx) = std::sync::mpsc::sync_channel::<String>(QUEUE);
            let writer = std::thread::Builder::new().name("ashlar-record".into()).spawn(move || {
                let run = || sink.run(&rx);
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => log::warn!("recording stopped: {e}"),
                    Err(_) => log::warn!("the recording writer panicked"),
                }
            })?;
            Ok(Recorder { tx: Some(tx), writer: Some(writer), dropped: 0 })
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (path, cap);
            Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no recording on the web"))
        }
    }

    /// Queue a line. Returns false when the writer has stopped (the recording should end).
    fn send(&mut self, line: String) -> bool {
        let Some(tx) = &self.tx else { return false };
        match tx.try_send(line) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.dropped = self.dropped.saturating_add(1);
                true
            }
            Err(TrySendError::Disconnected(_)) => {
                self.tx = None;
                false
            }
        }
    }

    /// Record a frame starting at `raw` (its time, screen and events). Returns false when the
    /// recording failed and should stop.
    pub(crate) fn frame(&mut self, raw: &egui::RawInput) -> bool {
        let mut line = Map::new();
        line.insert("t".into(), json!(raw.time.unwrap_or(0.0)));
        if let Some(s) = raw.screen_rect {
            line.insert("screen".into(), json!([s.width(), s.height()]));
        }
        if let Some(ppp) = raw.viewports.get(&egui::ViewportId::ROOT).and_then(|v| v.native_pixels_per_point) {
            line.insert("ppp".into(), json!(ppp));
        }
        let events: Vec<Value> = raw.events.iter().filter_map(encode).collect();
        if !events.is_empty() {
            line.insert("events".into(), Value::Array(events));
        }
        if self.dropped > 0 {
            line.insert("dropped".into(), json!(self.dropped));
        }
        self.send(Value::Object(line).to_string())
    }

    /// Record a control request served during the current frame.
    pub(crate) fn request(&mut self, method: &str, params: &Value) -> bool {
        self.send(json!({ "req": { "method": method, "params": params } }).to_string())
    }
}

impl Drop for Recorder {
    /// Close the queue and wait for the writer to flush it, so the file is complete once the
    /// runtime is gone.
    fn drop(&mut self) {
        self.tx = None;
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
    }
}

/// The writer thread's end: the file and its rotation.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct Sink {
    path: PathBuf,
    out: BufWriter<File>,
    written: u64,
    /// Bytes per file before it rotates to `.old`.
    half: u64,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl Sink {
    fn run(&mut self, rx: &Receiver<String>) -> std::io::Result<()> {
        while let Ok(first) = rx.recv() {
            self.line(&first)?;
            // write what is queued, then flush once
            while let Ok(more) = rx.try_recv() {
                self.line(&more)?;
            }
            self.out.flush()?;
        }
        self.out.flush()
    }

    fn header(&mut self) -> std::io::Result<()> {
        self.line(&json!({ "ashlar_record": 1 }).to_string())
    }

    fn line(&mut self, text: &str) -> std::io::Result<()> {
        // a frame line starts a new frame: rotate between frames, never inside one
        if self.written >= self.half && !text.starts_with("{\"req\"") {
            self.rotate()?;
        }
        self.out.write_all(text.as_bytes())?;
        self.out.write_all(b"\n")?;
        self.written = self.written.saturating_add(text.len() as u64 + 1);
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.out.flush()?;
        std::fs::rename(&self.path, old_path(&self.path))?;
        self.out = BufWriter::new(File::create(&self.path)?);
        self.written = 0;
        self.header()
    }
}

fn old_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".old");
    PathBuf::from(s)
}

/// One recorded frame: its time, screen, events and the control requests it served.
#[derive(Debug, Default)]
pub(crate) struct Frame {
    pub time: f64,
    pub screen: Option<(f32, f32)>,
    pub ppp: Option<f32>,
    pub events: Vec<Event>,
    pub requests: Vec<(String, Value)>,
}

/// Read a recording (`<path>.old` first, when there). Unreadable lines are skipped.
pub(crate) fn read(path: &Path) -> std::io::Result<Vec<Frame>> {
    let mut frames = Vec::new();
    let old = old_path(path);
    if old.exists() {
        read_file(&old, &mut frames)?;
    }
    read_file(path, &mut frames)?;
    Ok(frames)
}

fn read_file(path: &Path, frames: &mut Vec<Frame>) -> std::io::Result<()> {
    let mut input = BufReader::new(File::open(path)?);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // bounded: a line longer than MAX_LINE is cut and fails to parse
        let n = std::io::Read::take(&mut input, MAX_LINE as u64).read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Ok(());
        }
        if buf.last() != Some(&b'\n') && n >= MAX_LINE {
            // skip the rest of an over-long line
            let mut rest = Vec::new();
            input.read_until(b'\n', &mut rest)?;
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&buf) else { continue };
        if let Some(req) = v.get("req") {
            let method = req.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            if let Some(f) = frames.last_mut() {
                f.requests.push((method, params));
            } else {
                frames.push(Frame { requests: vec![(method, params)], ..Frame::default() });
            }
        } else if let Some(t) = v.get("t").and_then(Value::as_f64) {
            let screen = v.get("screen").and_then(Value::as_array).and_then(|a| Some((num(a.first()?)?, num(a.get(1)?)?)));
            let ppp = v.get("ppp").and_then(num);
            let events = v.get("events").and_then(Value::as_array).map(|a| a.iter().filter_map(decode).collect()).unwrap_or_default();
            frames.push(Frame { time: t, screen, ppp, events, requests: Vec::new() });
        }
    }
}

fn num(v: &Value) -> Option<f32> {
    v.as_f64().map(|f| f as f32).filter(|f| f.is_finite())
}

fn mods(m: Modifiers) -> u8 {
    u8::from(m.alt) | u8::from(m.ctrl) << 1 | u8::from(m.shift) << 2 | u8::from(m.mac_cmd) << 3 | u8::from(m.command) << 4
}

fn mods_of(v: &Value) -> Modifiers {
    let b = v.get("m").and_then(Value::as_u64).unwrap_or(0);
    Modifiers { alt: b & 1 != 0, ctrl: b & 2 != 0, shift: b & 4 != 0, mac_cmd: b & 8 != 0, command: b & 16 != 0 }
}

fn button_index(b: PointerButton) -> u8 {
    match b {
        PointerButton::Primary => 0,
        PointerButton::Secondary => 1,
        PointerButton::Middle => 2,
        PointerButton::Extra1 => 3,
        PointerButton::Extra2 => 4,
    }
}

fn phase_name(p: TouchPhase) -> &'static str {
    match p {
        TouchPhase::Start => "start",
        TouchPhase::Move => "move",
        TouchPhase::End => "end",
        TouchPhase::Cancel => "cancel",
    }
}

/// An event as JSON; `None` for events replay cannot reproduce.
fn encode(e: &Event) -> Option<Value> {
    Some(match e {
        Event::Copy => json!({ "k": "copy" }),
        Event::Cut => json!({ "k": "cut" }),
        Event::Paste(s) => json!({ "k": "paste", "s": s }),
        Event::Text(s) => json!({ "k": "text", "s": s }),
        Event::Key { key, physical_key, pressed, repeat, modifiers } => json!({
            "k": "key", "key": key.name(), "phys": physical_key.map(Key::name),
            "down": pressed, "repeat": repeat, "m": mods(*modifiers),
        }),
        Event::ModifiersChanged(m) => json!({ "k": "mods", "m": mods(*m) }),
        Event::PointerMoved(p) => json!({ "k": "move", "x": p.x, "y": p.y }),
        Event::MouseMoved(d) => json!({ "k": "motion", "x": d.x, "y": d.y }),
        Event::PointerButton { pos, button, pressed, modifiers } => {
            json!({ "k": "button", "x": pos.x, "y": pos.y, "b": button_index(*button), "down": pressed, "m": mods(*modifiers) })
        }
        Event::PointerGone => json!({ "k": "gone" }),
        Event::Zoom(f) => json!({ "k": "zoom", "f": f }),
        Event::Rotate(f) => json!({ "k": "rotate", "f": f }),
        Event::MouseWheel { unit, delta, phase, modifiers } => {
            let unit = match unit {
                MouseWheelUnit::Point => "point",
                MouseWheelUnit::Line => "line",
                MouseWheelUnit::Page => "page",
            };
            json!({ "k": "wheel", "unit": unit, "x": delta.x, "y": delta.y, "phase": phase_name(*phase), "m": mods(*modifiers) })
        }
        Event::WindowFocused(on) => json!({ "k": "focus", "on": on }),
        _ => return None,
    })
}

/// A recorded event; `None` when it is malformed (hostile files are skipped, not trusted).
fn decode(v: &Value) -> Option<Event> {
    let f = |k: &str| v.get(k).and_then(num);
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some(match v.get("k")?.as_str()? {
        "copy" => Event::Copy,
        "cut" => Event::Cut,
        "paste" => Event::Paste(s("s")?),
        "text" => Event::Text(s("s")?),
        "key" => Event::Key {
            key: Key::from_name(v.get("key")?.as_str()?)?,
            physical_key: v.get("phys").and_then(Value::as_str).and_then(Key::from_name),
            pressed: b("down"),
            repeat: b("repeat"),
            modifiers: mods_of(v),
        },
        "mods" => Event::ModifiersChanged(mods_of(v)),
        "move" => Event::PointerMoved(pos2(f("x")?, f("y")?)),
        "motion" => Event::MouseMoved(vec2(f("x")?, f("y")?)),
        "button" => Event::PointerButton {
            pos: pos2(f("x")?, f("y")?),
            button: match v.get("b")?.as_u64()? {
                0 => PointerButton::Primary,
                1 => PointerButton::Secondary,
                2 => PointerButton::Middle,
                3 => PointerButton::Extra1,
                4 => PointerButton::Extra2,
                _ => return None,
            },
            pressed: b("down"),
            modifiers: mods_of(v),
        },
        "gone" => Event::PointerGone,
        "zoom" => Event::Zoom(f("f")?),
        "rotate" => Event::Rotate(f("f")?),
        "wheel" => Event::MouseWheel {
            unit: match v.get("unit")?.as_str()? {
                "point" => MouseWheelUnit::Point,
                "line" => MouseWheelUnit::Line,
                "page" => MouseWheelUnit::Page,
                _ => return None,
            },
            delta: vec2(f("x")?, f("y")?),
            phase: match v.get("phase").and_then(Value::as_str).unwrap_or("move") {
                "start" => TouchPhase::Start,
                "end" => TouchPhase::End,
                "cancel" => TouchPhase::Cancel,
                _ => TouchPhase::Move,
            },
            modifiers: mods_of(v),
        },
        "focus" => Event::WindowFocused(b("on")),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn events_round_trip() {
        let m = Modifiers { shift: true, command: true, ..Modifiers::NONE };
        let events = vec![
            Event::Text("é ok".into()),
            Event::Key { key: Key::Z, physical_key: Some(Key::Z), pressed: true, repeat: false, modifiers: m },
            Event::PointerMoved(pos2(1.5, 2.0)),
            Event::PointerButton { pos: pos2(3.0, 4.0), button: PointerButton::Secondary, pressed: false, modifiers: m },
            Event::MouseWheel { unit: MouseWheelUnit::Line, delta: vec2(0.0, -3.0), phase: TouchPhase::Move, modifiers: m },
            Event::Paste("x".into()),
            Event::WindowFocused(true),
        ];
        for e in events {
            assert_eq!(decode(&encode(&e).unwrap()), Some(e));
        }
    }

    #[test]
    fn hostile_lines_are_skipped() {
        for bad in
            [json!({}), json!({"k": "button", "x": 1, "y": 1, "b": 9}), json!({"k": "key", "key": "NoSuchKey"}), json!({"k": "move", "x": "a"})]
        {
            assert_eq!(decode(&bad), None);
        }
    }

    #[test]
    fn the_ring_rotates_and_reads_back_in_order() {
        let dir = std::env::temp_dir().join(format!("ashlar-record-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rec.jsonl");
        let mut r = Recorder::create(&path, 600).unwrap();
        for i in 0..40 {
            let raw = egui::RawInput { time: Some(f64::from(i)), events: vec![Event::Text(format!("{i}"))], ..Default::default() };
            assert!(r.frame(&raw));
        }
        drop(r);
        let total = std::fs::metadata(&path).unwrap().len() + std::fs::metadata(old_path(&path)).unwrap().len();
        assert!(total <= 600 + 200, "the ring stays near its cap: {total}");
        let frames = read(&path).unwrap();
        assert!(!frames.is_empty() && frames.len() < 40, "old frames were dropped");
        assert!(frames.windows(2).all(|w| w[0].time < w[1].time), "in order");
        assert_eq!(frames.last().map(|f| f.time), Some(39.0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
