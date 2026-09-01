//! Frame-time statistics: how long producing each frame took (logic + UI, not the time between
//! frames, which is unbounded for an app that repaints on demand).

use std::collections::VecDeque;
use std::time::Duration;

use serde::Serialize;

/// Frames kept for the percentiles.
pub const WINDOW: usize = 1024;

/// Recent frame times.
#[derive(Debug, Default)]
pub struct FrameStats {
    recent: VecDeque<Duration>,
    frames: u64,
    first_frame: Option<Duration>,
    max_ever: Duration,
}

/// A summary for telemetry and `app.stats`. Times in milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct StatsSummary {
    /// Frames produced since start.
    pub frames: u64,
    /// Frames in the percentile window.
    pub window: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// Slowest frame since start.
    pub max_ever_ms: f64,
    /// Time from runtime creation to the end of the first frame.
    pub first_frame_ms: Option<f64>,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

impl FrameStats {
    pub fn record(&mut self, d: Duration) {
        if self.recent.len() >= WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back(d);
        self.frames = self.frames.saturating_add(1);
        self.max_ever = self.max_ever.max(d);
    }

    pub fn set_first_frame(&mut self, d: Duration) {
        self.first_frame.get_or_insert(d);
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Forget the percentile window (start a measurement).
    pub fn reset_window(&mut self) {
        self.recent.clear();
    }

    pub fn summary(&self) -> StatsSummary {
        let mut v: Vec<Duration> = self.recent.iter().copied().collect();
        v.sort_unstable();
        let pick = |q: f64| -> f64 {
            if v.is_empty() {
                return 0.0;
            }
            let i = ((v.len() as f64 - 1.0) * q).round() as usize;
            v.get(i.min(v.len() - 1)).copied().map_or(0.0, ms)
        };
        StatsSummary {
            frames: self.frames,
            window: v.len(),
            p50_ms: pick(0.5),
            p95_ms: pick(0.95),
            max_ms: v.last().copied().map_or(0.0, ms),
            max_ever_ms: ms(self.max_ever),
            first_frame_ms: self.first_frame.map(ms),
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
    fn the_window_is_bounded() {
        let mut s = FrameStats::default();
        for _ in 0..(WINDOW * 3) {
            s.record(Duration::from_micros(10));
        }
        assert_eq!(s.summary().window, WINDOW);
        assert_eq!(s.frames() as usize, WINDOW * 3);
    }
}
