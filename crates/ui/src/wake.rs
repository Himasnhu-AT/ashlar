//! The frame scheduler: ask for frames with a named [`Reason`], so an app can say why each frame
//! ran.
//!
//! An ashlar app paints on demand: a frame runs only for input or because something asked for
//! one. Every such request goes through [`wake`] / [`wake_after`] (or a [`Waker`] on another
//! thread) instead of `Context::request_repaint`. The context's [`Scheduler`] counts requests per
//! reason and credits each frame to every reason whose wake was due when it started; a frame
//! nobody asked for (pointer and keyboard input, a resize, egui's own animations) counts as
//! [`Reason::INPUT`]. Requests made during a frame are for the frames after it. A timed wake that
//! is not due yet when another frame runs stays pending: the scheduler asks egui for it again
//! ([`Scheduler::rearm`]), so code asks once per deadline, not once per frame.
//!
//! The host calls [`Scheduler::begin_frame`] once per frame, before the frame's logic runs (in
//! eframe: `App::raw_input_hook`), and [`Scheduler::rearm`] during the frame. `ashlar-app`'s
//! runtime does both; a host of its own does them too.
//!
//! ```
//! use std::time::Duration;
//! use ashlar_ui::wake::{self, Reason};
//!
//! const THUMBNAILS: Reason = Reason("thumbnails"); // an app's own reason
//! let ctx = egui::Context::default();
//! wake::wake(&ctx, Reason::JOB);
//! wake::wake_after(&ctx, THUMBNAILS, Duration::from_millis(100));
//! let worker = wake::waker(&ctx, Reason::AUTOSAVE); // Send + Sync, for a worker thread
//! std::thread::spawn(move || worker.wake()).join().ok();
//! let stats = wake::Scheduler::of(&ctx).stats();
//! assert_eq!(stats.requests_for(Reason::JOB), 1);
//! assert_eq!(stats.requests(), 3);
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Why a frame was asked for: a short camelCase name, used as the key in [`WakeStats`]. Apps
/// define their own next to the standard ones (`const THUMBNAILS: Reason = Reason("thumbnails")`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Reason(pub &'static str);

impl Reason {
    /// Nobody asked: input, a resize, egui's own animations.
    pub const INPUT: Reason = Reason("input");
    /// Fonts, theme and app setup over the first frames.
    pub const STARTUP: Reason = Reason("startup");
    /// A control-port request or injected agent input waiting to be processed.
    pub const CONTROL: Reason = Reason("control");
    /// An OS menu item was chosen.
    pub const MENU: Reason = Reason("menu");
    /// The document changed during a frame: one more so everything shows the new state.
    pub const DOCUMENT: Reason = Reason("document");
    /// A screenshot waiting for a presented frame.
    pub const SCREENSHOT: Reason = Reason("screenshot");
    /// Background job progress or results.
    pub const JOB: Reason = Reason("job");
    /// An autosave is due, or the autosave writer finished.
    pub const AUTOSAVE: Reason = Reason("autosave");
    /// Changed settings are due to be written.
    pub const SETTINGS: Reason = Reason("settings");
    /// A short UI timer (a hold hint, a click-to-rename delay).
    pub const TIMER: Reason = Reason("timer");
    /// An animation or fade the app runs itself.
    pub const ANIMATION: Reason = Reason("animation");

    /// The standard reasons.
    pub const STANDARD: [Reason; 11] = [
        Reason::INPUT,
        Reason::STARTUP,
        Reason::CONTROL,
        Reason::MENU,
        Reason::DOCUMENT,
        Reason::SCREENSHOT,
        Reason::JOB,
        Reason::AUTOSAVE,
        Reason::SETTINGS,
        Reason::TIMER,
        Reason::ANIMATION,
    ];

