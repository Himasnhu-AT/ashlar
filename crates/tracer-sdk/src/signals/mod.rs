//! Crash, hang, performance, system and log capture. Talks to the rest of the SDK only through [`Sink`].

use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracer_proto::{Breadcrumb, Crash, CrashKind, Perf, Signal};

use crate::config::{Config, LoggerMode};
use crate::sink::Sink;

mod crash;
mod hang;
mod logs;
mod perf;
mod report;
pub mod rt;
mod scrub;
mod sys;

#[cfg(test)]
mod testutil;

/// Static details about this machine for the envelope header; see `sys::device_info`. The graphics
/// adapter comes from the host, which is the only one that knows it.
pub(crate) fn device_info(config: &Config) -> tracer_proto::Device {
    tracer_proto::Device { gpu: config.gpu.clone(), ..sys::device_info() }
}

/// The settings the signal modules read, copied out of [`Config`] once.
pub(crate) struct Settings {
    pub signals: Vec<Signal>,
    pub perf_window: Duration,
    pub sys_interval: Duration,
    pub hang_threshold: Duration,
    pub breadcrumb_level: log::Level,
    pub app_name: String,
    pub app_version: String,
    pub build: Option<String>,
}

/// State shared by the signal modules, the panic hook, the watchdog thread and the logger. The hook, the
/// watchdog and the logger hold it weakly, so dropping [`Signals`] always ends their use of it.
pub(crate) struct Shared {
    pub sink: Arc<dyn Sink>,
    pub settings: Settings,
    pub scrubber: scrub::Scrubber,
    /// Reference point for monotonic timestamps; also "process start" for the startup time.
    pub epoch: Instant,
    pub perf: Mutex<perf::Window>,
    pub first_frame_seen: AtomicBool,
    pub sampler: sys::Sampler,
    pub registry: rt::Registry,
    pub ring: logs::Ring,
    pub hang: hang::State,
    pub last_crash: Mutex<Option<Crash>>,
    pub closed: AtomicBool,
}

impl Shared {
    fn new(config: &Config, sink: Arc<dyn Sink>) -> Self {
        let epoch = Instant::now();
        Self {
            sink,
            settings: Settings {
                signals: config.signals.clone(),
                perf_window: config.perf_window,
                sys_interval: config.sys_interval,
                hang_threshold: config.hang_threshold,
                breadcrumb_level: config.breadcrumb_level,
                app_name: config.app_name.clone(),
                app_version: config.app_version.clone(),
                build: config.build.clone(),
            },
            scrubber: scrub::Scrubber::from_env(),
            epoch,
            perf: Mutex::new(perf::Window::new(epoch)),
            first_frame_seen: AtomicBool::new(false),
            sampler: sys::Sampler::new(),
            registry: rt::Registry::default(),
            ring: logs::Ring::new(config.breadcrumb_capacity),
            hang: hang::State::default(),
            last_crash: Mutex::new(None),
            closed: AtomicBool::new(false),
        }
    }

    /// True when the host enabled `signal` and the sink (consent, kill switches) allows it.
    pub(super) fn allowed(&self, signal: Signal) -> bool {
        self.settings.signals.contains(&signal) && self.sink.allows(signal)
    }

    /// An empty crash of `kind` with the recent log lines and last performance window attached.
    /// With `blocking` false it never waits for a lock, which the panic hook needs.
    pub(super) fn crash_base(&self, kind: CrashKind, blocking: bool) -> Crash {
        let breadcrumbs = if self.allowed(Signal::Logs) { self.ring.snapshot(blocking) } else { Vec::new() };
        let perf = lock(&self.perf, blocking).and_then(|window| window.last.clone());
        Crash { kind, breadcrumbs, perf, ..Crash::default() }
    }

    /// Remembers the latest crash for the local report.
    pub(super) fn remember_crash(&self, crash: &Crash, blocking: bool) {
        if let Some(mut slot) = lock(&self.last_crash, blocking) {
            *slot = Some(crash.clone());
        }
    }

    pub(super) fn last_perf(&self) -> Option<Perf> {
        lock(&self.perf, true).and_then(|window| window.last.clone())
    }
}

