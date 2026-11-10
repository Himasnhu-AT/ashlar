//! Frame-time statistics: how long producing each frame took (logic + UI, not the time between
//! frames, which is unbounded for an app that repaints on demand).
//!
//! The headline numbers (`p50_ms`, `p95_ms`, `max_ms`) time [`crate::Runtime::ui`], the part of
//! the frame the app and the runtime control. The rest of the frame is kept per phase: `pass` is
//! the whole egui pass (input, logic, UI and egui's end of pass), `tessellate` turns the pass's
//! shapes into meshes, and `frame_cpu` is the whole frame on the CPU (the headless driver: pass +
//! tessellation; a native window: what eframe reports, including painting).

use std::collections::VecDeque;
use std::time::Duration;

use serde::Serialize;

/// Frames kept for the percentiles.
pub const WINDOW: usize = 1024;

/// The recent durations of one phase.
#[derive(Debug, Default)]
struct Series {
    recent: VecDeque<Duration>,
}

impl Series {
    fn push(&mut self, d: Duration) {
        if self.recent.len() >= WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back(d);
    }

    fn summary(&self) -> PhaseSummary {
        let mut v: Vec<Duration> = self.recent.iter().copied().collect();
        v.sort_unstable();
        let pick = |q: f64| -> f64 {
            if v.is_empty() {
                return 0.0;
            }
            let i = ((v.len() as f64 - 1.0) * q).round() as usize;
            v.get(i.min(v.len() - 1)).copied().map_or(0.0, ms)
        };
        PhaseSummary { window: v.len(), p50_ms: pick(0.5), p95_ms: pick(0.95), max_ms: v.last().copied().map_or(0.0, ms) }
    }
}

/// Recent frame times.
#[derive(Debug, Default)]
pub struct FrameStats {
    ui: Series,
    pass: Series,
    tessellate: Series,
    frame_cpu: Series,
    frames: u64,
    first_frame: Option<Duration>,
    launch: LaunchTimes,
    max_ever: Duration,
}

/// How long a native launch took, from the first line of `run()`. Times in milliseconds; `None`
/// headless and on the web.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct LaunchTimes {
    /// Until eframe had built the window and its wgpu surface.
    pub to_window_ms: Option<f64>,
    /// Until the end of the first [`crate::Runtime::ui`].
    pub to_first_frame_ms: Option<f64>,
    /// Until the first frame had been painted and presented (the start of the next frame).
    pub launch_to_present_ms: Option<f64>,
}

/// Percentiles of one phase over the window. Times in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PhaseSummary {
    /// Frames in the percentile window that recorded this phase.
    pub window: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

/// A summary for telemetry and `app.stats`. Times in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StatsSummary {
    /// Frames produced since start.
    pub frames: u64,
    /// Frames in the percentile window.
    pub window: usize,
    /// [`crate::Runtime::ui`] only.
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// Slowest frame since start.
    pub max_ever_ms: f64,
    /// Time from runtime creation to the end of the first frame.
    pub first_frame_ms: Option<f64>,
    /// Process start to window, first frame and first present (native windows only).
    pub launch: LaunchTimes,
    /// The whole egui pass (input, logic, UI, end of pass); recorded by the headless driver.
    pub pass: PhaseSummary,
    /// Shapes to meshes; recorded by the headless driver.
    pub tessellate: PhaseSummary,
    /// The whole frame on the CPU: pass + tessellation headless, eframe's own figure (painting
    /// included) in a native window.
    pub frame_cpu: PhaseSummary,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

impl FrameStats {
    /// Record the time [`crate::Runtime::ui`] took.
    pub fn record(&mut self, d: Duration) {
        self.ui.push(d);
        self.frames = self.frames.saturating_add(1);
        self.max_ever = self.max_ever.max(d);
    }

    /// Record a whole egui pass.
    pub fn record_pass(&mut self, d: Duration) {
        self.pass.push(d);
    }

    /// Record a tessellation.
    pub fn record_tessellate(&mut self, d: Duration) {
        self.tessellate.push(d);
    }

    /// Record a whole frame's CPU time.
    pub fn record_frame_cpu(&mut self, d: Duration) {
        self.frame_cpu.push(d);
    }

    pub fn set_first_frame(&mut self, d: Duration) {
        self.first_frame.get_or_insert(d);
    }

    pub(crate) fn set_to_window(&mut self, d: Duration) {
        self.launch.to_window_ms.get_or_insert(ms(d));
    }

    pub(crate) fn set_to_first_frame(&mut self, d: Duration) {
        self.launch.to_first_frame_ms.get_or_insert(ms(d));
    }

    pub(crate) fn set_presented(&mut self, d: Duration) {
        self.launch.launch_to_present_ms.get_or_insert(ms(d));
    }

    /// Process start to window, first frame and first present.
    pub fn launch(&self) -> LaunchTimes {
        self.launch
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Forget the percentile windows (start a measurement).
    pub fn reset_window(&mut self) {
        self.ui.recent.clear();
        self.pass.recent.clear();
        self.tessellate.recent.clear();
        self.frame_cpu.recent.clear();
    }

    pub fn summary(&self) -> StatsSummary {
        let ui = self.ui.summary();
        StatsSummary {
            frames: self.frames,
            window: ui.window,
            p50_ms: ui.p50_ms,
            p95_ms: ui.p95_ms,
            max_ms: ui.max_ms,
            max_ever_ms: ms(self.max_ever),
            first_frame_ms: self.first_frame.map(ms),
            launch: self.launch,
            pass: self.pass.summary(),
            tessellate: self.tessellate.summary(),
            frame_cpu: self.frame_cpu.summary(),
        }
    }
}

/// This process, for `app.stats` → `process`. A field is `null` where the platform cannot say:
/// everything is `null` on the web.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct ProcessStats {
    /// What the system charges the process for: macOS physical footprint, Linux private resident
    /// + swap, Windows private commit (`ashlar_native::memory`).
    pub footprint_bytes: Option<u64>,
    /// The highest footprint since the process started: the OS's own lifetime peak where it keeps
    /// one (macOS, Windows), so a spike between samples counts; on Linux the highest footprint
    /// sampled (at most once a second while frames run, and at every reading).
    pub peak_bytes: Option<u64>,
    /// CPU time used by every thread since the process started, in seconds.
    pub cpu_s: Option<f64>,
    /// Threads in the process.
    pub threads: Option<u64>,
}

