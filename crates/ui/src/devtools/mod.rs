//! The UI debugger: drive any egui app the way a browser automation tool drives a web page.
//! Snapshot the UI as an element tree with short refs (`e12`), act on refs (click, fill, press,
//! drag…), wait for conditions, read values, take screenshots, and an in-app inspector (F12).
//!
//! Everything lives in an egui [plugin](egui::Plugin) on the context, so it works in any egui app,
//! with or without `ashlar-app`'s runtime: the host only passes control requests in.
//!
//! - The element tree is egui's own AccessKit output (so plain egui widgets show up with their
//!   role, label, value and state) merged with ashlar's [`automation`](crate::automation)
//!   registry (dotted ids and component roles). The first request turns egui's AccessKit output
//!   on for the rest of the session.
//! - Refs are stable: the same widget keeps its ref across snapshots while it exists. A ref whose
//!   element is gone fails with `stale_ref`.
//! - Input is injected one frame at a time; a request is answered once the frames that carry it
//!   have run. Waits are checked on frames the app runs anyway, plus one wake at their deadline:
//!   nothing repaints in a loop.
//! - The inspector costs nothing while closed: no frames, no allocations.
//!
//! The wire protocol is `docs/control-protocol.md` (methods [`METHODS`]).
//!
//! # In an app without `ashlar-app`
//!
//! Install the plugin once, and hand every request your control dispatcher does not answer
//! itself to [`handle`] (from any thread). The reply arrives on the request's channel once the
//! frames it needs have run, so keep your event loop running as usual.
//!
//! ```
//! use ashlar_control::{Reply, Request};
//! use ashlar_ui::devtools::{self, Outcome};
//!
//! let ctx = egui::Context::default();
//! devtools::install(&ctx);
//!
//! // in your control dispatcher:
//! let (req, reply) = Request::new("ui.snapshot", serde_json::json!({"interactive": true}));
//! match devtools::handle(&ctx, req) {
//!     Outcome::Accepted => {}            // answered on a later frame
//!     Outcome::NotMine(req) => req.unknown(), // or your own methods
//! }
//!
//! // your event loop runs frames; the debugger answers from them
//! for _ in 0..3 {
//!     let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
//!         if ui.button("Save").clicked() { /* … */ }
//!     });
//!     out.textures_delta.clear();
//! }
//! let Ok(Reply::Ok(snap)) = reply.try_recv() else { panic!("no snapshot") };
//! assert!(snap["tree"].as_str().unwrap().contains(r#"button "Save""#));
//! ```
//!
//! Optional hooks: [`set_busy`] (your own pending work, for `ui.wait {for: "idle"}`),
//! [`section`] (extra inspector panels: logs, undo history…), [`set_hotkey`].

pub mod image;
pub mod input;
pub mod model;
mod overlay;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use ashlar_control::{Reply, Request};
use egui::accesskit::Action;
use egui::{Event, Key, Pos2, Rect, ViewportCommand};
use serde_json::{Value, json};

use crate::automation;
use crate::wake::{self, Reason};
use input::InputFrame;
use model::{Model, Outline, Refs, Sources, Target, rect_json};

/// The methods [`handle`] answers.
pub const METHODS: &[&str] = &[
    "ui.snapshot",
    "ui.click",
    "ui.hover",
    "ui.move",
    "ui.focus",
    "ui.fill",
    "ui.press",
    "ui.key",
    "ui.type",
    "ui.scroll",
    "ui.scroll_into_view",
    "ui.drag",
    "ui.get",
    "ui.wait",
    "ui.screenshot",
    "ui.highlight",
    "devtools.toggle",
    "devtools.state",
];

/// The methods answered with injected input (`{}` once it has been processed).
const INPUT_METHODS: &[&str] =
    &["ui.click", "ui.hover", "ui.move", "ui.focus", "ui.fill", "ui.type", "ui.press", "ui.key", "ui.scroll", "ui.scroll_into_view", "ui.drag"];

/// Frames the inspector and the debugger's timers ask for.
pub const DEVTOOLS: Reason = Reason("devtools");

