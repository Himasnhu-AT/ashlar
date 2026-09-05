//! A tiny app without a window that answers the debugger methods (`ui.snapshot`, `ui.click`, …)
//! over a real control port, deterministically: `ashlar-debug demo --control <port>`. It is what
//! the tests drive, and a way to try the CLI without building an app.
//!
//! ```text
//! - window "Demo" [ref=e1]
//!   - textbox "Title" [ref=e2]
//!   - button "Save" [ref=e3]               Save: status "Saved: <title>", enables Delete
//!   - checkbox "Wrap lines" [ref=e4]       toggles
//!   - button "Delete" [ref=e5] [disabled]  removes the last item
//!   - button "Load" [ref=e6]               "Loading…", then 1 s later "Loaded" and "Item 4"
//!   - label "Ready" [ref=e7]               the status line (id `status`)
//!   - list "Items" [ref=e8]
//!     - listitem "Item 1" [ref=e9] …
//! ```

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use ashlar_command::{CommandError, CommandInfo, Dispatch};
use ashlar_control::{AppInfo, Config, Reply, Request, Server, UiHandler, handle};
use base64::Engine;
use serde_json::{Value, json};

/// What the demo answers besides the standard methods.
pub const METHODS: &[&str] = &[
    "ui.inspect",
    "ui.snapshot",
    "ui.click",
    "ui.hover",
    "ui.focus",
    "ui.fill",
    "ui.type",
    "ui.press",
    "ui.key",
    "ui.scroll",
    "ui.scroll_into_view",
    "ui.drag",
    "ui.get",
    "ui.wait",
    "ui.screenshot",
    "ui.highlight",
    "ui.menu.list",
    "ui.menu.invoke",
    "devtools.toggle",
    "devtools.state",
    "app.quit",
    "app.stats",
    "app.logs",
];

const W: u32 = 360;
const H: u32 = 260;
const LOAD_DELAY: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone)]
struct El {
    r: u32,
    id: &'static str,
    role: &'static str,
    name: String,
    value: Option<String>,
    enabled: bool,
    checked: Option<bool>,
    rect: [f32; 4],
    parent: Option<u32>,
}

impl El {
    fn interactive(&self) -> bool {
        matches!(self.role, "button" | "textbox" | "checkbox" | "listitem")
    }
}

/// The demo's state.
struct Model {
    els: Vec<El>,
    next_ref: u32,
    generation: u64,
    focused: Option<u32>,
    hovered: Option<u32>,
    title: String,
    load_at: Option<Instant>,
    devtools: bool,
    log: Vec<String>,
    requests: u64,
    /// The `raise` of the last `ui.screenshot` (null when not given).
    screenshot_raise: Value,
    quit: bool,
}

fn item_rect(i: usize) -> [f32; 4] {
    [20.0, 114.0 + 22.0 * i as f32, 192.0, 20.0]
}

impl Model {
    fn new() -> Self {
        let el = |r, id, role, name: &str, rect, parent| El {
            r,
            id,
            role,
            name: name.to_string(),
            value: None,
            enabled: true,
            checked: None,
            rect,
            parent,
        };
        let mut els = vec![
            el(1, "window", "window", "Demo", [0.0, 0.0, W as f32, H as f32], None),
            El { value: Some(String::new()), ..el(2, "title", "textbox", "Title", [16.0, 16.0, 200.0, 24.0], Some(1)) },
            el(3, "file.save", "button", "Save", [224.0, 16.0, 56.0, 24.0], Some(1)),
            El { checked: Some(false), ..el(4, "view.wrap", "checkbox", "Wrap lines", [16.0, 52.0, 120.0, 20.0], Some(1)) },
            El { enabled: false, ..el(5, "edit.delete", "button", "Delete", [144.0, 52.0, 64.0, 24.0], Some(1)) },
            el(6, "file.load", "button", "Load", [216.0, 52.0, 64.0, 24.0], Some(1)),
            el(7, "status", "label", "Ready", [16.0, 84.0, 200.0, 18.0], Some(1)),
            el(8, "items", "list", "Items", [16.0, 110.0, 200.0, 130.0], Some(1)),
        ];
        for i in 0..3u32 {
            els.push(el(9 + i, "", "listitem", &format!("Item {}", i + 1), item_rect(i as usize), Some(8)));
        }
        Self {
            els,
            next_ref: 12,
            generation: 0,
            focused: None,
            hovered: None,
            title: String::new(),
            load_at: None,
            devtools: false,
            log: Vec::new(),
            requests: 0,
            screenshot_raise: Value::Null,
            quit: false,
        }
    }

