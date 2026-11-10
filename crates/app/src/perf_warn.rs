//! Perf warnings for development builds, and the per-frame probe that feeds them and the frame
//! log ([`crate::frame_log`]).
//!
//! In debug builds every app watches its own frames and prints one stderr line, once per site, when:
//!
//! - **a frame stalls**: the pass took ≥ 8 ms of wall time and ≥ 4× the UI thread's CPU time, so
//!   the thread was waiting (blocking I/O, a lock, a sleep). The line names the command the frame
//!   ran, or else the wake reasons it was drawn for.
//! - **the same frame is drawn again and again**: 60 frames in a row with identical shapes, drawn
//!   for non-input wake reasons only (a timer or animation that never settles). The line names
//!   the reasons.
//!
//! `ASHLAR_FRAME_BUDGET_MS=N` (any build) adds at most one line per second for passes over `N` ms,
//! with the phase split and the wake reasons. `ASHLAR_PERF_WARN=0` silences all of them. Release
//! builds without these variables install nothing.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use egui::epaint::ClippedShape;

use crate::frame_log::FrameLog;

/// Wall time from which a waiting frame counts as a stall.
pub const STALL_WALL: Duration = Duration::from_millis(8);
/// A stall's wall time is at least this many times its CPU time.
pub const STALL_RATIO: u32 = 4;
/// Identical non-input frames in a row before the "same frame" warning.
pub const SAME_FRAMES: u32 = 60;
/// Sites remembered for "once per site" (a flood of distinct sites stops warning after this).
const MAX_SITES: usize = 1024;

/// One egui pass, as the probe measured it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameSample {
    /// The runtime's frame number (passes of one frame share it).
    pub frame: u64,
    /// Seconds since the probe started (for the budget line's rate limit).
    pub t: f64,
    /// The pass, from egui's input hook to its output hook.
    pub wall: Duration,
    /// The UI thread's CPU time over the same span (`None` where it cannot be read).
    pub cpu: Option<Duration>,
    /// The runtime's own frame (`Runtime::ui`: menus, view, dialogs), when it ran in this pass.
    pub ui: Option<Duration>,
    /// The wake reasons the frame was credited to (`app.stats` → `wakes`).
    pub reasons: Vec<String>,
    /// Top-level commands run during the frame.
    pub commands: Vec<&'static str>,
    /// The pass carried input events.
    pub input: bool,
    /// The pass's number within its frame (1, or more when egui ran another pass).
    pub pass: u32,
    /// Shapes identical to the previous non-input frame's (`None`: not compared, an input frame).
    pub same: Option<bool>,
}

impl FrameSample {
    /// Drawn only for wake reasons other than input (the frames the "same frame" meter watches).
    pub fn non_input(&self) -> bool {
        !self.input && !self.reasons.is_empty() && !self.reasons.iter().any(|r| r == "input")
    }

