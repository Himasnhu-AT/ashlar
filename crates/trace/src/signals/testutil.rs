//! Helpers shared by the unit tests in this module.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tracer_proto::Signal;

use super::Signals;
use crate::config::Config;
use crate::sink::testing::MemorySink;

/// A directory under the system temp dir, removed on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!("tracer-sdk-signals-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::create_dir_all(&path);
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Signals wired to a [`MemorySink`] in a temp directory, with millisecond-scale settings and the system
/// sampler off unless the test asks for it.
pub(crate) struct Fixture {
    pub signals: Arc<Signals>,
    pub sink: Arc<MemorySink>,
    pub dir: TempDir,
}

impl Fixture {
    /// Does not install the panic hook, so unrelated panics elsewhere in the process (other tests panic on
    /// purpose) cannot show up as crashes in this fixture's sink.
    pub(crate) fn new(edit: impl FnOnce(&mut Config)) -> Self {
        Self::build(edit, false)
    }

    /// Installs the process-wide panic hook when the config enables `Crash`. The caller must hold [`serial`].
    pub(crate) fn new_locked(edit: impl FnOnce(&mut Config)) -> Self {
        Self::build(edit, true)
    }

    fn build(edit: impl FnOnce(&mut Config), hook: bool) -> Self {
        let dir = TempDir::new();
        let sink = Arc::new(MemorySink::new(dir.path()));
        let mut config = Config::new("key", "demo", "1.2.3");
        config.signals = Signal::ALL.iter().copied().filter(|signal| *signal != Signal::Sys && *signal != Signal::Hang).collect();
        config.perf_window = Duration::from_secs(60);
        config.sys_interval = Duration::from_millis(1);
        config.hang_threshold = Duration::from_millis(100);
        edit(&mut config);
        super::crash::SKIP_HOOK.with(|skip| skip.set(!hook));
        let signals = Signals::start(&config, sink.clone());
        super::crash::SKIP_HOOK.with(|skip| skip.set(false));
        Self { signals, sink, dir }
    }

    pub(crate) fn deny(&self, signal: Signal) {
        if let Ok(mut denied) = self.sink.denied.lock() {
            denied.push(signal);
        }
    }

    /// A moment `by` after the signals were started, for driving `tick_at` without sleeping.
    pub(crate) fn at(&self, by: Duration) -> Instant {
        self.signals.shared.epoch + by
    }
}

/// Polls `condition` for up to five seconds.
pub(crate) fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    condition()
}

/// Tests that install the process-wide panic hook or global logger take this lock, so they do not
/// interfere with each other.
pub(crate) fn serial() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}