    fn note(&mut self, s: String) {
        self.log.push(s);
        if self.log.len() > 500 {
            self.log.remove(0);
        }
    }

    /// Time-based changes (the Load button's delayed result).
    fn tick(&mut self) {
        if let Some(at) = self.load_at
            && Instant::now() >= at
        {
            self.load_at = None;
            self.set_status("Loaded");
            let n = self.els.iter().filter(|e| e.role == "listitem").count();
            let r = self.next_ref;
            self.next_ref = self.next_ref.saturating_add(1);
            self.els.push(El {
                r,
                id: "",
                role: "listitem",
                name: format!("Item {}", n.saturating_add(1)),
                value: None,
                enabled: true,
                checked: None,
                rect: item_rect(n),
                parent: Some(8),
            });
            self.note("loaded".into());
        }
    }

    fn by_id(&mut self, id: &str) -> Option<&mut El> {
        self.els.iter_mut().find(|e| e.id == id)
    }

    fn set_status(&mut self, s: &str) {
        if let Some(e) = self.by_id("status") {
            e.name = s.to_string();
        }
    }

    fn get(&self, r: u32) -> Option<&El> {
        self.els.iter().find(|e| e.r == r)
    }

    /// Resolve a target (`{target: …}` or the target's fields at the top level).
    fn resolve(&self, p: &Value) -> Result<u32, Reply> {
        let t = p.get("target").filter(|t| t.is_object()).unwrap_or(p);
        self.resolve_target(t)
    }

    fn resolve_target(&self, t: &Value) -> Result<u32, Reply> {
        let s = |k: &str| t.get(k).and_then(Value::as_str);
        if let Some(r) = s("ref") {
            let n = r.strip_prefix('e').and_then(|d| d.parse::<u32>().ok());
            return n.filter(|n| self.get(*n).is_some()).ok_or_else(|| {
                Reply::err_with(
                    format!("stale_ref: no element {r} in snapshot {} (take a new snapshot)", self.generation),
                    json!({"kind": "stale_ref", "ref": r, "generation": self.generation}),
                )
            });
        }
        let found = if let Some(id) = s("id") {
            self.els.iter().find(|e| e.id == id)
        } else if let Some(text) = s("text") {
            let role = s("role");
            self.els.iter().find(|e| e.name == text && role.is_none_or(|r| r == e.role))
        } else if let (Some(x), Some(y)) = (t.get("x").and_then(Value::as_f64), t.get("y").and_then(Value::as_f64)) {
            let (x, y) = (x as f32, y as f32);
            self.els.iter().rev().find(|e| x >= e.rect[0] && y >= e.rect[1] && x < e.rect[0] + e.rect[2] && y < e.rect[1] + e.rect[3])
        } else {
            return Err(Reply::err("a target is {ref}, {id}, {text, role?} or {x, y}"));
        };
        found.map(|e| e.r).ok_or_else(|| Reply::err_with(format!("not_found: no element matches {t}"), json!({"kind": "not_found"})))
    }

    fn save(&mut self) {
        let msg = if self.title.is_empty() { "Saved".to_string() } else { format!("Saved: {}", self.title) };
        self.set_status(&msg);
        if let Some(d) = self.by_id("edit.delete") {
            d.enabled = true;
        }
        self.note(msg);
    }

    /// Activate an element (a click, or Enter on it).
    fn activate(&mut self, r: u32) -> Result<(), Reply> {
        let Some(e) = self.get(r).cloned() else { return Err(Reply::err("no such element")) };
        if !e.enabled {
            return Err(Reply::err_with(format!("`{}` is disabled", e.name), json!({"kind": "disabled"})));
        }
        if e.role == "textbox" {
            self.focused = Some(r);
        }
        match e.id {
            "file.save" => self.save(),
            "view.wrap" => {
                if let Some(c) = self.by_id("view.wrap") {
                    c.checked = Some(!c.checked.unwrap_or(false));
                }
            }
            "edit.delete" => {
                if let Some(i) = self.els.iter().rposition(|e| e.role == "listitem") {
                    self.els.remove(i);
                }
                self.set_status("Deleted");
            }
            "file.load" => {
                self.load_at = Some(Instant::now() + LOAD_DELAY);
                self.set_status("Loading…");
            }
            _ => {}
        }
        self.note(format!("activated {}", e.name));
        Ok(())
    }