    /// What the frame was for: its first command, or else its wake reasons.
    fn site(&self) -> String {
        match self.commands.first() {
            Some(c) => format!("command {c}"),
            None if self.reasons.is_empty() => "wake input".into(),
            None => format!("wake {}", self.reasons.join("+")),
        }
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The warning meters. Feed every pass to [`PerfWarn::observe`].
#[derive(Debug)]
pub struct PerfWarn {
    /// The stall and same-frame meters (debug builds).
    detect: bool,
    budget: Option<Duration>,
    warned: HashSet<String>,
    identical: u32,
    last_budget_line: Option<f64>,
    over_since_line: u64,
}

impl PerfWarn {
    /// Meters: `detect` turns on the stall and same-frame warnings, `budget` the budget line.
    pub fn new(detect: bool, budget: Option<Duration>) -> PerfWarn {
        PerfWarn { detect, budget, warned: HashSet::new(), identical: 0, last_budget_line: None, over_since_line: 0 }
    }

    /// From the environment: `None` when nothing would ever warn (`ASHLAR_PERF_WARN=0`, or a
    /// release build without `ASHLAR_FRAME_BUDGET_MS`).
    pub fn from_env() -> Option<PerfWarn> {
        let var = |k: &str| std::env::var(k).ok();
        Self::configure(cfg!(debug_assertions), var("ASHLAR_PERF_WARN").as_deref(), var("ASHLAR_FRAME_BUDGET_MS").as_deref())
    }

    fn configure(debug: bool, perf_warn: Option<&str>, budget_ms: Option<&str>) -> Option<PerfWarn> {
        if perf_warn.is_some_and(|v| v.trim() == "0") {
            return None;
        }
        let budget = budget_ms
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|n| n.is_finite() && *n > 0.0)
            .and_then(|n| Duration::try_from_secs_f64(n / 1000.0).ok());
        (debug || budget.is_some()).then(|| PerfWarn::new(debug, budget))
    }

    fn once(&mut self, site: String) -> bool {
        self.warned.len() < MAX_SITES && self.warned.insert(site)
    }

    /// Look at one pass; push any warning lines to `out`.
    pub fn observe(&mut self, s: &FrameSample, out: &mut Vec<String>) {
        if self.detect {
            self.stall(s, out);
            self.same(s, out);
        }
        self.budget(s, out);
    }

    fn stall(&mut self, s: &FrameSample, out: &mut Vec<String>) {
        let Some(cpu) = s.cpu else { return };
        if s.wall < STALL_WALL || s.wall < cpu.saturating_mul(STALL_RATIO) {
            return;
        }
        let site = s.site();
        if self.once(format!("stall:{site}")) {
            out.push(format!(
                "ashlar perf: a frame waited {:.1} ms with {:.1} ms of CPU during {site}: blocking I/O, a lock or a sleep on the UI thread? Run it as a job. (once per site; ASHLAR_PERF_WARN=0 silences)",
                ms(s.wall),
                ms(cpu)
            ));
        }
    }

    fn same(&mut self, s: &FrameSample, out: &mut Vec<String>) {
        if s.same == Some(true) && s.non_input() {
            self.identical = self.identical.saturating_add(1);
        } else {
            self.identical = 0;
            return;
        }
        if self.identical != SAME_FRAMES {
            return;
        }
        let site = s.site();
        if self.once(format!("same:{site}")) {
            out.push(format!(
                "ashlar perf: {SAME_FRAMES} identical frames in a row for {site}: something asks for frames that change nothing (a timer or an animation that never settles). (once per site; ASHLAR_PERF_WARN=0 silences)"
            ));
        }
    }

    fn budget(&mut self, s: &FrameSample, out: &mut Vec<String>) {
        let Some(budget) = self.budget else { return };
        if s.wall <= budget {
            return;
        }
        self.over_since_line = self.over_since_line.saturating_add(1);
        if self.last_budget_line.is_some_and(|last| s.t - last < 1.0) {
            return;
        }
        let mut line = format!("ashlar perf: frame {:.1} ms over the {:.1} ms budget (pass {:.1} ms", ms(s.wall), ms(budget), ms(s.wall));
        if let Some(ui) = s.ui {
            let _ = write!(line, ", ui {:.1} ms", ms(ui));
        }
        if let Some(cpu) = s.cpu {
            let _ = write!(line, ", cpu {:.1} ms", ms(cpu));
        }
        let _ = write!(line, "; {}", s.site());
        if !s.commands.is_empty() {
            let _ = write!(line, ", wake {}", if s.reasons.is_empty() { "input".into() } else { s.reasons.join("+") });
        }
        let earlier = self.over_since_line.saturating_sub(1);
        if earlier > 0 {
            let _ = write!(line, "; {earlier} more over budget since the last line");
        }
        line.push(')');
        out.push(line);
        self.last_budget_line = Some(s.t);
        self.over_since_line = 0;
    }
}

/// The wake reasons credited between two cumulative readings of `app.stats` → `wakes`.
pub(crate) fn credited(before: &ashlar_ui::wake::WakeStats, after: &ashlar_ui::wake::WakeStats) -> Vec<String> {
    after.frames_by.iter().filter(|(k, n)| before.frames_by.get(*k).copied().unwrap_or(0) < **n).map(|(k, _)| k.clone()).collect()
}

// ------------------------------------------------------------------ the probe

#[derive(Default)]
struct State {
    warn: Option<PerfWarn>,
    log: Option<FrameLog>,
    frame: u64,
    epoch: Option<std::time::Instant>,
    /// The runtime started a new frame since the last pass.
    new_frame: bool,
    pass: u32,
    reasons: Vec<String>,
    commands: Vec<&'static str>,
    ui: Option<Duration>,
    started: Option<(std::time::Instant, Option<Duration>)>,
    input: bool,
    prev_shapes: Vec<ClippedShape>,
    have_prev: bool,
    lines: Vec<String>,
    /// Warning lines printed so far.
    emitted: u64,
}

/// Shared between the runtime (frame reasons, commands, its own time) and the egui plugin that
/// times each pass and hands it to the warnings and the frame log.
#[derive(Clone, Default)]
pub(crate) struct Probe {
    state: Arc<Mutex<State>>,
}

fn thread_cpu() -> Option<Duration> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        cpu_time::ThreadTime::try_now().ok().map(|t| t.as_duration())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

impl Probe {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A probe with the environment's warnings and `ASHLAR_FRAME_LOG`; `None` when nothing
    /// would use it.
    pub(crate) fn from_env() -> Option<Probe> {
        let p = PerfWarn::from_env().map(Probe::with_warn);
        let Some(path) = std::env::var_os("ASHLAR_FRAME_LOG").filter(|v| !v.is_empty()) else { return p };
        match FrameLog::create(std::path::Path::new(&path), crate::frame_log::MAX_BYTES) {
            Ok(log) => {
                let p = p.unwrap_or_default();
                p.set_log(log);
                Some(p)
            }
            Err(e) => {
                log::warn!("ASHLAR_FRAME_LOG: cannot write {}: {e}", path.to_string_lossy());
                p
            }
        }
    }