/// Most requests waiting for the next frame.
const MAX_INBOX: usize = 1024;
/// Most input frames queued at once (a hostile `steps` cannot grow memory without bound).
const MAX_SCRIPT: usize = 4096;
/// Most waits and screenshots pending at once.
const MAX_WAITS: usize = 64;
const MAX_SHOTS: usize = 8;
/// A frame that asks for no repaint sooner than this counts as idle.
const IDLE_AFTER: Duration = Duration::from_millis(200);
/// A screenshot not delivered within this long fails.
const SHOT_TIMEOUT_S: f64 = 10.0;

/// What [`handle`] did with a request.
#[derive(Debug)]
pub enum Outcome {
    /// It is a debugger method: answered now or on a later frame.
    Accepted,
    /// Not a debugger method: handled by the caller.
    NotMine(Request),
}

/// Whether [`handle`] answers `method`.
pub fn handles(method: &str) -> bool {
    METHODS.contains(&method)
}

/// Add the debugger (and the automation registry) to a context. Idempotent; [`handle`] does it
/// too, but installing before the first frame saves one frame on the first request.
pub fn install(ctx: &egui::Context) {
    automation::install(ctx);
    if ctx.with_plugin::<Plugin, _>(|_| ()).is_none() {
        ctx.add_plugin(Plugin::default());
    }
}

fn with<R>(ctx: &egui::Context, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    install(ctx);
    let mut f = Some(f);
    ctx.with_plugin::<Plugin, _>(|p| f.take().map(|f| f(&mut p.st))).flatten()
}

/// Take a control request: a debugger method is queued for the next frame (which this asks
/// for) and answered there or later; anything else is handed back. Callable from any thread, but
/// not from inside a `ctx.input`/`ctx.memory` closure (they hold the context's lock).
pub fn handle(ctx: &egui::Context, req: Request) -> Outcome {
    if !handles(req.method()) {
        return Outcome::NotMine(req);
    }
    let refused = with(ctx, |s| {
        if s.inbox.len() >= MAX_INBOX {
            return Some(req);
        }
        s.inbox.push(req);
        None
    });
    match refused {
        Some(Some(req)) => req.fail("too many debugger requests waiting; let the app draw a frame"),
        Some(None) => wake::wake(ctx, Reason::CONTROL),
        None => {}
    }
    Outcome::Accepted
}

/// [`handle`] with a fresh request: the receiver gets the reply.
pub fn call(ctx: &egui::Context, method: &str, params: Value) -> Receiver<Reply> {
    let (req, rx) = Request::new(method, params);
    if let Outcome::NotMine(req) = handle(ctx, req) {
        req.unknown();
    }
    rx
}

/// Open or close the inspector.
pub fn set_open(ctx: &egui::Context, open: bool) {
    with(ctx, |s| s.overlay.set_open(open));
    wake::wake(ctx, DEVTOOLS);
}

/// Whether the inspector is open.
pub fn is_open(ctx: &egui::Context) -> bool {
    ctx.with_plugin::<Plugin, _>(|p| p.st.overlay.open).unwrap_or(false)
}

/// Tell `ui.wait {for: "idle"}` whether the app has work of its own pending (background jobs,
/// a save in flight). Cheap: call it every frame.
pub fn set_busy(ctx: &egui::Context, busy: bool) {
    ctx.with_plugin::<Plugin, _>(|p| p.st.busy = busy);
}

/// The key that opens and closes the inspector (F12 by default; `None`: only
/// `devtools.toggle` and [`set_open`]).
pub fn set_hotkey(ctx: &egui::Context, key: Option<Key>) {
    with(ctx, |s| s.hotkey = key);
}

