//! Performance windows: frame-time histogram, startup time, process CPU and memory, and the host's
//! counters and gauges, closed every `perf_window` and submitted as one `perf` item.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tracer_proto::{FRAME_BUCKETS, FROZEN_FRAME_US, FrameStats, ItemBody, Perf, SLOW_FRAME_US, Signal};

use super::{Shared, lock, sys};

/// The frame-time histogram of the open window, as plain atomics: recording a frame is three atomic
/// operations, takes no lock, and so can never wait behind the worker or behind another thread that is
/// recording.
#[derive(Default)]
pub(crate) struct Frames {
    buckets: [AtomicU64; FRAME_BUCKETS],
    sum_us: AtomicU64,
    max_us: AtomicU32,
}

impl Frames {
    fn record(&self, us: u32) {
        if let Some(bucket) = self.buckets.get(FrameStats::bucket_index(us)) {
            bucket.fetch_add(1, Ordering::Relaxed);
        }
        self.sum_us.fetch_add(u64::from(us), Ordering::Relaxed);
        self.max_us.fetch_max(us, Ordering::Relaxed);
    }

    /// Takes what has been recorded and starts again from nothing. The count and the slow and frozen
    /// counts are worked out from the buckets, whose bounds include both thresholds, so the three always
    /// agree with the histogram. A frame recorded at this very moment lands in this window or the next.
    fn take(&self) -> FrameStats {
        let buckets: Vec<u64> = self.buckets.iter().map(|bucket| bucket.swap(0, Ordering::Relaxed)).collect();
        let above = |threshold: u32| buckets.iter().skip(FrameStats::bucket_index(threshold) + 1).sum();
        FrameStats {
            count: buckets.iter().sum(),
            sum_us: self.sum_us.swap(0, Ordering::Relaxed),
            max_us: self.max_us.swap(0, Ordering::Relaxed),
            slow: above(SLOW_FRAME_US),
            frozen: above(FROZEN_FRAME_US),
            buckets,
        }
    }
}

/// What else has accumulated since the window opened. Held behind a lock for the length of an update only.
pub(crate) struct Window {
    start: Instant,
    startup_ms: Option<u32>,
    cpu_sum: f64,
    cpu_samples: u32,
    rss_bytes: Option<u64>,
    rss_peak_bytes: u64,
    last_sample: Option<Instant>,
    /// The most recent window that was submitted, kept for crash reports.
    pub last: Option<Perf>,
}

impl Window {
    pub(super) fn new(start: Instant) -> Self {
        Self { start, startup_ms: None, cpu_sum: 0.0, cpu_samples: 0, rss_bytes: None, rss_peak_bytes: 0, last_sample: None, last: None }
    }
}

fn megabytes(bytes: u64) -> u32 {
    u32::try_from(bytes / (1024 * 1024)).unwrap_or(u32::MAX)
}

impl Shared {
    pub(super) fn record_frame(&self, duration: Duration) {
        if !self.allowed(Signal::Perf) {
            return;
        }
        self.frames.record(u32::try_from(duration.as_micros()).unwrap_or(u32::MAX));
    }

