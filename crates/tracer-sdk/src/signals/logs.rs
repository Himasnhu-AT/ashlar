//! Log lines kept as crash breadcrumbs: a bounded ring of scrubbed lines behind a `log::Log` that forwards
//! every record to the logger it wraps. The ring is saved to `breadcrumbs.jsonl` so a crash that kills the
//! process (or an unclean exit) still leaves the lines behind for the next run.

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use tracer_proto::{Breadcrumb, Signal};

use super::scrub::{MAX_LABEL, MAX_LOG_LINE};
use super::{Shared, lock, now_ms, report};

const FILE_NAME: &str = "breadcrumbs.jsonl";
/// A saved file larger than this is not trusted.
const MAX_SAVED_BYTES: u64 = 4 * 1024 * 1024;

thread_local! {
    static IN_LOGGER: Cell<bool> = const { Cell::new(false) };
}

/// The most recent breadcrumbs, oldest first.
pub(crate) struct Ring {
    inner: Mutex<RingInner>,
}

struct RingInner {
    items: VecDeque<Breadcrumb>,
    capacity: usize,
    /// Changed since the last save.
    dirty: bool,
}

impl Ring {
    pub(super) fn new(capacity: usize) -> Self {
        Self { inner: Mutex::new(RingInner { items: VecDeque::new(), capacity, dirty: false }) }
    }

    pub(super) fn push(&self, crumb: Breadcrumb) {
        let Some(mut inner) = lock(&self.inner, true) else { return };
        if inner.capacity == 0 {
            return;
        }
        while inner.items.len() >= inner.capacity {
            inner.items.pop_front();
        }
        inner.items.push_back(crumb);
        inner.dirty = true;
    }

    /// A copy of the ring, oldest first. With `blocking` false, empty when the lock is busy.
    pub(super) fn snapshot(&self, blocking: bool) -> Vec<Breadcrumb> {
        lock(&self.inner, blocking).map(|inner| inner.items.iter().cloned().collect()).unwrap_or_default()
    }

    /// Empties the ring. Nothing is left to save either: the caller deletes the saved file.
    pub(super) fn clear(&self) {
        if let Some(mut inner) = lock(&self.inner, true) {
            inner.items.clear();
            inner.dirty = false;
        }
    }

    /// The ring if it changed since the last call, marking it saved.
    fn take_dirty(&self) -> Option<Vec<Breadcrumb>> {
        let mut inner = lock(&self.inner, true)?;
        if !inner.dirty {
            return None;
        }
        inner.dirty = false;
        Some(inner.items.iter().cloned().collect())
    }
}

pub(super) struct SignalLogger {
    shared: Weak<Shared>,
    inner: Option<Box<dyn log::Log>>,
    level: log::Level,
}

impl SignalLogger {
    pub(super) fn new(shared: &Arc<Shared>, inner: Option<Box<dyn log::Log>>) -> Self {
        Self { shared: Arc::downgrade(shared), inner, level: shared.settings.breadcrumb_level }
    }

    fn capture(&self, record: &log::Record<'_>) {
        if record.level() > self.level {
            return;
        }
        let Some(shared) = self.shared.upgrade() else { return };
        if !shared.allowed(Signal::Logs) {
            return;
        }
        let crumb = Breadcrumb {
            ts: now_ms(),
            level: record.level().as_str().to_ascii_lowercase(),
            target: shared.scrubber.apply(record.target(), MAX_LABEL),
            message: shared.scrubber.apply(&record.args().to_string(), MAX_LOG_LINE),
        };
        shared.ring.push(crumb);
    }
}

/// Marks the thread as inside the logger until dropped, so a record logged while handling a record is ignored.
struct Reentry;

impl Reentry {
    fn enter() -> Option<Self> {
        let already = IN_LOGGER.try_with(|flag| flag.replace(true)).unwrap_or(true);
        (!already).then_some(Self)
    }
}

impl Drop for Reentry {
    fn drop(&mut self) {
        let _ = IN_LOGGER.try_with(|flag| flag.set(false));
    }
}