    fn element_json(&self, e: &El) -> Value {
        let mut o = json!({
            "ref": format!("e{}", e.r),
            "role": e.role,
            "name": e.name,
            "enabled": e.enabled,
            "focused": self.focused == Some(e.r),
            "visible": true,
            "rect": e.rect,
        });
        if !e.id.is_empty() {
            o["id"] = json!(e.id);
        }
        if let Some(v) = &e.value {
            o["value"] = json!(v);
        }
        if let Some(c) = e.checked {
            o["checked"] = json!(c);
        }
        if let Some(p) = e.parent {
            o["parent"] = json!(format!("e{p}"));
        }
        o
    }

    fn depth_of(&self, e: &El) -> usize {
        let mut d = 0usize;
        let mut p = e.parent;
        while let Some(r) = p {
            d = d.saturating_add(1);
            p = self.get(r).and_then(|e| e.parent);
            if d > 64 {
                break;
            }
        }
        d
    }

    fn within(&self, e: &El, scope: u32) -> bool {
        let mut cur = Some(e.r);
        for _ in 0..64 {
            match cur {
                Some(r) if r == scope => return true,
                Some(r) => cur = self.get(r).and_then(|e| e.parent),
                None => return false,
            }
        }
        false
    }

    fn snapshot(&mut self, p: &Value) -> Reply {
        let interactive = p.get("interactive").and_then(Value::as_bool).unwrap_or(false);
        let max_depth = p.get("depth").and_then(Value::as_u64).map(|d| d as usize);
        let scope = match p.get("scope") {
            Some(t) if t.is_object() => match self.resolve_target(t) {
                Ok(r) => Some(r),
                Err(e) => return e,
            },
            _ => None,
        };
        self.generation = self.generation.saturating_add(1);
        let base = scope.and_then(|s| self.get(s)).map(|e| self.depth_of(e)).unwrap_or(0);
        let mut tree = String::new();
        let mut elements = Vec::new();
        for e in &self.els {
            if scope.is_some_and(|s| !self.within(e, s)) || (interactive && !e.interactive()) {
                continue;
            }
            let depth = self.depth_of(e).saturating_sub(base);
            if max_depth.is_some_and(|m| depth > m) {
                continue;
            }
            let indent = if interactive { 0 } else { depth };
            let mut line = format!("{}- {} \"{}\" [ref=e{}]", "  ".repeat(indent), e.role, e.name, e.r);
            if !e.enabled {
                line.push_str(" [disabled]");
            }
            if self.focused == Some(e.r) {
                line.push_str(" [focused]");
            }
            if e.checked == Some(true) {
                line.push_str(" [checked]");
            }
            if let Some(v) = e.value.as_ref().filter(|v| !v.is_empty()) {
                line.push_str(&format!(" value=\"{v}\""));
            }
            tree.push_str(&line);
            tree.push('\n');
            elements.push(self.element_json(e));
        }
        Reply::ok(json!({"generation": self.generation, "tree": tree, "elements": elements}))
    }

    fn get_what(&self, r: u32, what: &str) -> Reply {
        let Some(e) = self.get(r) else { return Reply::err("no such element") };
        let v = match what {
            "text" => json!(e.value.clone().unwrap_or_else(|| e.name.clone())),
            "value" => json!(e.value),
            "rect" => json!(e.rect),
            "enabled" => json!(e.enabled),
            "visible" => json!(true),
            "focused" => json!(self.focused == Some(r)),
            "checked" => json!(e.checked.unwrap_or(false)),
            "name" => json!(e.name),
            "role" => json!(e.role),
            "selected" => json!(false),
            "element" => self.element_json(e),
            other => {
                return Reply::err(format!(
                    "ui.get: unknown `what` `{other}` (text, value, name, role, rect, enabled, visible, focused, checked, selected, element)"
                ));
            }
        };
        Reply::ok(json!({"value": v}))
    }

