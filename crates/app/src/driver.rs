//! A headless event loop for tests and benches: runs a [`Runtime`] on an egui context without a
//! window, the way eframe does, on a virtual clock. A frame runs only when something asked for
//! one (input, `request_repaint`, a repaint timer), so tests can prove that an idle app draws
//! nothing.
//!
//! There is no GPU: `ui.screenshot` gets a blank image of the screen size (the PNG plumbing is
//! real, the pixels are not). Each frame is still tessellated, as a window would, so the frame
//! stats ([`Runtime::stats`]) hold the whole CPU frame: `pass`, `tessellate` and `frame_cpu`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ashlar_control::{Reply, Request};
use egui::{Event, Rect, ViewportCommand, ViewportId, pos2, vec2};
use serde_json::Value;

use crate::runtime::Runtime;
use crate::session::{Document, now};

/// Repaint requests made outside a pass (another thread: a job, the control server).
#[derive(Default)]
struct Wakes {
    /// (delay, pass number when requested)
    pending: Vec<(Duration, u64)>,
}

/// Drives a [`Runtime`] headless. See the module docs.
pub struct Driver<D: Document> {
    pub runtime: Runtime<D>,
    ctx: egui::Context,
    time: f64,
    wakes: Arc<Mutex<Wakes>>,
    /// Virtual time of the next frame egui asked for.
    next: Option<f64>,
    input: VecDeque<Event>,
    screen: Rect,
    frames: u64,
    closed: bool,
    close_requested: bool,
    /// Deliver `ViewportCommand::Screenshot` captures (off: a window that presents no frame).
    screenshots: bool,
    /// The window is minimized: frames run only [`Runtime::logic`], as eframe does.
    hidden: bool,
    /// The display's pixels per point (`None`: egui's default, 1).
    pixels_per_point: Option<f32>,
}