    /// Records the time since start once. Later calls do nothing.
    pub(super) fn record_first_frame(&self) {
        if !self.allowed(Signal::Perf) || self.first_frame_seen.swap(true, Ordering::AcqRel) {
            return;
        }
        let ms = u32::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u32::MAX);
        if let Some(mut window) = lock(&self.perf, true) {
            window.startup_ms = Some(ms);
        }
    }

    /// Takes a CPU and memory sample if `sys_interval` has passed since the last one.
    pub(super) fn sample_sys(&self, now: Instant) {
        if !self.allowed(Signal::Sys) {
            return;
        }
        {
            let Some(mut window) = lock(&self.perf, true) else { return };
            if window.last_sample.is_some_and(|last| now.saturating_duration_since(last) < self.settings.sys_interval) {
                return;
            }
            window.last_sample = Some(now);
        }
        if let Some(sample) = self.sampler.sample() {
            self.fold_sample(sample);
        }
    }

    pub(super) fn fold_sample(&self, sample: sys::Sample) {
        if let Some(mut window) = lock(&self.perf, true) {
            if let Some(cpu) = sample.cpu_pct {
                window.cpu_sum += f64::from(cpu);
                window.cpu_samples += 1;
            }
            window.rss_bytes = Some(sample.rss_bytes);
            window.rss_peak_bytes = window.rss_peak_bytes.max(sample.rss_bytes);
        }
    }

    /// Closes the window when `perf_window` has passed (or always, with `force`), submits it if it holds
    /// anything, and starts the next one. Counters and gauges are read even when nothing is submitted, so
    /// they never pile up across windows.
    pub(super) fn close_window(&self, now: Instant, force: bool) {
        let report = {
            let Some(mut window) = lock(&self.perf, true) else { return };
            let elapsed = now.saturating_duration_since(window.start);
            if !force && elapsed < self.settings.perf_window {
                return;
            }
            let (counters, gauges) = self.registry.drain();
            window.start = now;
            let frames = self.frames.take();
            let startup_ms = window.startup_ms.take();
            let cpu_pct = (window.cpu_samples > 0).then(|| (window.cpu_sum / f64::from(window.cpu_samples)) as f32);
            let rss_bytes = window.rss_bytes.take();
            let rss_peak_bytes = std::mem::take(&mut window.rss_peak_bytes);
            window.cpu_sum = 0.0;
            window.cpu_samples = 0;

            let perf = Perf {
                window_s: u32::try_from(elapsed.as_secs_f64().round() as u64).unwrap_or(u32::MAX).max(1),
                frames: (frames.count > 0).then_some(frames),
                cpu_pct,
                rss_mb: rss_bytes.map(megabytes),
                rss_peak_mb: rss_bytes.map(|_| megabytes(rss_peak_bytes)),
                startup_ms,
                counters,
                gauges,
            };
            let has_content = perf.frames.is_some()
                || perf.cpu_pct.is_some()
                || perf.rss_mb.is_some()
                || perf.startup_ms.is_some()
                || !perf.counters.is_empty()
                || !perf.gauges.is_empty();
            if has_content && self.allowed(Signal::Perf) {
                window.last = Some(perf.clone());
                Some(perf)
            } else {
                None
            }
        };
        if let Some(perf) = report {
            self.sink.submit(Signal::Perf, ItemBody::Perf(perf));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tracer_proto::{CrashKind, FRAME_BUCKETS, ItemBody, Perf, Signal};

    use super::*;
    use crate::signals::testutil::Fixture;

    fn perfs(fixture: &Fixture) -> Vec<Perf> {
        fixture
            .sink
            .bodies()
            .into_iter()
            .filter_map(|body| match body {
                ItemBody::Perf(perf) => Some(perf),
                _ => None,
            })
            .collect()
    }

    const WINDOW: Duration = Duration::from_secs(60);

    #[test]
    fn window_holds_a_frame_histogram() {
        let fixture = Fixture::new(|_| {});
        for ms in [4, 4, 20, 800] {
            fixture.signals.frame(Duration::from_millis(ms));
        }
        fixture.signals.tick_at(fixture.at(WINDOW));
        let items = perfs(&fixture);
        assert_eq!(items.len(), 1);
        let perf = &items[0];
        assert_eq!(perf.window_s, 60);
        let frames = perf.frames.as_ref().expect("frames");
        assert_eq!(frames.count, 4);
        assert_eq!(frames.sum_us, 4_000 + 4_000 + 20_000 + 800_000);
        assert_eq!(frames.max_us, 800_000);
        assert_eq!(frames.slow, 2);
        assert_eq!(frames.frozen, 1);
        assert_eq!(frames.buckets.len(), FRAME_BUCKETS);
        assert_eq!(frames.buckets.iter().sum::<u64>(), 4);
        let sent = fixture.sink.items.lock().expect("lock");
        assert_eq!(sent[0].0, Signal::Perf);
        assert!(!sent[0].2, "perf must not use submit_now");
    }

    #[test]
    fn nothing_is_submitted_for_an_empty_window() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.tick_at(fixture.at(WINDOW));
        fixture.signals.tick_at(fixture.at(WINDOW * 2));
        assert!(fixture.sink.bodies().is_empty());
    }

    #[test]
    fn window_is_not_closed_early_and_data_carries_over() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.frame(Duration::from_millis(5));
        let counter = fixture.signals.counter("work.done");
        counter.add(3);
        fixture.signals.tick_at(fixture.at(WINDOW - Duration::from_secs(1)));
        assert!(perfs(&fixture).is_empty());
        counter.add(2);
        fixture.signals.tick_at(fixture.at(WINDOW));
        let items = perfs(&fixture);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].frames.as_ref().map(|f| f.count), Some(1));
        assert_eq!(items[0].counters.get("work.done"), Some(&5), "increases from the early tick must not be lost");
    }

    #[test]
    fn startup_is_recorded_once() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.first_frame();
        fixture.signals.first_frame();
        fixture.signals.tick_at(fixture.at(WINDOW));
        fixture.signals.first_frame();
        fixture.signals.frame(Duration::from_millis(3));
        fixture.signals.tick_at(fixture.at(WINDOW * 2));
        let items = perfs(&fixture);
        assert_eq!(items.len(), 2);
        assert!(items[0].startup_ms.is_some());
        assert!(items[1].startup_ms.is_none());
    }

    #[test]
    fn counters_are_deltas_and_gauges_are_last_value() {
        let fixture = Fixture::new(|_| {});
        let xruns = fixture.signals.counter("audio.xruns");
        let load = fixture.signals.gauge("audio.load");
        xruns.add(2);
        load.set(0.25);
        load.set(0.5);
        fixture.signals.tick_at(fixture.at(WINDOW));
        xruns.add(1);
        fixture.signals.tick_at(fixture.at(WINDOW * 2));
        fixture.signals.tick_at(fixture.at(WINDOW * 3));
        let items = perfs(&fixture);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].counters.get("audio.xruns"), Some(&2));
        assert_eq!(items[0].gauges.get("audio.load"), Some(&0.5));
        assert_eq!(items[1].counters.get("audio.xruns"), Some(&1));
        assert!(items[1].gauges.is_empty());
    }

    #[test]
    fn counter_handles_for_one_name_are_shared() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.counter("n").inc();
        fixture.signals.counter("n").inc();
        fixture.signals.tick_at(fixture.at(WINDOW));
        assert_eq!(perfs(&fixture)[0].counters.get("n"), Some(&2));
    }

    #[test]
    fn shutdown_flushes_the_open_window_once() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.frame(Duration::from_millis(5));
        fixture.signals.shutdown();
        assert_eq!(perfs(&fixture).len(), 1);
        fixture.signals.frame(Duration::from_millis(5));
        fixture.signals.shutdown();
        fixture.signals.tick_at(fixture.at(WINDOW * 5));
        assert_eq!(perfs(&fixture).len(), 1);
    }

    #[test]
    fn last_perf_is_attached_to_crash_reports() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.report_handled("before", None);
        fixture.signals.frame(Duration::from_millis(5));
        fixture.signals.tick_at(fixture.at(WINDOW));
        fixture.signals.report_handled("after", None);
        let crashes: Vec<_> = fixture
            .sink
            .bodies()
            .into_iter()
            .filter_map(|body| match body {
                ItemBody::Crash(crash) if crash.kind == CrashKind::Handled => Some(crash),
                _ => None,
            })
            .collect();
        assert_eq!(crashes.len(), 2);
        assert!(crashes[0].perf.is_none());
        assert_eq!(crashes[1].perf.as_ref().and_then(|p| p.frames.as_ref()).map(|f| f.count), Some(1));
    }

    #[test]
    fn denied_perf_produces_nothing() {
        let fixture = Fixture::new(|_| {});
        fixture.deny(Signal::Perf);
        fixture.signals.frame(Duration::from_millis(5));
        fixture.signals.first_frame();
        fixture.signals.counter("c").inc();
        fixture.signals.tick_at(fixture.at(WINDOW));
        fixture.signals.shutdown();
        assert!(fixture.sink.bodies().is_empty());
    }

    #[test]
    fn perf_left_out_of_the_config_produces_nothing() {
        let fixture = Fixture::new(|config| config.signals = vec![Signal::Crash]);
        fixture.signals.frame(Duration::from_millis(5));
        fixture.signals.tick_at(fixture.at(WINDOW));
        assert!(fixture.sink.bodies().is_empty());
    }

    #[test]
    fn samples_fold_into_mean_cpu_last_and_peak_memory() {
        let fixture = Fixture::new(|_| {});
        let mb = 1024 * 1024;
        let shared = &fixture.signals.shared;
        shared.fold_sample(sys::Sample { cpu_pct: Some(10.0), rss_bytes: 300 * mb });
        shared.fold_sample(sys::Sample { cpu_pct: Some(30.0), rss_bytes: 500 * mb });
        shared.fold_sample(sys::Sample { cpu_pct: None, rss_bytes: 400 * mb });
        fixture.signals.tick_at(fixture.at(WINDOW));
        let perf = &perfs(&fixture)[0];
        assert_eq!(perf.cpu_pct, Some(20.0));
        assert_eq!(perf.rss_mb, Some(400));
        assert_eq!(perf.rss_peak_mb, Some(500));
        assert!(perf.frames.is_none());
        // The next window starts clean.
        fixture.signals.tick_at(fixture.at(WINDOW * 2));
        assert_eq!(perfs(&fixture).len(), 1);
    }

    #[cfg(all(feature = "sysinfo", not(target_arch = "wasm32")))]
    #[test]
    fn real_sampling_reports_memory_when_sys_is_enabled() {
        let fixture = Fixture::new(|config| config.signals = vec![Signal::Perf, Signal::Sys]);
        fixture.signals.tick_at(fixture.at(WINDOW));
        let perf = &perfs(&fixture)[0];
        assert!(perf.rss_mb.is_some_and(|mb| mb > 0), "{perf:?}");
        assert!(perf.rss_peak_mb >= perf.rss_mb);
    }

    #[test]
    fn denied_or_disabled_sys_takes_no_samples() {
        let fixture = Fixture::new(|config| config.signals = vec![Signal::Perf, Signal::Sys]);
        fixture.deny(Signal::Sys);
        fixture.signals.tick_at(fixture.at(WINDOW));
        assert!(fixture.sink.bodies().is_empty());
        let fixture = Fixture::new(|config| config.signals = vec![Signal::Perf]);
        fixture.signals.tick_at(fixture.at(WINDOW));
        assert!(fixture.sink.bodies().is_empty());
    }

    #[test]
    fn samples_respect_the_interval() {
        let fixture = Fixture::new(|config| {
            config.signals = vec![Signal::Perf, Signal::Sys];
            config.sys_interval = Duration::from_secs(10);
        });
        let shared = &fixture.signals.shared;
        shared.sample_sys(fixture.at(Duration::from_secs(1)));
        let first = crate::signals::lock(&shared.perf, true).map(|w| w.last_sample);
        shared.sample_sys(fixture.at(Duration::from_secs(5)));
        let second = crate::signals::lock(&shared.perf, true).map(|w| w.last_sample);
        assert_eq!(first, second, "no new sample inside the interval");
        shared.sample_sys(fixture.at(Duration::from_secs(12)));
        let third = crate::signals::lock(&shared.perf, true).map(|w| w.last_sample);
        assert_ne!(second, third);
    }
}