    pub(crate) fn set_log(&self, log: FrameLog) {
        self.lock().log = Some(log);
    }

    pub(crate) fn with_warn(warn: PerfWarn) -> Probe {
        let p = Probe::default();
        p.lock().warn = Some(warn);
        p
    }

    /// Warning lines printed so far.
    #[cfg(test)]
    pub(crate) fn emitted(&self) -> u64 {
        self.lock().emitted
    }

    /// The runtime credited a new frame to `reasons` (before egui's pass begins).
    pub(crate) fn begin_frame(&self, reasons: Vec<String>) {
        let mut s = self.lock();
        s.new_frame = true;
        s.frame = s.frame.saturating_add(1);
        s.reasons = reasons;
    }

    /// The runtime's own frame ended after `ui`, having run `commands`.
    pub(crate) fn end_ui(&self, ui: Option<Duration>, commands: Vec<&'static str>) {
        let mut s = self.lock();
        s.ui = ui;
        let room = 64usize.saturating_sub(s.commands.len());
        s.commands.extend(commands.into_iter().take(room));
    }

    /// The egui plugin to install on the app's context.
    pub(crate) fn plugin(&self) -> Plugin {
        Plugin { probe: self.clone() }
    }

    fn input(&self, raw: &egui::RawInput) {
        let mut s = self.lock();
        let Some(now) = crate::session::now() else { return };
        if s.epoch.is_none() {
            s.epoch = Some(now);
        }
        if std::mem::take(&mut s.new_frame) {
            s.pass = 0;
        } else if s.pass > 0 {
            // another pass of the same frame: it has no new reasons or commands
            s.commands.clear();
        }
        s.pass = s.pass.saturating_add(1);
        s.input = !raw.events.is_empty();
        s.started = Some((now, thread_cpu()));
    }