impl log::Log for SignalLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.level || self.inner.as_ref().is_some_and(|inner| inner.enabled(metadata))
    }

    fn log(&self, record: &log::Record<'_>) {
        let Some(_guard) = Reentry::enter() else { return };
        if let Some(inner) = &self.inner
            && inner.enabled(record.metadata())
        {
            inner.log(record);
        }
        // The SDK's own few lines are for the host's log only. Kept as breadcrumbs they would be sent.
        if record.target() != crate::sink::LOG_TARGET {
            self.capture(record);
        }
    }

    fn flush(&self) {
        if let Some(inner) = &self.inner {
            inner.flush();
        }
    }
}

/// Installs the SDK's logger as the global `log` logger. Does nothing if another logger is already set.
pub(super) fn install(shared: &Arc<Shared>) {
    let logger = SignalLogger::new(shared, None);
    let filter = shared.settings.breadcrumb_level.to_level_filter();
    if log::set_boxed_logger(Box::new(logger)).is_ok() && log::max_level() < filter {
        log::set_max_level(filter);
    }
}

fn path_in(dir: &Path) -> std::path::PathBuf {
    dir.join(FILE_NAME)
}

/// Writes the ring to disk if it changed. Called from the tick.
pub(super) fn save(shared: &Shared) {
    if !shared.allowed(Signal::Logs) {
        return;
    }
    let Some(items) = shared.ring.take_dirty() else { return };
    let mut text = String::new();
    for crumb in &items {
        if let Ok(line) = serde_json::to_string(crumb) {
            text.push_str(&line);
            text.push('\n');
        }
    }
    if report::write_atomic(&path_in(shared.sink.session_dir()), text.as_bytes()).is_err() {
        shared.sink.count_drop("breadcrumbs_write");
    }
}

/// Deletes the saved file. Called at clean shutdown, when there is nothing left to recover.
pub(super) fn remove_saved(shared: &Shared) {
    let _ = std::fs::remove_file(path_in(shared.sink.session_dir()));
}

