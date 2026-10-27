//! Give memory back when nobody is using the app: an idle / background trim policy, and the
//! numbers that show where memory goes.
//!
//! [`IdleTrim`] watches activity (input, and whatever the app counts as busy: playing, rendering,
//! a job running) and the window's state (focused, minimized, occluded). It fires once per quiet
//! spell: [`Trim::Idle`] after [`IdlePolicy::idle_after`] with no activity, or
//! [`Trim::Background`] after [`IdlePolicy::background_after`] with the window in the background.
//! The app then shrinks its caches to what redraws the screen and calls
//! `ashlar_native::memory::release_free_memory` so the freed pages leave the process. Anything
//! trimmed is rebuilt on demand. Between the trigger and the next activity it asks for no frames:
//! an idle app does zero work.
//!
//! eframe calls `App::logic` while the window is hidden but keeps the input of the last shown
//! frame, so [`Activity::of`] ignores input while hidden, and the policy runs on the app's own
//! monotonic clock (seconds), not `InputState::time`.
//!
//! ```
//! use ashlar_ui::idle::{Activity, IdleTrim, Trim};
//! let mut idle = IdleTrim::default();
//! assert_eq!(idle.step(0.0, Activity { busy: true, background: false }).trim, None);
//! let step = idle.step(1.0, Activity::default());
//! assert_eq!(step.wake_in, Some(std::time::Duration::from_secs(29)));
//! assert_eq!(idle.step(30.0, Activity::default()).trim, Some(Trim::Idle));
//! assert_eq!(idle.step(90.0, Activity::default()).trim, None); // once per quiet spell
//! ```
//!
//! [`texture_memory`] totals egui's textures (the font atlas, images, thumbnails) and
//! [`MemoryReport`] gives the apps one JSON shape for their `perf.stats`.

use std::time::Duration;

use serde::Serialize;

use crate::wake::{self, Reason};

/// When an app counts as idle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IdlePolicy {
    /// No input and nothing busy for this long: [`Trim::Idle`].
    pub idle_after: Duration,
    /// The window unfocused, minimized or hidden (and nothing busy) for this long:
    /// [`Trim::Background`].
    pub background_after: Duration,
}

impl Default for IdlePolicy {
    fn default() -> Self {
        IdlePolicy { idle_after: Duration::from_secs(30), background_after: Duration::from_secs(10) }
    }
}

/// Why the app trims.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Trim {
    /// Nothing happened for [`IdlePolicy::idle_after`].
    Idle,
    /// The window was in the background for [`IdlePolicy::background_after`].
    Background,
}

/// What the app is doing this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    /// Input arrived, or the app is working for the user (playing, rendering, a job running).
    pub busy: bool,
    /// The window is unfocused, minimized or covered.
    pub background: bool,
}

impl Activity {
    /// This frame's activity from egui's input (`app_busy`: what the app itself counts as busy).
    /// Input of a hidden window is the last shown frame's and is ignored.
    pub fn of(ctx: &egui::Context, app_busy: bool) -> Activity {
        ctx.input(|i| {
            let v = i.viewport();
            let hidden = v.minimized == Some(true) || v.occluded == Some(true);
            let background = hidden || v.focused == Some(false);
            let input = !hidden && (!i.events.is_empty() || i.pointer.any_down() || i.pointer.is_moving());
            Activity { busy: app_busy || input, background }
        })
    }
}

/// What [`IdleTrim::step`] decided.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Step {
    /// Trim now.
    pub trim: Option<Trim>,
    /// Ask for a frame this long from now to check again (`None`: nothing to wait for).
    pub wake_in: Option<Duration>,
}

/// The idle / background trim state machine (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct IdleTrim {
    pub policy: IdlePolicy,
    last_busy: Option<f64>,
    background_since: Option<f64>,
    trimmed: bool,
    trims: u64,
    last_trim: Option<Trim>,
}

impl IdleTrim {
    pub fn new(policy: IdlePolicy) -> Self {
        IdleTrim { policy, ..Default::default() }
    }