/// Show a panel of text lines in the inspector (logs, the undo history…). `lines` runs only while
/// the inspector is open, so calling this every frame costs nothing when it is closed. Call it
/// each frame before the frame ends (sections are not kept).
pub fn section(ctx: &egui::Context, title: &'static str, lines: impl FnOnce() -> Vec<String>) {
    if !is_open(ctx) {
        return;
    }
    let lines = lines();
    ctx.with_plugin::<Plugin, _>(|p| {
        if let Some(s) = p.st.overlay.sections.iter_mut().find(|(t, _)| *t == title) {
            s.1 = lines;
        } else if p.st.overlay.sections.len() < 16 {
            p.st.overlay.sections.push((title, lines));
        }
    });
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

/// Matches a screenshot to its request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShotToken(u64);

enum ShotStage {
    /// Paint annotations and ask for the screenshot on the next frame.
    Paint,
    /// Asked at this time; annotations stay painted until the image arrives.
    Sent(f64),
    Got(Arc<egui::ColorImage>),
}

struct Shot {
    req: Request,
    token: u64,
    stage: ShotStage,
    crop: Option<Rect>,
    highlight: Option<Rect>,
    /// (ref, rect) of interactive elements, painted on the image.
    annotations: Vec<(String, Rect)>,
}

#[derive(Clone, Debug)]
enum Cond {
    Element(Target),
    Gone(Target),
    Text { text: String, target: Option<Target> },
    Enabled(Target),
    Idle,
}

struct Wait {
    req: Request,
    cond: Cond,
    start: f64,
    deadline: f64,
}

/// Injected input and the requests waiting for it.
#[derive(Default)]
struct Script {
    frames: VecDeque<InputFrame>,
    injected: u64,
    queued: u64,
    waiting: Vec<(u64, Request)>,
}

impl Script {
    fn push(&mut self, frames: Vec<InputFrame>, req: Request) {
        if self.frames.len().saturating_add(frames.len()) > MAX_SCRIPT {
            return req.fail("too much queued input; wait for earlier ui.* requests to finish");
        }
        for f in frames {
            self.frames.push_back(f);
            self.queued = self.queued.saturating_add(1);
        }
        self.waiting.push((self.queued, req));
    }

    fn settle(&mut self) {
        if self.waiting.iter().all(|(mark, _)| *mark > self.injected) {
            return;
        }
        let injected = self.injected;
        let (done, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting).into_iter().partition(|(mark, _)| *mark <= injected);
        self.waiting = keep;
        for (_, req) in done {
            req.ok(json!({}));
        }
    }

    fn is_busy(&self) -> bool {
        !self.frames.is_empty() || !self.waiting.is_empty()
    }
}

struct State {
    inbox: Vec<Request>,
    script: Script,
    waits: Vec<Wait>,
    shots: Vec<Shot>,
    shot_seq: u64,
    /// (rect, until egui time)
    highlights: Vec<(Rect, f64)>,
    model: Model,
    refs: Refs,
    generation: u64,
    busy: bool,
    hotkey: Option<Key>,
    overlay: overlay::Overlay,
}

impl Default for State {
    fn default() -> Self {
        State {
            inbox: Vec::new(),
            script: Script::default(),
            waits: Vec::new(),
            shots: Vec::new(),
            shot_seq: 0,
            highlights: Vec::new(),
            model: Model::default(),
            refs: Refs::default(),
            generation: 0,
            busy: false,
            hotkey: Some(Key::F12),
            overlay: overlay::Overlay::default(),
        }
    }
}

#[derive(Default)]
struct Plugin {
    st: State,
}

impl egui::Plugin for Plugin {
    fn debug_name(&self) -> &'static str {
        "ashlar_ui::devtools"
    }

    fn input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.st.input(ctx, raw);
    }

    fn on_end_pass(&mut self, ui: &mut egui::Ui) {
        self.st.paint(ui);
    }

    fn output_hook(&mut self, ctx: &egui::Context, out: &mut egui::FullOutput) {
        self.st.output(ctx, out);
    }
}

fn param_str<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}

fn ms(p: &Value, k: &str, default: u64, max: u64) -> u64 {
    p.get(k).and_then(Value::as_u64).unwrap_or(default).min(max)
}

impl State {
    // ------------------------------------------------------------------ frame start