/// Reads and deletes the file saved by the previous run. Any problem with it gives an empty list.
pub(super) fn take_saved(dir: &Path) -> Vec<Breadcrumb> {
    let path = path_in(dir);
    let too_big = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_SAVED_BYTES);
    let text = if too_big { None } else { std::fs::read_to_string(&path).ok() };
    let _ = std::fs::remove_file(&path);
    let Some(text) = text else { return Vec::new() };
    let mut crumbs = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<Breadcrumb>(line) {
            Ok(crumb) => crumbs.push(crumb),
            Err(_) => return Vec::new(),
        }
    }
    crumbs
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use log::Log;
    use tracer_proto::Signal;

    use super::*;
    use crate::signals::testutil::{Fixture, serial};

    /// Collects what it is given, for checking forwarding.
    struct Collect(Arc<StdMutex<Vec<String>>>, log::LevelFilter);

    impl log::Log for Collect {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= self.1
        }

        fn log(&self, record: &log::Record<'_>) {
            if let Ok(mut lines) = self.0.lock() {
                lines.push(format!("{} {}", record.level(), record.args()));
            }
        }

        fn flush(&self) {
            if let Ok(mut lines) = self.0.lock() {
                lines.push("flush".to_owned());
            }
        }
    }

    fn emit(logger: &dyn log::Log, level: log::Level, target: &str, message: &str) {
        logger.log(&log::Record::builder().level(level).target(target).args(format_args!("{message}")).build());
    }

    #[test]
    fn keeps_lines_at_or_above_the_level_and_drops_the_rest() {
        let fixture = Fixture::new(|config| config.breadcrumb_level = log::Level::Warn);
        let logger = fixture.signals.logger(None);
        emit(&*logger, log::Level::Error, "a", "e");
        emit(&*logger, log::Level::Warn, "a", "w");
        emit(&*logger, log::Level::Info, "a", "i");
        emit(&*logger, log::Level::Debug, "a", "d");
        let crumbs = fixture.signals.shared.ring.snapshot(true);
        let seen: Vec<_> = crumbs.iter().map(|c| (c.level.as_str(), c.message.as_str())).collect();
        assert_eq!(seen, vec![("error", "e"), ("warn", "w")]);
        assert_eq!(crumbs[0].target, "a");
        assert!(crumbs[0].ts > 0);
    }

    #[test]
    fn the_sdks_own_log_lines_are_forwarded_but_never_kept() {
        let fixture = Fixture::new(|config| config.signals = vec![Signal::Logs]);
        let forwarded = Arc::new(StdMutex::new(Vec::new()));
        let logger = fixture.signals.logger(Some(Box::new(Collect(forwarded.clone(), log::LevelFilter::Trace))));
        logger.log(
            &log::Record::builder().level(log::Level::Warn).target(crate::sink::LOG_TARGET).args(format_args!("not sent until the notice")).build(),
        );
        logger.log(&log::Record::builder().level(log::Level::Warn).target("host").args(format_args!("a host line")).build());
        let kept: Vec<String> = fixture.signals.shared.ring.snapshot(true).into_iter().map(|crumb| crumb.message).collect();
        assert_eq!(kept, vec!["a host line".to_owned()]);
        assert_eq!(forwarded.lock().unwrap().len(), 2);
    }

    #[test]
    fn forget_empties_what_is_kept_in_memory() {
        let fixture = Fixture::new(|config| config.signals = vec![Signal::Logs, Signal::Perf]);
        let logger = fixture.signals.logger(None);
        logger.log(&log::Record::builder().level(log::Level::Warn).target("host").args(format_args!("before the forget")).build());
        fixture.signals.tick_at(fixture.at(std::time::Duration::from_secs(1)));
        assert!(fixture.dir.path().join(FILE_NAME).exists());
        let shared = &fixture.signals.shared;
        *shared.last_crash.lock().unwrap() = Some(tracer_proto::Crash::default());
        shared.perf.lock().unwrap().last = Some(tracer_proto::Perf::default());

        fixture.signals.forget();
        assert!(shared.ring.snapshot(true).is_empty());
        assert!(shared.last_crash.lock().unwrap().is_none());
        assert!(shared.last_perf().is_none());
        // A crash after the forget carries nothing from before it.
        let crash = shared.crash_base(tracer_proto::CrashKind::Handled, true);
        assert!(crash.breadcrumbs.is_empty() && crash.perf.is_none());
        // And the emptied ring is not written back out as if it had changed.
        std::fs::remove_file(fixture.dir.path().join(FILE_NAME)).unwrap();
        fixture.signals.tick_at(fixture.at(std::time::Duration::from_secs(2)));
        assert!(!fixture.dir.path().join(FILE_NAME).exists());
    }

    #[test]
    fn ring_is_bounded_and_keeps_the_newest() {
        let fixture = Fixture::new(|config| config.breadcrumb_capacity = 3);
        let logger = fixture.signals.logger(None);
        for n in 0..10 {
            emit(&*logger, log::Level::Info, "t", &format!("line {n}"));
        }
        let messages: Vec<_> = fixture.signals.shared.ring.snapshot(true).into_iter().map(|c| c.message).collect();
        assert_eq!(messages, vec!["line 7", "line 8", "line 9"]);
    }

    #[test]
    fn zero_capacity_keeps_nothing() {
        let fixture = Fixture::new(|config| config.breadcrumb_capacity = 0);
        let logger = fixture.signals.logger(None);
        emit(&*logger, log::Level::Error, "t", "x");
        assert!(fixture.signals.shared.ring.snapshot(true).is_empty());
    }

    #[test]
    fn forwards_every_record_to_the_wrapped_logger() {
        let fixture = Fixture::new(|config| config.breadcrumb_level = log::Level::Warn);
        let lines = Arc::new(StdMutex::new(Vec::new()));
        let logger = fixture.signals.logger(Some(Box::new(Collect(lines.clone(), log::LevelFilter::Debug))));
        emit(&*logger, log::Level::Debug, "t", "quiet");
        emit(&*logger, log::Level::Error, "t", "loud");
        emit(&*logger, log::Level::Trace, "t", "filtered by the wrapped logger");
        logger.flush();
        let got = lines.lock().map(|l| l.clone()).unwrap_or_default();
        assert_eq!(got, vec!["DEBUG quiet", "ERROR loud", "flush"]);
        // Only the line at or above the breadcrumb level became a breadcrumb.
        assert_eq!(fixture.signals.shared.ring.snapshot(true).len(), 1);
    }

    #[test]
    fn enabled_reflects_both_levels() {
        let fixture = Fixture::new(|config| config.breadcrumb_level = log::Level::Info);
        let lines = Arc::new(StdMutex::new(Vec::new()));
        let bare = fixture.signals.logger(None);
        let wrapped = fixture.signals.logger(Some(Box::new(Collect(lines, log::LevelFilter::Trace))));
        let meta = |level| log::Metadata::builder().level(level).target("t").build();
        assert!(bare.enabled(&meta(log::Level::Info)));
        assert!(!bare.enabled(&meta(log::Level::Debug)));
        assert!(wrapped.enabled(&meta(log::Level::Trace)));
    }

    #[test]
    fn lines_are_scrubbed_and_truncated() {
        let fixture = Fixture::new(|_| {});
        let logger = fixture.signals.logger(None);
        let long = "x".repeat(2000);
        emit(&*logger, log::Level::Info, "t", &format!("opened /Users/zed/doc.txt {long}"));
        let crumbs = fixture.signals.shared.ring.snapshot(true);
        assert_eq!(crumbs.len(), 1);
        assert!(crumbs[0].message.len() <= 512);
        assert!(!crumbs[0].message.contains("zed"), "{}", crumbs[0].message);
    }

    #[test]
    fn denied_logs_record_nothing_but_still_forward() {
        let fixture = Fixture::new(|_| {});
        fixture.deny(Signal::Logs);
        let lines = Arc::new(StdMutex::new(Vec::new()));
        let logger = fixture.signals.logger(Some(Box::new(Collect(lines.clone(), log::LevelFilter::Trace))));
        emit(&*logger, log::Level::Error, "t", "x");
        assert!(fixture.signals.shared.ring.snapshot(true).is_empty());
        assert_eq!(lines.lock().map(|l| l.len()).unwrap_or(0), 1);
        fixture.signals.tick_at(fixture.at(std::time::Duration::from_secs(1)));
        assert!(!fixture.dir.path().join(FILE_NAME).exists());
    }

    #[test]
    fn a_record_logged_while_logging_is_ignored() {
        struct Again(Mutex<Option<Arc<dyn log::Log>>>, Arc<StdMutex<u32>>);
        impl log::Log for Again {
            fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, record: &log::Record<'_>) {
                if let Ok(mut count) = self.1.lock() {
                    *count += 1;
                }
                let outer = self.0.lock().ok().and_then(|slot| slot.clone());
                if let Some(outer) = outer {
                    outer.log(record);
                }
            }
            fn flush(&self) {}
        }
        let fixture = Fixture::new(|_| {});
        let count = Arc::new(StdMutex::new(0));
        let again = Arc::new(Again(Mutex::new(None), count.clone()));
        let logger: Arc<dyn log::Log> = Arc::new(SignalLogger::new(&fixture.signals.shared, Some(Box::new(ArcLog(again.clone())))));
        if let Ok(mut slot) = again.0.lock() {
            *slot = Some(logger.clone());
        }
        emit(&*logger, log::Level::Error, "t", "x");
        assert_eq!(*count.lock().expect("lock"), 1, "re-entry must not loop");
        assert_eq!(fixture.signals.shared.ring.snapshot(true).len(), 1);
    }

    struct ArcLog(Arc<dyn log::Log>);
    impl log::Log for ArcLog {
        fn enabled(&self, m: &log::Metadata<'_>) -> bool {
            self.0.enabled(m)
        }
        fn log(&self, r: &log::Record<'_>) {
            self.0.log(r);
        }
        fn flush(&self) {
            self.0.flush();
        }
    }

    #[test]
    fn tick_saves_only_when_changed_and_reload_round_trips() {
        let fixture = Fixture::new(|_| {});
        let logger = fixture.signals.logger(None);
        let file = fixture.dir.path().join(FILE_NAME);
        fixture.signals.tick_at(fixture.at(std::time::Duration::from_secs(1)));
        assert!(!file.exists(), "nothing to save yet");

        emit(&*logger, log::Level::Info, "net", "connected");
        emit(&*logger, log::Level::Error, "net", "dropped");
        fixture.signals.tick_at(fixture.at(std::time::Duration::from_secs(2)));
        assert!(file.exists());
        assert!(!fixture.dir.path().join("breadcrumbs.jsonl.tmp").exists(), "temp file is renamed away");

        // Unchanged ring: the file is not rewritten.
        let _ = std::fs::remove_file(&file);
        fixture.signals.tick_at(fixture.at(std::time::Duration::from_secs(3)));
        assert!(!file.exists());

        emit(&*logger, log::Level::Warn, "net", "retry");
        fixture.signals.tick_at(fixture.at(std::time::Duration::from_secs(4)));
        let crumbs = take_saved(fixture.dir.path());
        let messages: Vec<_> = crumbs.iter().map(|c| c.message.as_str()).collect();
        assert_eq!(messages, vec!["connected", "dropped", "retry"]);
        assert_eq!(crumbs[1].level, "error");
        assert_eq!(crumbs[1].target, "net");
        assert!(!file.exists(), "taking removes the file");
        assert!(take_saved(fixture.dir.path()).is_empty());
    }

    #[test]
    fn corrupt_or_missing_saved_file_gives_an_empty_list() {
        let dir = crate::signals::testutil::TempDir::new();
        assert!(take_saved(dir.path()).is_empty());
        let file = dir.path().join(FILE_NAME);
        std::fs::write(&file, "{\"ts\":1,\"level\":\"info\",\"target\":\"t\",\"message\":\"ok\"}\nnot json\n").expect("write");
        assert!(take_saved(dir.path()).is_empty());
        assert!(!file.exists(), "a corrupt file is still removed");
        std::fs::write(&file, [0xff, 0xfe, 0x00]).expect("write");
        assert!(take_saved(dir.path()).is_empty());
        assert!(!file.exists());
    }

    #[test]
    fn clean_shutdown_removes_the_saved_file() {
        let fixture = Fixture::new(|_| {});
        let logger = fixture.signals.logger(None);
        emit(&*logger, log::Level::Info, "t", "x");
        fixture.signals.tick_at(fixture.at(std::time::Duration::from_secs(1)));
        assert!(fixture.dir.path().join(FILE_NAME).exists());
        fixture.signals.shutdown();
        assert!(!fixture.dir.path().join(FILE_NAME).exists());
        emit(&*logger, log::Level::Info, "t", "late");
        fixture.signals.tick_at(fixture.at(std::time::Duration::from_secs(2)));
        assert!(!fixture.dir.path().join(FILE_NAME).exists(), "no saves after shutdown");
    }

    #[test]
    fn logger_outlives_signals_without_trouble() {
        let fixture = Fixture::new(|_| {});
        let logger = fixture.signals.logger(None);
        let Fixture { signals, .. } = fixture;
        drop(signals);
        emit(&*logger, log::Level::Error, "t", "after drop");
    }

    /// The only test in this crate that installs the global logger, so the first install is this one.
    #[test]
    fn install_sets_the_global_logger_and_tolerates_a_second_one() {
        let _serial = serial();
        let first = Fixture::new_locked(|config| config.logger = crate::config::LoggerMode::Install);
        assert_eq!(log::max_level(), log::LevelFilter::Info, "install raises the max level to the breadcrumb level");
        log::info!("through the global logger");
        log::debug!("below the level");
        let messages: Vec<_> = first.signals.shared.ring.snapshot(true).into_iter().map(|c| c.message).collect();
        assert_eq!(messages, vec!["through the global logger"]);

        // A second start cannot install again; it must not fail or disturb the first.
        let second = Fixture::new_locked(|config| config.logger = crate::config::LoggerMode::Install);
        log::info!("again");
        assert_eq!(first.signals.shared.ring.snapshot(true).len(), 2);
        assert!(second.signals.shared.ring.snapshot(true).is_empty());
    }
}