    /// Feed one frame's activity at `now` (monotonic seconds). The first call starts the clock.
    pub fn step(&mut self, now: f64, a: Activity) -> Step {
        if a.busy || self.last_busy.is_none() {
            self.last_busy = Some(now);
            self.trimmed = false;
        }
        if a.background {
            self.background_since.get_or_insert(now);
        } else {
            self.background_since = None;
        }
        if self.trimmed {
            // wait for activity
            return Step::default();
        }
        let idle_at = self.last_busy.unwrap_or(now) + self.policy.idle_after.as_secs_f64();
        let bg_at = self.background_since.map(|t| t.max(self.last_busy.unwrap_or(t)) + self.policy.background_after.as_secs_f64());
        let (at, why) = match bg_at {
            Some(b) if b < idle_at => (b, Trim::Background),
            _ => (idle_at, Trim::Idle),
        };
        if now >= at {
            self.trimmed = true;
            self.trims = self.trims.saturating_add(1);
            self.last_trim = Some(why);
            return Step { trim: Some(why), wake_in: None };
        }
        Step { trim: None, wake_in: Some(Duration::from_secs_f64(at - now)) }
    }

    /// [`Self::step`] with egui: reads [`Activity::of`] and keeps one [`Reason::IDLE`] wake at the
    /// next deadline (none once trimmed).
    pub fn update(&mut self, ctx: &egui::Context, now: f64, app_busy: bool) -> Option<Trim> {
        let step = self.step(now, Activity::of(ctx, app_busy));
        match step.wake_in {
            Some(d) => wake::reschedule(ctx, Reason::IDLE, d),
            None => wake::Scheduler::of(ctx).cancel(Reason::IDLE),
        }
        step.trim
    }

    /// Trimmed and still quiet.
    pub fn trimmed(&self) -> bool {
        self.trimmed
    }

    /// Trims so far, and the last one's kind (for `perf.stats`).
    pub fn trims(&self) -> (u64, Option<Trim>) {
        (self.trims, self.last_trim)
    }
}

/// egui's textures: how many and their bytes (CPU-side size of the pixels; the GPU copy costs
/// about the same again).
pub fn texture_memory(ctx: &egui::Context) -> (usize, u64) {
    let tex = ctx.tex_manager();
    let tex = tex.read();
    let mut bytes = 0u64;
    let mut n = 0usize;
    for (_, meta) in tex.allocated() {
        n += 1;
        bytes = bytes.saturating_add(meta.bytes_used() as u64);
    }
    (n, bytes)
}

/// Where memory goes, for `perf.stats`: named byte counts (an app's caches) and the process
/// footprint the OS charges. Serialized as
/// `{"footprint": n, "resident": n, "accounted": n, "parts": {name: bytes}}`.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MemoryReport {
    /// What the system charges the process (None where unknown).
    pub footprint: Option<u64>,
    /// Resident pages (None where unknown).
    pub resident: Option<u64>,
    /// The sum of `parts`.
    pub accounted: u64,
    /// Bytes per cache, in insertion order.
    pub parts: serde_json::Map<String, serde_json::Value>,
}

impl MemoryReport {
    /// A report with the process numbers (from `ashlar_native::memory::process_memory`).
    pub fn new(footprint: Option<u64>, resident: Option<u64>) -> Self {
        MemoryReport { footprint, resident, ..Default::default() }
    }

    /// Add a part (`name` in camelCase).
    pub fn part(mut self, name: &str, bytes: u64) -> Self {
        self.accounted = self.accounted.saturating_add(bytes);
        self.parts.insert(name.to_string(), bytes.into());
        self
    }