/// Reads [`ProcessStats`] and keeps the peak footprint.
#[derive(Debug, Default)]
pub struct ProcessProbe {
    peak: Option<u64>,
    last_tick: Option<std::time::Instant>,
}

impl ProcessProbe {
    /// Called every frame: sample the footprint for the peak, at most once a second.
    pub fn tick(&mut self) {
        let Some(now) = crate::session::now() else { return };
        if self.last_tick.is_some_and(|t| now.duration_since(t) < Duration::from_secs(1)) {
            return;
        }
        self.last_tick = Some(now);
        self.footprint();
    }

    fn footprint(&mut self) -> Option<u64> {
        let m = ashlar_native::memory::process_memory()?;
        let seen = m.peak.map_or(m.footprint, |p| p.max(m.footprint));
        self.peak = Some(self.peak.map_or(seen, |p| p.max(seen)));
        Some(m.footprint)
    }

    /// Read the process now.
    pub fn read(&mut self) -> ProcessStats {
        let footprint_bytes = self.footprint();
        ProcessStats { footprint_bytes, peak_bytes: self.peak, cpu_s: process_cpu_s(), threads: threads() }
    }
}

fn process_cpu_s() -> Option<f64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        cpu_time::ProcessTime::try_now().ok().map(|t| t.as_duration().as_secs_f64())
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

fn threads() -> Option<u64> {
    ashlar_native::memory::thread_count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_process_is_read_and_its_peak_kept() {
        let mut p = ProcessProbe::default();
        p.tick();
        let s = p.read();
        if cfg!(any(target_os = "macos", target_os = "linux", target_os = "windows")) {
            let f = s.footprint_bytes.unwrap();
            assert!(f > 0 && s.peak_bytes.unwrap() >= f, "{s:?}");
            assert!(s.cpu_s.unwrap() > 0.0, "{s:?}");
        }
        if cfg!(any(target_os = "macos", target_os = "linux", target_os = "windows")) {
            assert!(s.threads.unwrap() >= 1, "{s:?}");
        }
        let v = serde_json::to_value(s).unwrap();
        for k in ["footprint_bytes", "peak_bytes", "cpu_s", "threads"] {
            assert!(v.get(k).is_some(), "{k} in {v}");
        }
    }

    #[test]
    fn app_stats_reports_the_process() {
        let mut d = crate::Driver::new(crate::App::<i64>::new("statstest", "0.0.0").runtime());
        d.frame();
        let v = d.call("app.stats", serde_json::json!({})).unwrap();
        let p = &v["process"];
        for k in ["footprint_bytes", "peak_bytes", "cpu_s", "threads"] {
            assert!(p.get(k).is_some(), "{k} in {p}");
        }
        if cfg!(any(target_os = "macos", target_os = "linux", target_os = "windows")) {
            assert!(p["footprint_bytes"].as_u64().is_some_and(|f| f > 0), "{p}");
            assert!(p["peak_bytes"].as_u64() >= p["footprint_bytes"].as_u64(), "{p}");
            assert!(p["threads"].as_u64().is_some_and(|t| t >= 1), "{p}");
        }
    }

    #[test]
    fn percentiles() {
        let mut s = FrameStats::default();
        for i in 1..=100u64 {
            s.record(Duration::from_millis(i));
        }
        let sum = s.summary();
        assert_eq!(sum.frames, 100);
        assert!((sum.p50_ms - 50.0).abs() <= 1.0);
        assert!((sum.p95_ms - 95.0).abs() <= 1.0);
        assert!((sum.max_ms - 100.0).abs() < 1e-9);
        assert_eq!(FrameStats::default().summary().p95_ms, 0.0);
    }

    #[test]
    fn phases_are_kept_apart() {
        let mut s = FrameStats::default();
        s.record(Duration::from_millis(1));
        s.record_pass(Duration::from_millis(2));
        s.record_tessellate(Duration::from_millis(3));
        s.record_frame_cpu(Duration::from_millis(5));
        let sum = s.summary();
        assert!((sum.p95_ms - 1.0).abs() < 1e-9);
        assert!((sum.pass.p95_ms - 2.0).abs() < 1e-9);
        assert!((sum.tessellate.max_ms - 3.0).abs() < 1e-9);
        assert!((sum.frame_cpu.p50_ms - 5.0).abs() < 1e-9);
        assert_eq!((sum.window, sum.pass.window, sum.tessellate.window, sum.frame_cpu.window), (1, 1, 1, 1));
        s.reset_window();
        let sum = s.summary();
        assert_eq!((sum.window, sum.pass.window, sum.tessellate.window, sum.frame_cpu.window), (0, 0, 0, 0));
        assert_eq!(sum.frames, 1);
    }

    #[test]
    fn the_window_is_bounded() {
        let mut s = FrameStats::default();
        for _ in 0..(WINDOW * 3) {
            s.record(Duration::from_micros(10));
        }
        assert_eq!(s.summary().window, WINDOW);
        assert_eq!(s.frames() as usize, WINDOW * 3);
    }
}