/// Locks `mutex`, treating poison as success. With `blocking` false, gives up instead of waiting.
pub(super) fn lock<T>(mutex: &Mutex<T>, blocking: bool) -> Option<MutexGuard<'_, T>> {
    if blocking {
        return Some(mutex.lock().unwrap_or_else(PoisonError::into_inner));
    }
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
}

pub(crate) struct Signals {
    shared: Arc<Shared>,
    watchdog: Mutex<Option<JoinHandle<()>>>,
}

impl Signals {
    /// Builds the state and starts what the config enables: panic hook, hang watchdog, logger.
    pub(crate) fn start(config: &Config, sink: Arc<dyn Sink>) -> Arc<Self> {
        let shared = Arc::new(Shared::new(config, sink));
        if shared.settings.signals.contains(&Signal::Crash) {
            crash::install_hook(&shared);
        }
        let watchdog = if shared.settings.signals.contains(&Signal::Hang) { hang::spawn(&shared) } else { None };
        if config.logger == LoggerMode::Install && shared.settings.signals.contains(&Signal::Logs) {
            logs::install(&shared);
        }
        Arc::new(Self { shared, watchdog: Mutex::new(watchdog) })
    }

    /// Called from the pipeline's worker thread about once a second: closes performance windows,
    /// samples CPU and memory, and saves breadcrumbs to disk.
    pub(crate) fn tick(&self) {
        self.tick_at(Instant::now());
    }

    fn tick_at(&self, now: Instant) {
        if self.shared.closed.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        self.shared.sample_sys(now);
        self.shared.close_window(now, false);
        logs::save(&self.shared);
    }

    /// Flushes the open performance window. Called before the session ends.
    pub(crate) fn shutdown(&self) {
        if self.shared.closed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        self.shared.close_window(Instant::now(), true);
        self.stop_watchdog();
        logs::remove_saved(&self.shared);
    }

    fn stop_watchdog(&self) {
        let handle = lock(&self.watchdog, true).and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            hang::stop(&self.shared, handle);
        }
    }

    pub(crate) fn frame(&self, duration: Duration) {
        self.shared.record_frame(duration);
    }

    pub(crate) fn first_frame(&self) {
        self.shared.record_first_frame();
    }

    /// One atomic store.
    pub(crate) fn heartbeat(&self) {
        self.shared.hang.beat(self.shared.epoch);
    }

    pub(crate) fn pause_hang_detection(&self) {
        self.shared.hang.pause();
    }

    pub(crate) fn resume_hang_detection(&self) {
        self.shared.hang.resume(self.shared.epoch);
    }

    pub(crate) fn report_handled(&self, message: &str, context: Option<&'static str>) {
        crash::report_handled(&self.shared, message, context);
    }

    pub(crate) fn report_child_exit(&self, program: &'static str, status: ExitStatus) {
        crash::report_child_exit(&self.shared, program, status);
    }

    pub(crate) fn counter(&self, name: &'static str) -> rt::Counter {
        self.shared.registry.counter(name)
    }

    pub(crate) fn gauge(&self, name: &'static str) -> rt::Gauge {
        self.shared.registry.gauge(name)
    }

    /// A logger that records breadcrumbs and forwards every record to `inner`.
    pub(crate) fn logger(self: &Arc<Self>, inner: Option<Box<dyn log::Log>>) -> Box<dyn log::Log> {
        Box::new(logs::SignalLogger::new(&self.shared, inner))
    }

    /// Writes a human-readable report (session, device, recent logs, last crash) for a bug report.
    pub(crate) fn write_report(&self, path: &Path) -> std::io::Result<()> {
        report::write(&self.shared, path)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        self.stop_watchdog();
    }
}

/// Breadcrumbs the previous run saved before it ended, oldest first. Removes them from disk.
/// Called by the pipeline at startup when it finds that the previous run ended uncleanly.
pub(crate) fn take_saved_breadcrumbs(dir: &Path) -> Vec<Breadcrumb> {
    logs::take_saved(dir)
}
