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
    max_ever: Duration,
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
            pass: self.pass.summary(),
            tessellate: self.tessellate.summary(),
            frame_cpu: self.frame_cpu.summary(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