    fn condition(&self, p: &Value) -> Result<bool, Reply> {
        let what = p.get("for").and_then(Value::as_str).unwrap_or("element");
        Ok(match what {
            "idle" => self.load_at.is_none(),
            "text" => {
                let Some(t) = p.get("text").and_then(Value::as_str) else { return Err(Reply::err("ui.wait for text needs `text`")) };
                self.els.iter().any(|e| e.name.contains(t) || e.value.as_deref().is_some_and(|v| v.contains(t)))
            }
            "element" | "gone" | "enabled" => {
                let found = match self.resolve(p) {
                    Ok(r) => Some(r),
                    Err(Reply::Err { detail: Some(d), .. }) if d.get("kind").and_then(Value::as_str) == Some("not_found") => None,
                    // A stale ref also means "not there" for `gone`.
                    Err(Reply::Err { detail: Some(d), .. }) if what == "gone" && d.get("kind").and_then(Value::as_str) == Some("stale_ref") => None,
                    Err(e) => return Err(e),
                };
                match what {
                    "element" => found.is_some(),
                    "gone" => found.is_none(),
                    _ => found.and_then(|r| self.get(r)).is_some_and(|e| e.enabled),
                }
            }
            other => return Err(Reply::err(format!("ui.wait: unknown `for` `{other}` (element, gone, text, enabled, idle)"))),
        })
    }

    fn wait(&mut self, p: &Value) -> Reply {
        let timeout = Duration::from_millis(p.get("timeout_ms").and_then(Value::as_u64).unwrap_or(5000).min(55_000));
        let start = Instant::now();
        loop {
            self.tick();
            match self.condition(p) {
                Err(e) => return e,
                Ok(true) => return Reply::ok(json!({"elapsed_ms": start.elapsed().as_millis() as u64})),
                Ok(false) if start.elapsed() >= timeout => {
                    return Reply::err_with(
                        format!("timeout: the condition did not hold within {} ms", timeout.as_millis()),
                        json!({"kind": "timeout", "elapsed_ms": start.elapsed().as_millis() as u64}),
                    );
                }
                Ok(false) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn press(&mut self, keys: &str) -> Reply {
        let chord = keys.trim();
        if chord.is_empty() {
            return Reply::err("ui.press needs `keys`, like Mod+S");
        }
        for k in chord.split_whitespace() {
            match k.to_ascii_lowercase().as_str() {
                "mod+s" | "cmd+s" | "ctrl+s" => self.save(),
                "enter" => {
                    if let Some(r) = self.focused
                        && let Err(e) = self.activate(r)
                    {
                        return e;
                    }
                    if self.focused.and_then(|r| self.get(r)).is_some_and(|e| e.role == "textbox") {
                        self.save();
                    }
                }
                "tab" => {
                    let order: Vec<u32> = self.els.iter().filter(|e| e.interactive() && e.enabled).map(|e| e.r).collect();
                    let at = self.focused.and_then(|f| order.iter().position(|r| *r == f));
                    self.focused = match at {
                        Some(i) => order.get(i.saturating_add(1)).or(order.first()).copied(),
                        None => order.first().copied(),
                    };
                }
                _ => {}
            }
            self.note(format!("pressed {k}"));
        }
        Reply::ok(json!({}))
    }

    fn screenshot(&mut self, p: &Value) -> Reply {
        self.screenshot_raise = p.get("raise").cloned().unwrap_or(Value::Null);
        let mut img = vec![0u8; (W * H * 4) as usize];
        let fill = |img: &mut Vec<u8>, r: [f32; 4], c: [u8; 4], outline: bool| {
            let (x0, y0) = (r[0].max(0.0) as u32, r[1].max(0.0) as u32);
            let (x1, y1) = (((r[0] + r[2]).max(0.0) as u32).min(W), ((r[1] + r[3]).max(0.0) as u32).min(H));
            for y in y0..y1 {
                for x in x0..x1 {
                    if outline && x > x0 && x + 1 < x1 && y > y0 && y + 1 < y1 {
                        continue;
                    }
                    let i = ((y * W + x) * 4) as usize;
                    if let Some(px) = img.get_mut(i..i + 4) {
                        px.copy_from_slice(&c);
                    }
                }
            }
        };
        for e in &self.els {
            let c = match e.role {
                "window" => [245, 245, 245, 255],
                "textbox" => [255, 255, 255, 255],
                "button" if e.enabled => [70, 120, 220, 255],
                "button" => [170, 170, 170, 255],
                "checkbox" if e.checked == Some(true) => [60, 170, 90, 255],
                "checkbox" => [200, 200, 200, 255],
                "list" => [230, 230, 236, 255],
                _ => [210, 214, 222, 255],
            };
            fill(&mut img, e.rect, c, false);
        }
        let mut annotations = Vec::new();
        if p.get("annotate").and_then(Value::as_bool) == Some(true) {
            for e in self.els.iter().filter(|e| e.interactive()) {
                fill(&mut img, [e.rect[0], e.rect[1], 8.0, 8.0], [220, 40, 40, 255], false);
                annotations.push(json!({"ref": format!("e{}", e.r), "rect": e.rect}));
            }
        }
        if let Some(h) = p.get("highlight").filter(|h| h.is_object()) {
            match self.resolve_target(h) {
                Ok(r) => {
                    if let Some(e) = self.get(r) {
                        fill(&mut img, e.rect, [255, 0, 255, 255], true);
                    }
                }
                Err(e) => return e,
            }
        }
        let pad = p.get("pad").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 100.0) as f32;
        let crop = match p.get("target").filter(|t| t.is_object()).or(p.get("id").map(|_| p)) {
            Some(t) => match self.resolve_target(t) {
                Ok(r) => self.get(r).map(|e| e.rect),
                Err(e) => return e,
            },
            None => None,
        };
        let (x0, y0, w, h) = match crop {
            Some(r) => {
                let x0 = (r[0] - pad).max(0.0) as u32;
                let y0 = (r[1] - pad).max(0.0) as u32;
                let x1 = ((r[0] + r[2] + pad) as u32).min(W);
                let y1 = ((r[1] + r[3] + pad) as u32).min(H);
                (x0, y0, x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1))
            }
            None => (0, 0, W, H),
        };
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in y0..y0 + h {
            let i = ((y * W + x0) * 4) as usize;
            out.extend_from_slice(img.get(i..i + (w * 4) as usize).unwrap_or(&[]));
        }
        let mut png_bytes = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut png_bytes, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let written = enc.write_header().and_then(|mut wr| wr.write_image_data(&out));
            if let Err(e) = written {
                return Reply::err(format!("cannot encode the picture: {e}"));
            }
        }
        let mut v = json!({"png": base64::engine::general_purpose::STANDARD.encode(&png_bytes), "width": w, "height": h});
        if !annotations.is_empty() {
            v["annotations"] = json!(annotations);
        }
        Reply::ok(v)
    }