impl<D: Document> Driver<D> {
    /// A 1280×800 point screen at time 0.
    pub fn new(runtime: Runtime<D>) -> Self {
        let ctx = egui::Context::default();
        let wakes: Arc<Mutex<Wakes>> = Arc::default();
        let w = Arc::clone(&wakes);
        ctx.set_request_repaint_callback(move |info| {
            if info.viewport_id == ViewportId::ROOT {
                w.lock().unwrap_or_else(PoisonError::into_inner).pending.push((info.delay, info.current_cumulative_pass_nr));
            }
        });
        Driver {
            runtime,
            ctx,
            time: 0.0,
            wakes,
            next: Some(0.0),
            input: VecDeque::new(),
            screen: Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 800.0)),
            frames: 0,
            closed: false,
            close_requested: false,
            screenshots: true,
            hidden: false,
            pixels_per_point: None,
        }
    }

    /// Stop driving; the runtime is handed back (e.g. to [`Runtime::abandon`] it).
    pub fn into_runtime(self) -> Runtime<D> {
        self.runtime
    }

    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    pub fn set_screen(&mut self, width: f32, height: f32) {
        self.screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height));
        self.next = Some(self.time);
    }

    /// Draw as on a display with `ppp` pixels per point (2 on a Retina display), from the next
    /// frame on. Values that are not finite or not positive are ignored; others are clamped to
    /// 0.25–8.
    pub fn pixels_per_point(&mut self, ppp: f32) {
        if ppp.is_finite() && ppp > 0.0 {
            self.pixels_per_point = Some(ppp.clamp(0.25, 8.0));
            self.next = Some(self.time);
        }
    }

    /// Virtual seconds since the start.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Frames run so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The app closed its window.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Queue raw input for the next frame (it also wakes the loop).
    pub fn push_event(&mut self, e: Event) {
        self.input.push_back(e);
    }

    /// Whether screenshots are delivered (on by default). Off, the loop behaves like a window
    /// that is covered or on a locked screen: capture requests are dropped.
    pub fn set_screenshots(&mut self, deliver: bool) {
        self.screenshots = deliver;
    }

    /// Minimize the window (or show it again). Hidden, a frame runs [`Runtime::logic`] with no
    /// egui pass, the way eframe treats a minimized or covered window, then the runtime's
    /// [`Runtime::hidden_pass`] for pending debugger work; other input waits for the next shown
    /// frame. A raise from the app (`Focus`, `Minimized(false)`, `Visible(true)`) shows it again.
    pub fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
        self.next = Some(self.time);
    }

    /// Whether the window is hidden ([`Driver::set_hidden`]).
    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    /// Ask to close the window, as the window's close button does.
    pub fn request_close(&mut self) {
        self.close_requested = true;
    }

    /// Run one frame now, whether or not one was asked for.
    pub fn frame(&mut self) -> egui::FullOutput {
        if self.hidden {
            self.logic_only();
            return egui::FullOutput::default();
        }
        let mut raw = egui::RawInput { screen_rect: Some(self.screen), time: Some(self.time), focused: true, ..Default::default() };
        raw.events.extend(self.input.drain(..));
        if std::mem::take(&mut self.close_requested) {
            raw.viewports.entry(ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        }
        if let Some(ppp) = self.pixels_per_point {
            raw.viewports.entry(ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
        }
        let ctx = self.ctx.clone();
        let started = now();
        self.runtime.raw_input_hook(&ctx, &mut raw);
        let rt = &mut self.runtime;
        let mut out = ctx.run_ui(raw, |ui| {
            rt.logic(ui.ctx());
            rt.ui(ui);
        });
        let passed = now();
        // a real frame turns the shapes into meshes next (the copy stays out of the timing)
        let shapes = out.shapes.clone();
        let tess_start = now();
        let meshes = ctx.tessellate(shapes, out.pixels_per_point);
        let tess_end = now();
        drop(meshes);
        if let (Some(s), Some(p), Some(ts), Some(te)) = (started, passed, tess_start, tess_end) {
            let (pass, tess) = (p.duration_since(s), te.duration_since(ts));
            let stats = self.runtime.stats_mut();
            stats.record_pass(pass);
            stats.record_tessellate(tess);
            stats.record_frame_cpu(pass.saturating_add(tess));
        }
        out.textures_delta.clear();
        self.frames = self.frames.saturating_add(1);
        let done = self.ctx.cumulative_pass_nr();
        self.next = None;
        if let Some(vp) = out.viewport_output.get(&ViewportId::ROOT) {
            if vp.repaint_delay < Duration::MAX {
                self.next = Some(self.time + vp.repaint_delay.as_secs_f64());
            }
            for c in &vp.commands {
                match c {
                    ViewportCommand::Screenshot(_) if !self.screenshots => {}
                    ViewportCommand::Screenshot(user_data) => {
                        let [w, h] = [self.screen.width() as usize, self.screen.height() as usize];
                        let image = Arc::new(egui::ColorImage::new([w, h], vec![egui::Color32::from_gray(30); w * h]));
                        self.input.push_back(Event::Screenshot { viewport_id: ViewportId::ROOT, user_data: user_data.clone(), image });
                    }
                    ViewportCommand::Close => self.closed = true,
                    _ => {}
                }
            }
        }
        // requests made during the passes just run are in `repaint_delay`; keep later ones
        self.wakes.lock().unwrap_or_else(PoisonError::into_inner).pending.retain(|(_, nr)| *nr >= done);
        out
    }

    /// A frame of a hidden window: `logic` only, and the viewport commands it gave.
    fn logic_only(&mut self) {
        let mut raw = egui::RawInput { screen_rect: Some(self.screen), time: Some(self.time), focused: false, ..Default::default() };
        let vp = raw.viewports.entry(ViewportId::ROOT).or_default();
        vp.minimized = Some(true);
        vp.native_pixels_per_point = self.pixels_per_point;
        // screenshots the last pass asked for arrive with this one (a hidden window has none to
        // present; this stands in for the debugger's off-screen renderer)
        let (shots, rest): (Vec<Event>, Vec<Event>) = self.input.drain(..).partition(|e| matches!(e, Event::Screenshot { .. }));
        self.input.extend(rest);
        // wakes asked for from here on (also by a hidden pass, which has no repaint delay to
        // report) stay pending
        let done = self.ctx.cumulative_pass_nr();
        let (rt, time) = (&mut self.runtime, self.time);
        let out = self.ctx.run_logic(&raw, |ctx| {
            rt.logic(ctx);
            // nothing to upload textures to
            if let Some(mut out) = rt.hidden_pass(ctx, Some(time), shots) {
                out.textures_delta.clear();
            }
        });
        self.frames = self.frames.saturating_add(1);
        self.next = None;
        for c in out.viewport_commands.get(&ViewportId::ROOT).into_iter().flatten() {
            match c {
                ViewportCommand::Screenshot(user_data) if self.screenshots => {
                    let [w, h] = [self.screen.width() as usize, self.screen.height() as usize];
                    let image = Arc::new(egui::ColorImage::new([w, h], vec![egui::Color32::from_gray(30); w * h]));
                    self.input.push_back(Event::Screenshot { viewport_id: ViewportId::ROOT, user_data: user_data.clone(), image });
                    self.next = Some(self.time);
                }
                ViewportCommand::Focus | ViewportCommand::Minimized(false) | ViewportCommand::Visible(true) => {
                    self.hidden = false;
                    self.next = Some(self.time);
                }
                ViewportCommand::Close => self.closed = true,
                _ => {}
            }
        }
        self.wakes.lock().unwrap_or_else(PoisonError::into_inner).pending.retain(|(_, nr)| *nr >= done);
    }

    /// When the next frame is due, if one is.
    fn due(&self) -> Option<f64> {
        let wake =
            self.wakes.lock().unwrap_or_else(PoisonError::into_inner).pending.iter().map(|(d, _)| self.time + d.as_secs_f64()).reduce(f64::min);
        // a hidden window keeps its input for the next shown frame
        let input = ((!self.input.is_empty() && !self.hidden) || self.close_requested).then_some(self.time);
        [self.next, wake, input].into_iter().flatten().reduce(f64::min)
    }

    /// Let `secs` of virtual time pass, running every frame that is asked for (at most one per
    /// `1/120` s, like a display would). Returns the frames run.
    pub fn advance(&mut self, secs: f64) -> u64 {
        let end = self.time + secs.max(0.0);
        let start = self.frames;
        let min_dt = 1.0 / 120.0;
        let mut last_frame = f64::NEG_INFINITY;
        while !self.closed {
            let Some(due) = self.due() else { break };
            let at = due.max(last_frame + min_dt).max(self.time);
            if at > end {
                break;
            }
            self.time = at;
            self.frame();
            last_frame = at;
        }
        self.time = self.time.max(end);
        self.frames - start
    }

    /// Run frames until none is asked for (at most `max`); returns how many ran. Timers further
    /// than `horizon` seconds away do not count as pending.
    pub fn settle(&mut self, max: u64) -> u64 {
        let start = self.frames;
        let horizon = 0.25;
        while self.frames - start < max && !self.closed {
            match self.due() {
                Some(t) if t <= self.time + horizon => {
                    self.time = t.max(self.time) + 1.0 / 120.0;
                    self.frame();
                }
                _ => break,
            }
        }
        self.frames - start
    }

    /// Send a control request (as the control port would) and run frames until it is answered:
    /// one frame for the request, then only the frames something asks for, on the virtual clock
    /// (a request that needs frames nobody asks for is reported, not papered over by polling).
    pub fn request(&mut self, method: &str, params: Value) -> Reply {
        let (req, rx) = Request::new(method, params);
        self.runtime.inject_request(req);
        self.time += 1.0 / 120.0;
        self.frame();
        let limit = self.time + 600.0;
        for _ in 0..10_000 {
            if let Ok(r) = rx.try_recv() {
                return r;
            }
            if self.closed {
                break;
            }
            let Some(due) = self.due().filter(|d| *d <= limit) else { break };
            self.time = due.max(self.time + 1.0 / 120.0);
            self.frame();
        }
        rx.try_recv().unwrap_or_else(|_| Reply::err(format!("`{method}` was not answered, and no frame is pending")))
    }

    /// [`Driver::request`] expecting success.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.request(method, params).into_result().map_err(|(e, _)| e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::App;

    fn driver() -> Driver<i64> {
        Driver::new(App::<i64>::new("drivertest", "0.0.0").view(|ui, _cx| drop(ui.label("hello"))).runtime())
    }

    #[test]
    fn frames_are_measured_by_phase_at_the_display_scale() {
        let mut d = driver();
        d.pixels_per_point(2.0);
        d.pixels_per_point(f32::NAN);
        d.pixels_per_point(-1.0);
        let out = d.frame();
        assert_eq!(out.pixels_per_point, 2.0);
        assert!(!out.shapes.is_empty(), "the shapes are still handed back");
        assert_eq!(d.ctx().pixels_per_point(), 2.0);
        let s = d.runtime.stats().summary();
        assert_eq!((s.window, s.pass.window, s.tessellate.window, s.frame_cpu.window), (1, 1, 1, 1));
        assert!(s.frame_cpu.max_ms >= s.pass.max_ms && s.frame_cpu.max_ms >= s.tessellate.max_ms, "{s:?}");
        assert!(s.pass.max_ms >= s.max_ms, "the pass holds the UI: {s:?}");
    }
}