    fn output(&self, out: &egui::FullOutput) {
        let mut guard = self.lock();
        let s = &mut *guard;
        let Some((start, cpu0)) = s.started.take() else { return };
        let Some(now) = crate::session::now() else { return };
        let cpu = match (cpu0, thread_cpu()) {
            (Some(a), Some(b)) => Some(b.saturating_sub(a)),
            _ => None,
        };
        let mut sample = FrameSample {
            frame: s.frame,
            t: s.epoch.map_or(0.0, |e| start.duration_since(e).as_secs_f64()),
            wall: now.duration_since(start),
            cpu,
            ui: s.ui.take(),
            reasons: s.reasons.clone(),
            commands: std::mem::take(&mut s.commands),
            input: s.input,
            pass: s.pass,
            same: None,
        };
        // only the warnings compare shapes; non-input frames keep a copy of the last shapes
        if s.warn.is_some() && sample.non_input() {
            let same = s.have_prev && s.prev_shapes == out.shapes;
            if !same {
                s.prev_shapes.clone_from(&out.shapes);
            }
            s.have_prev = true;
            sample.same = Some(same);
        } else if s.have_prev {
            s.prev_shapes = Vec::new();
            s.have_prev = false;
        }
        if let Some(w) = &mut s.warn {
            w.observe(&sample, &mut s.lines);
            for line in s.lines.drain(..) {
                s.emitted = s.emitted.saturating_add(1);
                eprintln!("{line}");
            }
        }
        if let Some(log) = &mut s.log {
            log.frame(&sample);
        }
    }
}

/// Times each egui pass for the [`Probe`].
pub(crate) struct Plugin {
    probe: Probe,
}

impl egui::Plugin for Plugin {
    fn debug_name(&self) -> &'static str {
        "ashlar_app::perf_warn"
    }