    fn ui(&mut self, method: &str, p: &Value) -> Reply {
        self.requests = self.requests.saturating_add(1);
        self.tick();
        let target = |m: &Self| m.resolve(p);
        match method {
            "ui.snapshot" => self.snapshot(p),
            "ui.inspect" => {
                let els: Vec<Value> =
                    self.els.iter().filter(|e| !e.id.is_empty()).map(|e| json!({"id": e.id, "label": e.name, "rect": e.rect})).collect();
                Reply::ok(json!({"elements": els}))
            }
            "ui.click" => match target(self) {
                Ok(r) => {
                    let count = p.get("count").and_then(Value::as_u64).unwrap_or(1).clamp(1, 3);
                    for _ in 0..count {
                        if let Err(e) = self.activate(r) {
                            return e;
                        }
                    }
                    Reply::ok(json!({}))
                }
                Err(e) => e,
            },
            "ui.hover" | "ui.focus" | "ui.scroll_into_view" | "ui.highlight" => match target(self) {
                Ok(r) => {
                    match method {
                        "ui.hover" => self.hovered = Some(r),
                        "ui.focus" => self.focused = Some(r),
                        _ => {}
                    }
                    Reply::ok(json!({}))
                }
                Err(e) => e,
            },
            "ui.fill" => {
                let Some(text) = p.get("text").and_then(Value::as_str).map(str::to_string) else { return Reply::err("ui.fill needs `text`") };
                let r = match target(self) {
                    Ok(r) => r,
                    Err(e) => return e,
                };
                let Some(e) = self.els.iter_mut().find(|e| e.r == r) else { return Reply::err("no such element") };
                if e.role != "textbox" {
                    return Reply::err(format!("`{}` is a {}, not a text field", e.name, e.role));
                }
                e.value = Some(text.clone());
                self.title = text;
                self.focused = Some(r);
                if p.get("submit").and_then(Value::as_bool) == Some(true) {
                    self.save();
                }
                Reply::ok(json!({}))
            }
            "ui.type" => {
                let Some(text) = p.get("text").and_then(Value::as_str) else { return Reply::err("ui.type needs `text`") };
                let Some(f) = self.focused else { return Reply::err("nothing has keyboard focus (click or focus a text field first)") };
                match self.els.iter_mut().find(|e| e.r == f && e.role == "textbox") {
                    Some(e) => {
                        let v = format!("{}{text}", e.value.as_deref().unwrap_or(""));
                        e.value = Some(v.clone());
                        self.title = v;
                        Reply::ok(json!({}))
                    }
                    None => Reply::err("the focused element does not take text"),
                }
            }
            "ui.press" => self.press(p.get("keys").and_then(Value::as_str).unwrap_or("")),
            "ui.key" => self.press(p.get("key").and_then(Value::as_str).unwrap_or("")),
            "ui.scroll" => match p.get("target").filter(|t| t.is_object()) {
                Some(t) => self.resolve_target(t).map_or_else(|e| e, |_| Reply::ok(json!({}))),
                None => Reply::ok(json!({})),
            },
            "ui.drag" => {
                let (Some(from), Some(to)) = (p.get("from"), p.get("to")) else { return Reply::err("ui.drag needs `from` and `to`") };
                match (self.resolve_target(from), self.resolve_target(to)) {
                    (Ok(a), Ok(b)) => {
                        self.note(format!("dragged e{a} to e{b}"));
                        Reply::ok(json!({}))
                    }
                    (Err(e), _) | (_, Err(e)) => e,
                }
            }
            "ui.get" => match target(self) {
                Ok(r) => self.get_what(r, p.get("what").and_then(Value::as_str).unwrap_or("text")),
                Err(e) => e,
            },
            "ui.wait" => self.wait(p),
            "ui.screenshot" => self.screenshot(p),
            "ui.menu.list" => Reply::ok(json!([
                {"label": "File", "items": [
                    {"label": "Save", "id": "file.save", "shortcut": "Mod+S", "enabled": true},
                    {"label": "Load", "id": "file.load", "enabled": true}]},
                {"label": "View", "items": [{"label": "Wrap lines", "id": "view.wrap", "enabled": true}]}
            ])),
            "ui.menu.invoke" => {
                let id = p.get("id").and_then(Value::as_str).unwrap_or("");
                match self.els.iter().find(|e| e.id == id && !id.is_empty()).map(|e| e.r) {
                    Some(r) => self.activate(r).map_or_else(|e| e, |()| Reply::ok(json!({}))),
                    None => Reply::err(format!("no menu item `{id}`")),
                }
            }
            "devtools.toggle" => {
                self.devtools = p.get("open").and_then(Value::as_bool).unwrap_or(!self.devtools);
                Reply::ok(json!({"open": self.devtools}))
            }
            "devtools.state" => Reply::ok(json!({"open": self.devtools})),
            "app.quit" => {
                self.quit = true;
                Reply::ok(json!({"closing": true}))
            }
            "app.stats" => Reply::ok(
                json!({"requests": self.requests, "elements": self.els.len(), "generation": self.generation, "screenshot_raise": self.screenshot_raise}),
            ),
            "app.logs" => {
                let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(200) as usize;
                let lines: Vec<Value> =
                    self.log.iter().rev().take(limit).rev().map(|m| json!({"level": "INFO", "target": "demo", "message": m})).collect();
                Reply::ok(json!({"lines": lines}))
            }
            _ => Reply::err(format!("unknown method `{method}` (see app.version for the methods this app answers)")),
        }
    }