    pub fn name(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Pending wake times kept per reason (one code path asks again for the same deadline; distinct
/// deadlines are few).
const MAX_DUE: usize = 8;

/// Due times are egui input times (seconds); a frame starting this much early still counts.
const SLACK_S: f64 = 1e-3;

#[derive(Debug, Default)]
struct State {
    /// The egui time each reason asked to run at (`-inf` = now).
    /// The egui times each reason asked to run at, earliest first (`-inf` = now); at most
    /// [`MAX_DUE`] per reason.
    due: BTreeMap<&'static str, Vec<f64>>,
    requests: BTreeMap<&'static str, u64>,
    frames: BTreeMap<&'static str, u64>,
    total: u64,
}

fn bump(m: &mut BTreeMap<&'static str, u64>, k: &'static str) {
    let n = m.entry(k).or_insert(0);
    *n = n.saturating_add(1);
}

/// Counters since start (cumulative: diff two readings to measure an interval). Serialized as
/// `{"frames": n, "frames_by": {reason: n}, "requests_by": {reason: n}}` with the reasons that
/// have counts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeStats {
    /// Frames run, in total.
    pub frames: u64,
    /// Frames credited to each reason (a frame can have several).
    pub frames_by: BTreeMap<String, u64>,
    /// Wake requests per reason.
    pub requests_by: BTreeMap<String, u64>,
}

impl WakeStats {
    pub fn frames_for(&self, r: Reason) -> u64 {
        self.frames_by.get(r.0).copied().unwrap_or(0)
    }

    pub fn requests_for(&self, r: Reason) -> u64 {
        self.requests_by.get(r.0).copied().unwrap_or(0)
    }

    /// All wake requests.
    pub fn requests(&self) -> u64 {
        self.requests_by.values().fold(0, |a, b| a.saturating_add(*b))
    }
}

/// One context's wake requests and frame counters, shared through the egui context
/// ([`Scheduler::of`]). Cheap to clone (a handle).
#[derive(Clone, Debug, Default)]
pub struct Scheduler {
    state: Arc<Mutex<State>>,
}

fn id() -> egui::Id {
    egui::Id::new("ashlar.wake.scheduler")
}

impl Scheduler {
    /// The scheduler of this egui context (created on first use).
    pub fn of(ctx: &egui::Context) -> Scheduler {
        ctx.data_mut(|d| d.get_temp_mut_or_default::<Scheduler>(id()).clone())
    }

    /// Make this the scheduler of `ctx` (again, after something reset the context's memory).
    pub fn install(&self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(id(), self.clone()));
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record a request for `reason` at egui time `due` (`-inf` = the next frame).
    pub fn request(&self, reason: Reason, due: f64) {
        let mut s = self.lock();
        bump(&mut s.requests, reason.0);
        let list = s.due.entry(reason.0).or_default();
        // the same deadline asked again (within the slack) is one wake
        if list.iter().any(|d| (d - due).abs() <= SLACK_S || (d.is_infinite() && due.is_infinite())) {
            return;
        }
        let at = list.partition_point(|d| *d < due);
        list.insert(at, due);
        if list.len() > MAX_DUE {
            // keep the earliest ones and the latest: every pending deadline still gets a frame
            // by then
            list.remove(MAX_DUE - 1);
        }
    }

    /// Start of a frame at egui time `now`: credit it to the reasons that were due and forget
    /// them; wakes not due yet stay pending (see [`Scheduler::rearm`]).
    pub fn begin_frame(&self, now: f64) {
        let mut s = self.lock();
        let due = std::mem::take(&mut s.due);
        let mut any = false;
        for (r, mut list) in due {
            let n = list.partition_point(|d| *d <= now + SLACK_S);
            if n > 0 {
                bump(&mut s.frames, r);
                any = true;
                list.drain(..n);
            }
            if !list.is_empty() {
                s.due.insert(r, list);
            }
        }
        if !any {
            bump(&mut s.frames, Reason::INPUT.0);
        }
        s.total = s.total.saturating_add(1);
    }

    /// Forget a pending wake for `reason` (the work it was for finished early).
    pub fn cancel(&self, reason: Reason) {
        self.lock().due.remove(reason.0);
    }

    /// Ask egui again for the timed wakes still pending (egui forgets a timed repaint when another
    /// frame runs first). Call it during each frame; it does not count as a request.
    pub fn rearm(&self, ctx: &egui::Context) {
        let next = self.lock().due.values().filter_map(|l| l.first().copied()).fold(f64::INFINITY, f64::min);
        if !next.is_finite() {
            return;
        }
        let (now, predicted_dt) = ctx.input(|i| (i.time, i.predicted_dt));
        let delay = Duration::try_from_secs_f64((next - now).max(0.0)).unwrap_or_default();
        if delay.is_zero() {
            ctx.request_repaint();
        } else {
            let early = Duration::try_from_secs_f32(predicted_dt).unwrap_or_default();
            ctx.request_repaint_after(delay.saturating_add(early).saturating_add(Duration::from_millis(1)));
        }
    }

    pub fn stats(&self) -> WakeStats {
        let s = self.lock();
        let owned = |m: &BTreeMap<&'static str, u64>| m.iter().map(|(k, v)| ((*k).to_string(), *v)).collect();
        WakeStats { frames: s.total, frames_by: owned(&s.frames), requests_by: owned(&s.requests) }
    }
}

/// Ask for a frame now. Callable from any thread (not from inside a `ctx.input`/`ctx.data`
/// closure, which holds the context's lock).
pub fn wake(ctx: &egui::Context, reason: Reason) {
    Scheduler::of(ctx).request(reason, f64::NEG_INFINITY);
    ctx.request_repaint();
}

/// Ask for a frame after `delay` (measured from the current frame's time). The frame starts no
/// earlier than that (egui's `request_repaint_after` starts one predicted frame early).
pub fn wake_after(ctx: &egui::Context, reason: Reason, delay: Duration) {
    request_after(ctx, &Scheduler::of(ctx), reason, delay, false);
}

/// Like [`wake_after`], but this becomes the reason's only pending wake: for code that owns one
/// deadline and moves it (a debounced save restarting its quiet period), so the deadlines it
/// gave up on cost no frames.
pub fn reschedule(ctx: &egui::Context, reason: Reason, delay: Duration) {
    request_after(ctx, &Scheduler::of(ctx), reason, delay, true);
}

fn request_after(ctx: &egui::Context, s: &Scheduler, reason: Reason, delay: Duration, replace: bool) {
    if replace {
        s.cancel(reason);
    }
    if delay.is_zero() {
        s.request(reason, f64::NEG_INFINITY);
        ctx.request_repaint();
        return;
    }
    let (now, predicted_dt) = ctx.input(|i| (i.time, i.predicted_dt));
    s.request(reason, now + delay.as_secs_f64());
    // egui starts a timed frame one predicted frame early (so it is presented on time); a timer
    // wants its frame to start no earlier than `delay`, or the code that asked finds itself not
    // yet due and asks again. The extra millisecond absorbs clock rounding (f32/f64 seconds).
    let early = Duration::try_from_secs_f32(predicted_dt).unwrap_or_default();
    ctx.request_repaint_after(delay.saturating_add(early).saturating_add(Duration::from_millis(1)));
}

/// A handle that wakes `ctx` for `reason`, for worker threads and hooks
/// (`jobs.set_wake(move || w.wake())`).
pub fn waker(ctx: &egui::Context, reason: Reason) -> Waker {
    Waker { ctx: ctx.clone(), scheduler: Scheduler::of(ctx), reason }
}

/// Wakes one context for one reason. `Send + Sync + Clone`.
#[derive(Clone, Debug)]
pub struct Waker {
    ctx: egui::Context,
    scheduler: Scheduler,
    reason: Reason,
}

impl Waker {
    /// Ask for a frame now.
    pub fn wake(&self) {
        self.scheduler.request(self.reason, f64::NEG_INFINITY);
        self.ctx.request_repaint();
    }

    /// Make a frame after `delay` this reason's only pending wake (see [`reschedule`]).
    pub fn reschedule(&self, delay: Duration) {
        request_after(&self.ctx, &self.scheduler, self.reason, delay, true);
    }

    /// Ask for a frame after `delay`.
    pub fn wake_after(&self, delay: Duration) {
        request_after(&self.ctx, &self.scheduler, self.reason, delay, false);
    }

    pub fn reason(&self) -> Reason {
        self.reason
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: Reason = Reason("status");
    const PLAYBACK: Reason = Reason("playback");

    #[test]
    fn frames_are_credited_to_the_reasons_that_were_due() {
        let s = Scheduler::default();
        s.request(PLAYBACK, f64::NEG_INFINITY);
        s.request(STATUS, 1.0);
        s.begin_frame(0.5);
        // the status wake was not due: it stays pending and is credited when it is
        s.begin_frame(1.0);
        s.request(STATUS, 2.0);
        s.request(Reason::JOB, 1.15);
        s.begin_frame(1.15);
        let st = s.stats();
        assert_eq!(st.frames, 3);
        assert_eq!(st.frames_for(PLAYBACK), 1);
        assert_eq!(st.frames_for(STATUS), 1);
        assert_eq!(st.frames_for(Reason::JOB), 1);
        assert_eq!(st.frames_for(Reason::INPUT), 0);
        assert_eq!(st.requests_for(STATUS), 2);
        assert_eq!(st.requests(), 4);
        let v = serde_json::to_value(&st).unwrap_or_default();
        assert_eq!(v["frames_by"], serde_json::json!({"status": 1, "playback": 1, "job": 1}));
        assert_eq!(v["requests_by"], serde_json::json!({"playback": 1, "status": 2, "job": 1}));
        assert_eq!(v["frames"], serde_json::json!(3));
        let back: WakeStats = serde_json::from_value(v).unwrap_or_default();
        assert_eq!(back, st);
    }

    #[test]
    fn a_frame_counts_once_per_reason_and_the_earliest_request_wins() {
        let s = Scheduler::default();
        s.request(Reason::TIMER, 5.0);
        s.request(Reason::TIMER, 1.0);
        s.request(Reason::TIMER, 3.0);
        s.begin_frame(1.0);
        let st = s.stats();
        assert_eq!((st.frames_for(Reason::TIMER), st.requests_for(Reason::TIMER), st.frames), (1, 3, 1));
    }

    #[test]
    fn deadlines_not_yet_due_stay_pending_each_one_gets_its_frame() {
        let s = Scheduler::default();
        s.request(Reason::SETTINGS, 1.0);
        s.request(Reason::SETTINGS, 1.0); // the same deadline asked again
        s.request(Reason::SETTINGS, 1.4);
        s.begin_frame(0.2); // input came first: both stay pending
        s.begin_frame(1.0);
        s.begin_frame(1.2);
        s.begin_frame(1.4);
        let st = s.stats();
        assert_eq!((st.frames_for(Reason::SETTINGS), st.frames_for(Reason::INPUT), st.requests_for(Reason::SETTINGS)), (2, 2, 3));
        s.request(Reason::TIMER, 9.0);
        s.cancel(Reason::TIMER);
        s.begin_frame(9.0);
        assert_eq!(s.stats().frames_for(Reason::TIMER), 0);
        for i in 0..100 {
            s.request(Reason::JOB, 20.0 + f64::from(i));
        }
        s.begin_frame(200.0);
        assert_eq!(s.stats().frames_for(Reason::JOB), 1, "a capped list still ends with the latest deadline");
    }

    #[test]
    fn standard_names_are_unique() {
        let mut names: Vec<_> = Reason::STANDARD.iter().map(|r| r.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Reason::STANDARD.len());
    }

    #[test]
    fn wakes_reach_the_context_and_its_scheduler() {
        let ctx = egui::Context::default();
        let w = waker(&ctx, Reason::JOB);
        let t = std::thread::spawn(move || w.wake());
        assert!(t.join().is_ok());
        wake_after(&ctx, Reason::TIMER, Duration::from_millis(10));
        let st = Scheduler::of(&ctx).stats();
        assert_eq!((st.requests_for(Reason::JOB), st.requests_for(Reason::TIMER)), (1, 1));
        // a reset of egui's memory loses the scheduler unless it is installed again
        let s = Scheduler::of(&ctx);
        ctx.memory_mut(|m| *m = egui::Memory::default());
        s.install(&ctx);
        assert_eq!(Scheduler::of(&ctx).stats().requests(), 2);
    }
}