    fn input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.probe.input(raw);
    }

    fn output_hook(&mut self, _ctx: &egui::Context, out: &mut egui::FullOutput) {
        self.probe.output(out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: f64, wall_ms: u64, cpu_ms: u64, reasons: &[&str]) -> FrameSample {
        FrameSample {
            t,
            wall: Duration::from_millis(wall_ms),
            cpu: Some(Duration::from_millis(cpu_ms)),
            reasons: reasons.iter().map(|r| r.to_string()).collect(),
            pass: 1,
            ..FrameSample::default()
        }
    }

    fn feed(w: &mut PerfWarn, frames: impl IntoIterator<Item = FrameSample>) -> Vec<String> {
        let mut out = Vec::new();
        for f in frames {
            w.observe(&f, &mut out);
        }
        out
    }

    #[test]
    fn a_waiting_frame_warns_once_per_site() {
        let mut w = PerfWarn::new(true, None);
        let mut slow = frame(0.0, 30, 2, &["job"]);
        let out = feed(&mut w, [slow.clone(), slow.clone()]);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("wake job") && out[0].contains("30.0 ms"), "{}", out[0]);
        slow.commands = vec!["file.open"];
        let out = feed(&mut w, [slow.clone(), slow]);
        assert_eq!(out.len(), 1, "a new site warns once: {out:?}");
        assert!(out[0].contains("command file.open"), "{}", out[0]);
    }

    #[test]
    fn busy_or_short_frames_are_not_stalls() {
        let mut w = PerfWarn::new(true, None);
        let busy = frame(0.0, 30, 25, &["animation"]);
        let short = frame(0.0, 7, 0, &["animation"]);
        let unknown_cpu = FrameSample { cpu: None, ..frame(0.0, 30, 0, &["timer"]) };
        assert!(feed(&mut w, [busy, short, unknown_cpu]).is_empty());
    }

    #[test]
    fn identical_non_input_frames_warn_after_sixty() {
        let mut w = PerfWarn::new(true, None);
        let same = FrameSample { same: Some(true), ..frame(0.0, 1, 1, &["animation"]) };
        let out = feed(&mut w, std::iter::repeat_n(same.clone(), SAME_FRAMES as usize - 1));
        assert!(out.is_empty(), "{out:?}");
        let out = feed(&mut w, std::iter::repeat_n(same.clone(), 200));
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("wake animation"), "{}", out[0]);
        // an input frame or a changed frame breaks the run; input-credited frames never count
        let mut w = PerfWarn::new(true, None);
        let changed = FrameSample { same: Some(false), ..same.clone() };
        let input = FrameSample { same: Some(true), ..frame(0.0, 1, 1, &["input"]) };
        let mut frames: Vec<_> = std::iter::repeat_n(same.clone(), 59).collect();
        frames.push(changed);
        frames.extend(std::iter::repeat_n(same.clone(), 59));
        frames.extend(std::iter::repeat_n(input, 200));
        assert!(feed(&mut w, frames).is_empty());
    }

    #[test]
    fn the_budget_line_is_rate_limited() {
        let mut w = PerfWarn::new(false, Some(Duration::from_millis(10)));
        let frames = (0..30).map(|i| frame(f64::from(i) * 0.1, 12, 11, &["animation"]));
        let out = feed(&mut w, frames);
        assert_eq!(out.len(), 3, "one line per second over 3 s: {out:?}");
        assert!(out[0].contains("12.0 ms over the 10.0 ms budget") && out[0].contains("cpu 11.0 ms"), "{}", out[0]);
        assert!(out[1].contains("9 more over budget"), "{}", out[1]);
        assert!(feed(&mut w, [frame(10.0, 9, 9, &["timer"])]).is_empty(), "under budget");
        // detection off: a stall says nothing
        assert!(feed(&mut w, [frame(20.0, 9, 0, &["job"])]).is_empty());
    }

    #[test]
    fn the_environment_turns_meters_on_and_off() {
        assert!(PerfWarn::configure(true, Some("0"), Some("16")).is_none(), "0 silences everything");
        assert!(PerfWarn::configure(false, None, None).is_none(), "release: nothing to do");
        assert!(PerfWarn::configure(false, None, Some("nan")).is_none());
        let w = PerfWarn::configure(false, None, Some(" 16 ")).unwrap();
        assert_eq!((w.detect, w.budget), (false, Some(Duration::from_millis(16))));
        let w = PerfWarn::configure(true, Some("1"), None).unwrap();
        assert_eq!((w.detect, w.budget), (true, None));
    }

    /// The perf test's scroll scenario: a long list scrolled by wheel and left to settle warns
    /// about nothing; a view that blocks does.
    #[test]
    fn scrolling_a_list_emits_no_warnings_and_a_blocking_view_does() {
        use crate::{App, Driver};
        let app = App::<i64>::new("perfwarntest", "0.0.0").view(|ui, _cx| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                for i in 0..300 {
                    ui.label(format!("row {i}"));
                }
            });
        });
        let mut rt = app.runtime();
        let probe = rt.watch_frames(PerfWarn::new(true, None));
        let mut d = Driver::new(rt);
        d.frame();
        for _ in 0..60 {
            d.push_event(egui::Event::PointerMoved(egui::pos2(100.0, 100.0)));
            d.push_event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -40.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            });
            d.advance(1.0 / 60.0);
        }
        d.settle(600);
        assert!(d.frames() > 60, "frames ran: {}", d.frames());
        assert_eq!(probe.emitted(), 0, "the scroll scenario warns about nothing");

        let app = App::<i64>::new("perfwarntest", "0.0.0").view(|ui, _cx| {
            std::thread::sleep(Duration::from_millis(12));
            ui.label("slow");
        });
        let mut rt = app.runtime();
        let probe = rt.watch_frames(PerfWarn::new(true, None));
        let mut d = Driver::new(rt);
        d.frame();
        d.frame();
        d.frame();
        assert_eq!(probe.emitted(), 1, "a sleeping view stalls, warned once");
    }

    #[test]
    fn credited_reasons_are_the_counts_that_grew() {
        let mut a = ashlar_ui::wake::WakeStats::default();
        a.frames_by.insert("timer".into(), 2);
        let mut b = a.clone();
        b.frames_by.insert("animation".into(), 1);
        assert_eq!(credited(&a, &b), vec!["animation".to_string()]);
        b.frames_by.insert("timer".into(), 3);
        assert_eq!(credited(&a, &b), vec!["animation".to_string(), "timer".to_string()]);
    }
}