    fn commands(&self) -> Vec<CommandInfo> {
        let c = |id: &str, label: &str, menu: &str, shortcut: Option<&str>, enabled: bool| CommandInfo {
            id: id.into(),
            label: label.into(),
            menu: vec![menu.into()],
            shortcut: shortcut.map(String::from),
            params_schema: json!({"type": "object"}),
            params_text: None,
            enabled,
            disabled_reason: (!enabled).then(|| "nothing saved yet".to_string()),
            mutates: true,
        };
        let delete = self.els.iter().any(|e| e.id == "edit.delete" && e.enabled);
        vec![
            c("file.save", "Save", "File", Some("Mod+S"), true),
            c("file.load", "Load", "File", None, true),
            c("view.wrap", "Wrap lines", "View", None, true),
            c("edit.delete", "Delete", "Edit", None, delete),
        ]
    }
}

/// The model shared by the two halves `handle` needs (commands and UI), on the app thread only.
#[derive(Clone)]
struct Shared(Rc<RefCell<Model>>);

impl Shared {
    fn with<T>(&self, f: impl FnOnce(&mut Model) -> T, busy: T) -> T {
        match self.0.try_borrow_mut() {
            Ok(mut m) => f(&mut m),
            Err(_) => busy,
        }
    }
}

impl Dispatch for Shared {
    fn commands(&self) -> Vec<CommandInfo> {
        self.0.try_borrow().map(|m| m.commands()).unwrap_or_default()
    }

    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        let _ = params;
        let busy = Err(CommandError::Failed { id: id.into(), msg: "busy".into() });
        self.with(
            |m| {
                m.tick();
                let Some(r) = m.els.iter().find(|e| e.id == id).map(|e| e.r).filter(|_| m.commands().iter().any(|c| c.id == id)) else {
                    return Err(CommandError::Unknown { id: id.into() });
                };
                m.activate(r).map(|()| json!({"status": m.els.iter().find(|e| e.id == "status").map(|e| e.name.clone())})).map_err(|e| match e {
                    Reply::Err { error, .. } => CommandError::Disabled { id: id.into(), reason: error },
                    Reply::Ok(_) => CommandError::Failed { id: id.into(), msg: "?".into() },
                })
            },
            busy,
        )
    }
}