    fn input(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // a pass egui runs again (discarded) gets no new input
        if ctx.output(|o| o.num_completed_passes) != 0 {
            return;
        }
        if let Some(k) = self.hotkey
            && raw.events.iter().any(|e| matches!(e, Event::Key { key, pressed: true, repeat: false, .. } if *key == k))
        {
            raw.events.retain(|e| !matches!(e, Event::Key { key, .. } if *key == k));
            self.overlay.set_open(!self.overlay.open);
        }
        if !self.shots.is_empty() {
            let shots = &mut self.shots;
            raw.events.retain(|e| {
                let Event::Screenshot { user_data, image, .. } = e else { return true };
                let Some(token) = user_data.data.as_ref().and_then(|d| d.downcast_ref::<ShotToken>()) else { return true };
                if let Some(s) = shots.iter_mut().find(|s| s.token == token.0) {
                    s.stage = ShotStage::Got(Arc::clone(image));
                }
                false
            });
        }
        if self.overlay.open {
            self.overlay.input(raw);
        }
        if let Some(f) = self.script.frames.pop_front() {
            raw.events.push(Event::ModifiersChanged(f.modifiers));
            raw.events.extend(f.events);
            self.script.injected = self.script.injected.saturating_add(1);
        }
    }

    // ------------------------------------------------------------------ painting