    /// Add several parts (`(name, bytes)`, e.g. an engine's cache list).
    pub fn parts<'a>(self, parts: impl IntoIterator<Item = (&'a str, u64)>) -> Self {
        parts.into_iter().fold(self, |r, (name, bytes)| r.part(name, bytes))
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

/// Frames per second over the last second of frames, `0` when no frame ran in that second (an
/// app that paints on demand has no frame rate while idle; a smoothed value would show the last
/// burst's rate forever).
#[derive(Clone, Debug, Default)]
pub struct FrameRate {
    times: std::collections::VecDeque<f64>,
}

impl FrameRate {
    /// Frames kept at most (one second at 480 Hz).
    const MAX: usize = 480;

    /// A frame ran at `now` (seconds).
    pub fn frame(&mut self, now: f64) {
        if self.times.len() >= Self::MAX {
            self.times.pop_front();
        }
        self.times.push_back(now);
    }

    /// Frames in the second before `now`.
    pub fn fps(&self, now: f64) -> f64 {
        let recent: Vec<f64> = self.times.iter().copied().filter(|t| now - *t <= 1.0 && *t <= now).collect();
        match (recent.first(), recent.last()) {
            (Some(a), Some(b)) if recent.len() >= 2 && b > a => (recent.len() - 1) as f64 / (b - a),
            _ => 0.0,
        }
    }

    /// Seconds since the last frame (`None` before the first).
    pub fn since_last(&self, now: f64) -> Option<f64> {
        self.times.back().map(|t| (now - t).max(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUIET: Activity = Activity { busy: false, background: false };
    const BUSY: Activity = Activity { busy: true, background: false };
    const BACK: Activity = Activity { busy: false, background: true };

    #[test]
    fn quiet_for_the_idle_time_trims_once_until_activity() {
        let mut t = IdleTrim::default();
        assert_eq!(t.step(0.0, BUSY), Step { trim: None, wake_in: Some(Duration::from_secs(30)) });
        assert_eq!(t.step(10.0, QUIET).wake_in, Some(Duration::from_secs(20)));
        assert_eq!(t.step(29.9, QUIET).trim, None);
        assert_eq!(t.step(30.0, QUIET).trim, Some(Trim::Idle));
        assert!(t.trimmed());
        // trimmed: no more frames asked for, no second trim
        assert_eq!(t.step(100.0, QUIET), Step::default());
        // activity re-arms it
        t.step(101.0, BUSY);
        assert!(!t.trimmed());
        assert_eq!(t.step(131.0, QUIET).trim, Some(Trim::Idle));
        assert_eq!(t.trims(), (2, Some(Trim::Idle)));
    }

    #[test]
    fn the_background_trims_sooner_and_busy_work_holds_it_off() {
        let mut t = IdleTrim::default();
        t.step(0.0, BUSY);
        assert_eq!(t.step(2.0, BACK).wake_in, Some(Duration::from_secs(10)));
        // rendering in the background keeps the caches
        assert_eq!(t.step(11.0, Activity { busy: true, background: true }).trim, None);
        assert_eq!(t.step(20.0, BACK).trim, None);
        assert_eq!(t.step(21.0, BACK).trim, Some(Trim::Background));
        // focus back without input: still trimmed until something happens
        assert_eq!(t.step(22.0, QUIET), Step::default());
    }

    #[test]
    fn the_first_step_starts_the_clock() {
        let mut t = IdleTrim::new(IdlePolicy { idle_after: Duration::from_secs(5), background_after: Duration::from_secs(1) });
        assert_eq!(t.step(1000.0, QUIET).wake_in, Some(Duration::from_secs(5)));
        assert_eq!(t.step(1005.0, QUIET).trim, Some(Trim::Idle));
    }

    #[test]
    fn a_hidden_window_ignores_stale_input() {
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.events.push(egui::Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
        input.viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(true);
        let mut seen = None;
        ctx.run_ui(input, |ui| seen = Some(Activity::of(ui.ctx(), false))).textures_delta.clear();
        assert_eq!(seen, Some(Activity { busy: false, background: true }));
    }

    #[test]
    fn reports_sum_their_parts() {
        let r = MemoryReport::new(Some(100), None).part("a", 3).parts([("b", 4)]);
        assert_eq!(r.accounted, 7);
        let j = r.to_json();
        assert_eq!(j["parts"]["b"], 4);
        assert_eq!(j["footprint"], 100);
    }

    #[test]
    fn textures_are_counted() {
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |_| {}).textures_delta.clear();
        let before = texture_memory(&ctx);
        let _h = ctx.load_texture("t", egui::ColorImage::new([16, 16], vec![egui::Color32::RED; 256]), Default::default());
        let after = texture_memory(&ctx);
        assert_eq!(after.0, before.0 + 1);
        assert_eq!(after.1, before.1 + 16 * 16 * 4);
    }

    #[test]
    fn the_frame_rate_falls_to_zero_when_frames_stop() {
        let mut f = FrameRate::default();
        for i in 0..=60 {
            f.frame(i as f64 / 60.0);
        }
        assert!((f.fps(1.0) - 60.0).abs() < 0.5, "{}", f.fps(1.0));
        assert_eq!(f.fps(5.0), 0.0);
        assert_eq!(f.since_last(5.0), Some(4.0));
    }
}