impl UiHandler for Shared {
    fn methods(&self) -> Vec<String> {
        METHODS.iter().map(|m| m.to_string()).collect()
    }

    fn handle(&mut self, req: Request) {
        let (method, params) = (req.method().to_string(), req.params().clone());
        let r = self.with(|m| m.ui(&method, &params), Reply::err("busy"));
        req.reply(r);
    }
}

/// A running demo app (for tests): its port and key folder. Dropping it stops the app.
pub struct Running {
    pub port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Running {
    /// Whether the app has quit (`app.quit`).
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Serve the demo on `port` (0 picks one) with its key in `key_dir`, on a thread of its own.
pub fn start(port: u16, key_dir: &Path) -> Result<Running, String> {
    let (server, rx) = Server::start(Config::new(port, key_dir), || {}).map_err(|e| e.to_string())?;
    let port = server.port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("ashlar-debug-demo".into())
        .spawn(move || {
            let shared = Shared(Rc::new(RefCell::new(Model::new())));
            let (mut d, mut ui) = (shared.clone(), shared.clone());
            let info = AppInfo::new("demo", env!("CARGO_PKG_VERSION"));
            while !stop2.load(Ordering::Relaxed) && !shared.with(|m| m.quit, true) {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(req) => handle(req, &mut d, &mut ui, &info),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            // Let the quit reply reach the client before the port closes.
            std::thread::sleep(Duration::from_millis(50));
            drop(server);
        })
        .map_err(|e| e.to_string())?;
    Ok(Running { port, stop, thread: Some(thread) })
}

/// `ashlar-debug demo [--control PORT]`: serve until `app.quit`. The key goes to
/// `ASHLAR_CONTROL_KEY_DIR`, or a folder of its own in the temporary folder.
pub fn run(port: Option<u16>, key_dir: Option<&Path>, out: &mut dyn std::io::Write) -> Result<(), String> {
    let fallback = std::env::temp_dir().join(format!("ashlar-debug-demo-{}", std::process::id()));
    let key_dir = key_dir.unwrap_or(&fallback);
    let mut running = start(port.unwrap_or(0), key_dir)?;
    let _ = writeln!(out, "demo: control port 127.0.0.1:{}, key folder {}", running.port, key_dir.display());
    let _ = out.flush();
    if let Some(t) = running.thread.take() {
        let _ = t.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_demo_answers_the_whole_debugger_vocabulary() {
        for m in ashlar_control::DEBUG_METHODS {
            assert!(METHODS.contains(m), "{m}");
        }
    }
}