    fn paint(&mut self, ui: &mut egui::Ui) {
        if self.highlights.is_empty() && self.shots.is_empty() && !self.overlay.open {
            return;
        }
        let ctx = ui.ctx().clone();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, egui::Id::new("ashlar.devtools.paint")));
        let accent = egui::Color32::from_rgb(255, 80, 160);
        for (r, _) in &self.highlights {
            painter.rect_stroke(*r, 2.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Outside);
        }
        let discard = ctx.will_discard();
        for s in &mut self.shots {
            if matches!(s.stage, ShotStage::Got(_)) {
                continue;
            }
            if let Some(r) = s.highlight {
                painter.rect_stroke(r, 2.0, egui::Stroke::new(2.0, accent), egui::StrokeKind::Outside);
            }
            for (label, r) in &s.annotations {
                overlay::tag(&painter, *r, label, accent);
            }
            if matches!(s.stage, ShotStage::Paint) && !discard {
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(egui::UserData::new(ShotToken(s.token))));
                s.stage = ShotStage::Sent(ctx.input(|i| i.time));
            }
        }
        if self.overlay.open {
            overlay::show(ui, self);
        }
    }

    // ------------------------------------------------------------------ frame end

    fn output(&mut self, ctx: &egui::Context, out: &egui::FullOutput) {
        let p = &out.platform_output;
        let max_passes = ctx.options(|o| o.max_passes.get());
        if p.requested_discard() && p.num_completed_passes < max_passes {
            return; // another pass follows
        }
        self.script.settle();
        let busy_work = !self.inbox.is_empty() || !self.waits.is_empty() || !self.shots.is_empty() || !self.highlights.is_empty();
        if !busy_work && !self.overlay.open && !self.script.is_busy() {
            return;
        }
        let now = ctx.input(|i| i.time);
        if self.overlay.open {
            self.overlay.frames = self.overlay.frames.saturating_add(1);
        }
        let need_model = !self.inbox.is_empty() || !self.waits.is_empty() || self.overlay.open;
        let mut fresh = false;
        if need_model {
            match &p.accesskit_update {
                Some(u) => {
                    self.rebuild(ctx, u);
                    fresh = true;
                }
                // the tree comes with the next frame
                None => {
                    ctx.enable_accesskit();
                    wake::wake(ctx, Reason::CONTROL);
                }
            }
        }
        if fresh {
            for req in std::mem::take(&mut self.inbox) {
                self.answer(ctx, req, now);
            }
            let idle = self.is_idle(out);
            self.check_waits(now, idle);
        }
        self.finish_shots(ctx, now);
        self.highlights.retain(|(_, until)| *until > now);
        self.schedule(ctx, now);
    }

    fn is_idle(&self, out: &egui::FullOutput) -> bool {
        let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
        delay >= IDLE_AFTER && !self.script.is_busy() && !self.busy && self.shots.is_empty()
    }

    fn rebuild(&mut self, ctx: &egui::Context, update: &egui::accesskit::TreeUpdate) {
        let elements = automation::inspect(ctx).elements;
        let raw: Vec<(u64, egui::LayerId, Rect)> = ctx.viewport(|vp| {
            vp.prev_pass.widgets.layers().flat_map(|(layer, rects)| rects.iter().map(move |w| (w.id.value(), *layer, w.interact_rect))).collect()
        });
        let mut widgets = std::collections::HashMap::with_capacity(raw.len());
        let mut exclude = std::collections::HashSet::new();
        let overlay = overlay::window_id();
        if self.overlay.open {
            exclude.insert(overlay.value());
        }
        for (id, layer, r) in raw {
            if self.overlay.open && layer.id == overlay {
                exclude.insert(id);
                continue;
            }
            let r = match ctx.layer_transform_to_global(layer) {
                Some(t) => t * r,
                None => r,
            };
            widgets.insert(id, r);
        }
        let src = Sources {
            update: Some(update),
            elements: &elements,
            widgets: &widgets,
            screen: ctx.content_rect(),
            exclude: &exclude,
            pass: ctx.cumulative_pass_nr(),
        };
        self.model = Model::build(&src, &mut self.refs);
    }

    fn schedule(&self, ctx: &egui::Context, now: f64) {
        if self.script.is_busy() || !self.inbox.is_empty() {
            wake::wake(ctx, Reason::CONTROL);
        }
        // a backend delivers the image with a later frame's input: keep frames coming until it
        // has (a frame or two per screenshot)
        if !self.shots.is_empty() {
            wake::wake(ctx, Reason::SCREENSHOT);
        }
        let mut next = self.waits.iter().map(|w| w.deadline).chain(self.highlights.iter().map(|h| h.1)).fold(f64::INFINITY, f64::min);
        if let Some(t) = self.shots.iter().find_map(|s| if let ShotStage::Sent(t) = s.stage { Some(t + SHOT_TIMEOUT_S) } else { None }) {
            next = next.min(t);
        }
        if next.is_finite() {
            wake::wake_after(ctx, DEVTOOLS, Duration::try_from_secs_f64((next - now).max(0.0)).unwrap_or_default());
        }
    }

    // ------------------------------------------------------------------ requests

    fn hit(&self, t: &Target) -> Result<model::Hit, Reply> {
        self.model.resolve(t, &self.refs, self.generation).map_err(|m| Reply::err_with(m.error, m.detail))
    }

    /// The target of a request (`params.target` or flat), resolved.
    fn target(&self, p: &Value, method: &str) -> Result<(Target, model::Hit), Reply> {
        let t = Target::of(p).ok_or_else(|| Reply::err(format!("{method} needs a target: {{ref}}, {{id}}, {{text, role?}} or {{x, y}}")))?;
        let h = self.hit(&t)?;
        Ok((t, h))
    }

    fn answer(&mut self, ctx: &egui::Context, req: Request, now: f64) {
        let p = req.params().clone();
        let method = req.method().to_string();
        let r = match method.as_str() {
            "ui.snapshot" => self.snapshot(&p).map(Some),
            "ui.get" => self.get(&p).map(Some),
            m if INPUT_METHODS.contains(&m) => match self.input_frames(ctx, &method, &p) {
                Ok(frames) => return self.script.push(frames, req),
                Err(e) => Err(e),
            },
            "ui.wait" => match self.wait_cond(&p) {
                Ok(cond) => {
                    if self.waits.len() >= MAX_WAITS {
                        return req.fail("too many waits pending");
                    }
                    let timeout = ms(&p, "timeout_ms", 5000, 600_000) as f64 / 1000.0;
                    self.waits.push(Wait { req, cond, start: now, deadline: now + timeout });
                    return;
                }
                Err(e) => Err(e),
            },
            "ui.screenshot" => match self.screenshot(ctx, &p) {
                Ok(shot) => {
                    if self.shots.len() >= MAX_SHOTS {
                        return req.fail("too many screenshots pending");
                    }
                    self.shot_seq = self.shot_seq.wrapping_add(1);
                    let token = self.shot_seq;
                    let (crop, highlight, annotations) = shot;
                    self.shots.push(Shot { req, token, stage: ShotStage::Paint, crop, highlight, annotations });
                    return;
                }
                Err(e) => Err(e),
            },
            "ui.highlight" => self.target(&p, "ui.highlight").map(|(_, h)| {
                let rect = h.node.and_then(|i| self.model.get(i)).map_or(Rect::from_center_size(h.point, egui::vec2(8.0, 8.0)), |n| n.rect);
                let until = now + ms(&p, "ms", 1500, 60_000).max(50) as f64 / 1000.0;
                if self.highlights.len() >= 64 {
                    self.highlights.remove(0);
                }
                self.highlights.push((rect, until));
                Some(json!({"rect": rect_json(rect)}))
            }),
            "devtools.toggle" => {
                let open = p.get("open").and_then(Value::as_bool).unwrap_or(!self.overlay.open);
                self.overlay.set_open(open);
                Ok(Some(self.state_json()))
            }
            "devtools.state" => Ok(Some(self.state_json())),
            _ => return req.unknown(),
        };
        match r {
            Ok(Some(v)) => req.ok(v),
            Ok(None) => req.ok(json!({})),
            Err(e) => req.reply(e),
        }
    }

    fn state_json(&self) -> Value {
        json!({
            "open": self.overlay.open,
            "generation": self.generation,
            "elements": self.model.nodes.len(),
            "pending": {"input_frames": self.script.frames.len(), "waits": self.waits.len(), "screenshots": self.shots.len()},
            "repaints_while_open": self.overlay.frames,
        })
    }

    fn snapshot(&mut self, p: &Value) -> Result<Value, Reply> {
        let scope = match p.get("scope") {
            Some(s) if !s.is_null() => {
                let t = Target::parse(s).ok_or_else(|| Reply::err("`scope` is a target: {ref}, {id}, {text} or {x, y}"))?;
                self.hit(&t)?.node
            }
            _ => None,
        };
        let opts = Outline {
            interactive: p.get("interactive").and_then(Value::as_bool).unwrap_or(false),
            compact: p.get("compact").and_then(Value::as_bool).unwrap_or(false),
            depth: p.get("depth").and_then(Value::as_u64).map(|d| d.min(1000) as usize),
            scope,
        };
        self.generation = self.generation.saturating_add(1);
        let (tree, listed) = self.model.outline(&opts);
        let nodes = &self.model.nodes;
        let elements: Vec<Value> = listed.iter().filter_map(|i| nodes.get(*i)).map(|n| n.to_json(nodes)).collect();
        let focused = nodes.iter().find(|n| n.focused).map(|n| n.ref_str());
        Ok(json!({"generation": self.generation, "tree": tree, "elements": elements, "focused": focused}))
    }

    fn get(&self, p: &Value) -> Result<Value, Reply> {
        let (_, h) = self.target(p, "ui.get")?;
        let what = param_str(p, "what").unwrap_or("text");
        let Some(n) = h.node.and_then(|i| self.model.get(i)) else {
            return Err(Reply::err_with("not_found: no element there", json!({"kind": "not_found", "generation": self.generation})));
        };
        let value = match what {
            "text" => json!(n.text()),
            "name" => json!(n.name),
            "value" => json!(n.value),
            "rect" => rect_json(n.rect),
            "enabled" => json!(n.enabled),
            "visible" => json!(n.visible),
            "focused" => json!(n.focused),
            "checked" => json!(n.checked),
            "selected" => json!(n.selected),
            "role" => json!(n.role),
            "element" => n.to_json(&self.model.nodes),
            other => {
                return Err(Reply::err(format!(
                    "ui.get cannot read `{other}` (text, name, value, rect, enabled, visible, focused, checked, selected, role, element)"
                )));
            }
        };
        Ok(json!({"value": value}))
    }

    fn input_frames(&self, ctx: &egui::Context, method: &str, p: &Value) -> Result<Vec<InputFrame>, Reply> {
        let mods = input::modifiers(p);
        Ok(match method {
            "ui.click" => {
                let (_, h) = self.target(p, method)?;
                let count = ms(p, "count", 1, 3) as u32;
                input::click_frames(h.point, input::button(p), count, mods)
            }
            "ui.hover" | "ui.move" => input::hover_frames(self.target(p, method)?.1.point, mods),
            "ui.focus" => self.focus_frames(p, method)?,
            "ui.fill" => {
                let text = param_str(p, "text").ok_or_else(|| Reply::err("ui.fill needs `text`"))?;
                let mut v = if Target::of(p).is_some() { self.focus_frames(p, method)? } else { Vec::new() };
                v.extend(input::replace_frames(text, p.get("submit").and_then(Value::as_bool).unwrap_or(false)));
                v
            }
            "ui.type" => {
                let text = param_str(p, "text").ok_or_else(|| Reply::err("ui.type needs `text`"))?;
                let mut v = if Target::of(p).is_some() { self.focus_frames(p, method)? } else { Vec::new() };
                v.extend(input::type_frames(text));
                v
            }
            "ui.press" => {
                let keys = param_str(p, "keys").or_else(|| param_str(p, "key")).unwrap_or("");
                let mut v = Vec::new();
                for (k, m) in input::chords(keys).map_err(Reply::err)? {
                    v.extend(input::key_frames(k, m));
                }
                v
            }
            "ui.key" => match param_str(p, "key").and_then(input::key) {
                Some(k) => input::key_frames(k, mods),
                None => return Err(Reply::err("ui.key needs a `key` egui knows (Enter, Escape, A, F5, ArrowDown…)")),
            },
            "ui.scroll" => {
                let at = match Target::of(p) {
                    Some(t) => self.hit(&t)?.point,
                    None => ctx.content_rect().center(),
                };
                // the protocol's dy > 0 scrolls down (towards the end); egui's wheel delta is the
                // other way round
                input::scroll_frames(at, -input::delta(p), mods)
            }
            "ui.scroll_into_view" => {
                let (_, h) = self.target(p, method)?;
                match h.node.and_then(|i| self.model.get(i)).and_then(|n| n.egui) {
                    Some(id) => vec![input::action_frame(id, Action::ScrollIntoView)],
                    // an element only ashlar knows (a virtualized row) is on screen already
                    None => Vec::new(),
                }
            }
            "ui.drag" => {
                let end = |k: &str| -> Result<Pos2, Reply> {
                    let t = p
                        .get(k)
                        .and_then(Target::parse)
                        .ok_or_else(|| Reply::err(format!("ui.drag needs `{k}`: {{ref}}, {{id}}, {{text}} or {{x, y}}")))?;
                    Ok(self.hit(&t)?.point)
                };
                let steps = ms(p, "steps", 8, 240) as u32;
                input::drag_frames(end("from")?, end("to")?, steps, mods)
            }
            _ => Vec::new(),
        })
    }

    /// Give the target the keyboard: AccessKit focus for an egui widget that takes it, otherwise
    /// a click on it.
    fn focus_frames(&self, p: &Value, method: &str) -> Result<Vec<InputFrame>, Reply> {
        let (_, h) = self.target(p, method)?;
        let node = h.node.and_then(|i| self.model.get(i));
        let text_field = node.is_some_and(|n| matches!(n.role, "textbox" | "searchbox"));
        match node.and_then(|n| n.egui) {
            // a click puts the caret in a text field the way a user would
            Some(id) if !text_field && method == "ui.focus" => Ok(vec![input::action_frame(id, Action::Focus)]),
            _ => Ok(input::click_frames(h.point, egui::PointerButton::Primary, 1, egui::Modifiers::NONE)),
        }
    }

    fn wait_cond(&self, p: &Value) -> Result<Cond, Reply> {
        let target = || Target::of(p).ok_or_else(|| Reply::err("this wait needs a `target`"));
        Ok(match param_str(p, "for").unwrap_or(if p.get("text").is_some() { "text" } else { "element" }) {
            "element" | "visible" => Cond::Element(target()?),
            "gone" | "hidden" => Cond::Gone(target()?),
            "enabled" => Cond::Enabled(target()?),
            "text" => Cond::Text {
                text: param_str(p, "text").ok_or_else(|| Reply::err("ui.wait {for: \"text\"} needs `text`"))?.to_string(),
                target: Target::of(p),
            },
            "idle" => Cond::Idle,
            other => return Err(Reply::err(format!("ui.wait cannot wait for `{other}` (element, gone, text, enabled, idle)"))),
        })
    }

    fn met(&self, c: &Cond, idle: bool) -> bool {
        let node = |t: &Target| self.hit(t).ok().and_then(|h| h.node).and_then(|i| self.model.get(i));
        match c {
            Cond::Element(t) => node(t).is_some_and(|n| n.visible),
            Cond::Gone(t) => node(t).is_none_or(|n| !n.visible),
            Cond::Enabled(t) => node(t).is_some_and(|n| n.enabled),
            Cond::Text { text, target: Some(t) } => {
                node(t).is_some_and(|n| n.name.contains(text.as_str()) || n.value.as_deref().is_some_and(|v| v.contains(text.as_str())))
            }
            Cond::Text { text, target: None } => self.model.shows_text(text),
            Cond::Idle => idle,
        }
    }

    fn check_waits(&mut self, now: f64, idle: bool) {
        if self.waits.is_empty() {
            return;
        }
        let waits = std::mem::take(&mut self.waits);
        for w in waits {
            let elapsed_ms = ((now - w.start).max(0.0) * 1000.0).round() as u64;
            if self.met(&w.cond, idle) {
                w.req.ok(json!({"elapsed_ms": elapsed_ms}));
            } else if now >= w.deadline {
                let what = match &w.cond {
                    Cond::Element(t) => format!("{} to appear", t.describe()),
                    Cond::Gone(t) => format!("{} to go away", t.describe()),
                    Cond::Enabled(t) => format!("{} to be enabled", t.describe()),
                    Cond::Text { text, .. } => format!("text \"{text}\""),
                    Cond::Idle => "the app to go idle".into(),
                };
                w.req.reply(Reply::err_with(
                    format!("timeout: waited {elapsed_ms} ms for {what}"),
                    json!({"kind": "timeout", "elapsed_ms": elapsed_ms}),
                ));
            } else {
                self.waits.push(w);
            }
        }
    }

    #[allow(clippy::type_complexity)]
    fn screenshot(&self, ctx: &egui::Context, p: &Value) -> Result<(Option<Rect>, Option<Rect>, Vec<(String, Rect)>), Reply> {
        let pad = p.get("pad").and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, 1000.0) as f32;
        let crop = match Target::of(p) {
            Some(t) => {
                let h = self.hit(&t)?;
                let r = h.node.and_then(|i| self.model.get(i)).map(|n| n.rect).ok_or_else(|| Reply::err("no element there to crop to"))?;
                Some(r.expand(pad).intersect(ctx.content_rect()))
            }
            None => None,
        };
        let highlight = match p.get("highlight") {
            Some(v) if !v.is_null() => {
                let t = Target::parse(v).ok_or_else(|| Reply::err("`highlight` is a target"))?;
                let h = self.hit(&t)?;
                h.node.and_then(|i| self.model.get(i)).map(|n| n.rect)
            }
            _ => None,
        };
        let mut annotations = Vec::new();
        if p.get("annotate").and_then(Value::as_bool).unwrap_or(false) {
            let area = crop.unwrap_or(ctx.content_rect());
            for n in &self.model.nodes {
                if n.interactive && n.visible && area.intersects(n.rect) && annotations.len() < 2000 {
                    annotations.push((n.ref_str(), n.rect));
                }
            }
        }
        Ok((crop, highlight, annotations))
    }

    fn finish_shots(&mut self, ctx: &egui::Context, now: f64) {
        if self.shots.is_empty() {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let mut i = 0;
        while i < self.shots.len() {
            let done = match self.shots.get(i).map(|s| &s.stage) {
                Some(ShotStage::Got(_)) => true,
                Some(ShotStage::Sent(at)) => now - at > SHOT_TIMEOUT_S,
                _ => false,
            };
            if !done {
                i += 1;
                continue;
            }
            let s = self.shots.remove(i);
            let ShotStage::Got(img) = &s.stage else {
                s.req.fail("the screenshot never arrived (the window may be hidden)");
                continue;
            };
            match image::encode_png(img, s.crop, ppp) {
                Ok((png, w, h)) => {
                    let mut v = json!({"png": image::base64(&png), "width": w, "height": h});
                    if !s.annotations.is_empty() {
                        let origin = s.crop.map_or(Pos2::ZERO, |c| c.min);
                        let list: Vec<Value> = s
                            .annotations
                            .iter()
                            .map(|(r, rect)| {
                                let px = rect.translate(-origin.to_vec2()) * ppp;
                                json!({"ref": r, "rect": rect_json(px)})
                            })
                            .collect();
                        if let Some(o) = v.as_object_mut() {
                            o.insert("annotations".into(), Value::Array(list));
                        }
                    }
                    s.req.ok(v);
                }
                Err(e) => s.req.fail(e),
            }
        }
    }
}

#[cfg(test)]
mod tests;
