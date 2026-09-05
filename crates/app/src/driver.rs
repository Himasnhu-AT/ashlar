//! A headless event loop for tests and benches: runs a [`Runtime`] on an egui context without a
//! window, the way eframe does, on a virtual clock. A frame runs only when something asked for
//! one (input, `request_repaint`, a repaint timer), so tests can prove that an idle app draws
//! nothing.
//!
//! There is no GPU: `ui.screenshot` gets a blank image of the screen size (the PNG plumbing is
//! real, the pixels are not).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use ashlar_control::{Reply, Request};
use egui::{Event, Rect, ViewportCommand, ViewportId, pos2, vec2};
use serde_json::Value;

use crate::runtime::Runtime;
use crate::session::Document;

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

    /// Ask to close the window, as the window's close button does.
    pub fn request_close(&mut self) {
        self.close_requested = true;
    }

    /// Run one frame now, whether or not one was asked for.
    pub fn frame(&mut self) -> egui::FullOutput {
        let mut raw = egui::RawInput { screen_rect: Some(self.screen), time: Some(self.time), focused: true, ..Default::default() };
        raw.events.extend(self.input.drain(..));
        if std::mem::take(&mut self.close_requested) {
            raw.viewports.entry(ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
        }
        let ctx = self.ctx.clone();
        self.runtime.raw_input_hook(&ctx, &mut raw);
        let rt = &mut self.runtime;
        let mut out = ctx.run_ui(raw, |ui| {
            rt.logic(ui.ctx());
            rt.ui(ui);
        });
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

    /// When the next frame is due, if one is.
    fn due(&self) -> Option<f64> {
        let wake =
            self.wakes.lock().unwrap_or_else(PoisonError::into_inner).pending.iter().map(|(d, _)| self.time + d.as_secs_f64()).reduce(f64::min);
        let input = (!self.input.is_empty() || self.close_requested).then_some(self.time);
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
