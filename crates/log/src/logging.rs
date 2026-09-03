//! Logging: one `log` backend writing to stderr (optional), a rotating file in the logs folder and
//! an in-memory ring (what a control method such as `app.logs` and crash logs read).
//!
//! The file is written by a thread of its own: a log call on the UI thread formats one line and
//! hands it over, it never waits for the disk. When that thread falls behind, lines are dropped
//! (and counted) rather than blocking. [`flush`] waits for queued lines (call it before a CLI
//! exits). Nothing is ever written to stdout, and a closed stderr is ignored.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock, PoisonError};

use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::filter::Filter;
pub use crate::filter::filter_from_env;

/// Lines kept in memory.
pub const RING_LINES: usize = 2000;
/// Longest message kept (longer ones are cut on a character boundary).
pub const MAX_LINE: usize = 4096;
/// Size at which the log file is rotated.
pub const ROTATE_BYTES: u64 = 4 << 20;
/// Rotated files kept (`name.1.log` … `name.N.log`).
pub const KEEP_FILES: u32 = 3;
/// Lines waiting for the file thread before new ones are dropped.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const FILE_QUEUE: usize = 4096;
/// How long [`Logger::flush`] waits for the file thread.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const FLUSH_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// One log line, as `recent` returns it.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct LogLine {
    /// Seconds since 1970.
    pub unix: f64,
    pub level: String,
    pub target: String,
    pub message: String,
}

struct Ring {
    lines: Mutex<VecDeque<LogLine>>,
    dropped: AtomicU64,
}

fn ring() -> &'static Ring {
    static RING: OnceLock<Ring> = OnceLock::new();
    RING.get_or_init(|| Ring { lines: Mutex::new(VecDeque::with_capacity(RING_LINES)), dropped: AtomicU64::new(0) })
}

/// The newest `limit` lines at `min` level or more severe, oldest first.
pub fn recent(limit: usize, min: Level) -> Vec<LogLine> {
    let lines = ring().lines.lock().unwrap_or_else(PoisonError::into_inner);
    let mut out: Vec<LogLine> = lines.iter().rev().filter(|l| level_of(&l.level).is_some_and(|lv| lv <= min)).take(limit).cloned().collect();
    out.reverse();
    out
}

/// Recent lines without waiting for the lock (for the panic hook, which may run while the
/// panicking thread holds it).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) fn try_recent(limit: usize) -> Vec<LogLine> {
    match ring().lines.try_lock() {
        Ok(lines) => lines.iter().rev().take(limit).rev().cloned().collect(),
        Err(_) => Vec::new(),
    }
}

/// Lines the file writer had to drop because it fell behind.
pub fn dropped() -> u64 {
    ring().dropped.load(Ordering::Relaxed)
}

/// Parse a level name (`error`, `warn`, `info`, `debug`, `trace`), any case.
pub fn level_of(name: &str) -> Option<Level> {
    name.parse().ok()
}

fn push_ring(line: LogLine) {
    let mut lines = ring().lines.lock().unwrap_or_else(PoisonError::into_inner);
    while lines.len() >= RING_LINES {
        lines.pop_front();
    }
    lines.push_back(line);
}

fn clip(mut s: String) -> String {
    if s.len() > MAX_LINE {
        let mut cut = MAX_LINE;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    s
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Msg {
    Line(String),
    /// Answer once every line before this one is written.
    Flush(SyncSender<()>),
}

/// An extra place each formatted line goes (a browser console), without the trailing newline.
pub type Output = Box<dyn Fn(&str) + Send + Sync>;

/// The process logger. Build it with [`Logger::new`], narrow it with [`Logger::with_filter`],
/// wrap it if needed (telemetry breadcrumbs), then [`install`] it.
pub struct Logger {
    filter: Filter,
    stderr: bool,
    file: Option<SyncSender<Msg>>,
    output: Option<Output>,
}

impl Logger {
    /// A logger keeping records at `level` or more severe. With `dir`, lines also go to
    /// `<dir>/<name>.log` (rotated); with `stderr`, to stderr.
    pub fn new(name: &str, dir: Option<&Path>, level: LevelFilter, stderr: bool) -> Logger {
        let file = dir.and_then(|d| start_file_writer(d, name));
        Logger { filter: Filter::new(level), stderr, file, output: None }
    }

    /// Keep only what `filter` allows (replaces the level given to [`Logger::new`]).
    pub fn with_filter(mut self, filter: Filter) -> Logger {
        self.filter = filter;
        self
    }

    /// Also hand every line to `output` (e.g. the browser console on the web).
    pub fn with_output(mut self, output: Output) -> Logger {
        self.output = Some(output);
        self
    }

    /// The filter in use.
    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    /// The level to pass to [`install`] (the most verbose one the filter keeps).
    pub fn max_level(&self) -> LevelFilter {
        self.filter.max_level()
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.filter.enabled(metadata.target(), metadata.level())
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = LogLine {
            unix: crate::unix_now(),
            level: record.level().as_str().to_lowercase(),
            target: record.target().chars().take(128).collect(),
            message: clip(record.args().to_string()),
        };
        let text = format!("{:.3} {:5} {}: {}\n", line.unix, record.level(), line.target, line.message);
        if self.stderr {
            // a closed pipe (`my-cli | head`) is not an error worth dying for
            let _ = std::io::stderr().write_all(text.as_bytes());
        }
        if let Some(out) = &self.output {
            out(text.trim_end());
        }
        if let Some(tx) = &self.file {
            match tx.try_send(Msg::Line(text)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    ring().dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        push_ring(line);
    }

    /// Wait (up to two seconds) until the file thread has written every line logged so far.
    fn flush(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(tx) = &self.file {
            let (ack, done) = std::sync::mpsc::sync_channel(1);
            if tx.send(Msg::Flush(ack)).is_ok() {
                let _ = done.recv_timeout(FLUSH_WAIT);
            }
        }
        if self.stderr {
            let _ = std::io::stderr().flush();
        }
    }
}

/// Install `logger` as the process logger at `level`. Only the first call in a process wins
/// (later ones return false); the in-memory ring is fed either way by whichever logger is ours.
pub fn install(logger: Box<dyn Log>, level: LevelFilter) -> bool {
    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(level);
        true
    } else {
        false
    }
}

/// Flush the installed logger (wait for its file writer); call it before a CLI exits.
pub fn flush() {
    log::logger().flush();
}

/// The level from `<APP>_LOG` or `RUST_LOG` (a plain level name), `info` otherwise. For
/// per-target directives use [`filter_from_env`].
pub fn level_from_env(app_env_prefix: &str) -> LevelFilter {
    for var in [format!("{app_env_prefix}_LOG"), "RUST_LOG".to_string()] {
        if let Ok(v) = std::env::var(&var)
            && let Ok(l) = v.trim().parse::<LevelFilter>()
        {
            return l;
        }
    }
    LevelFilter::Info
}

#[cfg(target_arch = "wasm32")]
fn start_file_writer(_dir: &Path, _name: &str) -> Option<SyncSender<Msg>> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn start_file_writer(dir: &Path, name: &str) -> Option<SyncSender<Msg>> {
    use std::fs;
    if fs::create_dir_all(dir).is_err() {
        return None;
    }
    let stem: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(64).collect();
    let path = dir.join(format!("{stem}.log"));
    let (tx, rx) = std::sync::mpsc::sync_channel::<Msg>(FILE_QUEUE);
    let spawned = std::thread::Builder::new().name("ashlar-log".into()).spawn(move || {
        let mut file = open_append(&path);
        let mut size = file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
        for msg in rx {
            let line = match msg {
                Msg::Line(line) => line,
                Msg::Flush(ack) => {
                    if let Some(f) = &mut file {
                        let _ = f.flush();
                    }
                    let _ = ack.try_send(());
                    continue;
                }
            };
            if size >= ROTATE_BYTES {
                let _ = file.take(); // close before renaming (Windows)
                rotate(&path);
                file = open_append(&path);
                size = 0;
            }
            if let Some(f) = &mut file
                && f.write_all(line.as_bytes()).is_ok()
            {
                size = size.saturating_add(line.len() as u64);
            }
        }
    });
    spawned.ok().map(|_| tx)
}

#[cfg(not(target_arch = "wasm32"))]
fn open_append(path: &Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn rotated(path: &Path, n: u32) -> std::path::PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{stem}.{n}.log"))
}

/// `name.log` → `name.1.log` → … → `name.KEEP.log` (the oldest is deleted).
#[cfg(not(target_arch = "wasm32"))]
fn rotate(path: &Path) {
    use std::fs;
    let _ = fs::remove_file(rotated(path, KEEP_FILES));
    for n in (1..KEEP_FILES).rev() {
        let _ = fs::rename(rotated(path, n), rotated(path, n + 1));
    }
    let _ = fs::rename(path, rotated(path, 1));
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Tests that read the process-wide ring take turns (one floods it).
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn info(logger: &Logger, target: &str, msg: &str) {
        logger.log(&Record::builder().args(format_args!("{msg}")).level(Level::Info).target(target).build());
    }

    #[test]
    fn ring_keeps_the_newest_lines_filtered_by_level() {
        let _serial = serial();
        let logger = Logger::new("t", None, LevelFilter::Trace, false);
        for i in 0..5 {
            info(&logger, "t", &format!("ring-test {i}"));
        }
        logger.log(&Record::builder().args(format_args!("ring-test warn")).level(Level::Warn).target("t").build());
        let warn = recent(10, Level::Warn);
        assert!(warn.iter().any(|l| l.message == "ring-test warn"));
        assert!(warn.iter().all(|l| l.level == "warn" || l.level == "error"));
        let last = recent(RING_LINES, Level::Trace);
        assert!(last.iter().any(|l| l.message == "ring-test 4"));
    }

    #[test]
    fn the_ring_never_grows_past_its_limit() {
        let _serial = serial();
        let logger = Logger::new("t", None, LevelFilter::Trace, false);
        for i in 0..RING_LINES + 50 {
            info(&logger, "ring-limit", &format!("limit {i}"));
        }
        let all = recent(usize::MAX, Level::Trace);
        assert_eq!(all.len(), RING_LINES);
        assert!(recent(3, Level::Trace).len() == 3);
    }

    #[test]
    fn the_filter_decides_per_target() {
        let _serial = serial();
        let logger = Logger::new("t", None, LevelFilter::Trace, false).with_filter(Filter::parse("warn,keep=debug,keep::quiet=off"));
        assert_eq!(logger.max_level(), LevelFilter::Debug);
        let m = |target: &'static str, level| Metadata::builder().target(target).level(level).build();
        assert!(logger.enabled(&m("keep::x", Level::Debug)));
        assert!(!logger.enabled(&m("keep::quiet::y", Level::Error)));
        assert!(!logger.enabled(&m("other", Level::Info)));
        info(&logger, "other", "filtered-out-line");
        info(&logger, "keep::x", "kept-line");
        let lines = recent(RING_LINES, Level::Trace);
        assert!(!lines.iter().any(|l| l.message == "filtered-out-line"));
        assert!(lines.iter().any(|l| l.message == "kept-line"));
    }

    #[test]
    fn output_gets_each_line() {
        let seen = std::sync::Arc::new(Mutex::new(Vec::<String>::new()));
        let s = seen.clone();
        let logger = Logger::new("t", None, LevelFilter::Info, false)
            .with_output(Box::new(move |l| s.lock().unwrap_or_else(PoisonError::into_inner).push(l.into())));
        info(&logger, "out", "to the console");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].ends_with("out: to the console") && !seen[0].ends_with('\n'));
    }

    #[test]
    fn long_lines_are_cut_on_a_char_boundary() {
        let s = clip("é".repeat(MAX_LINE));
        assert!(s.len() <= MAX_LINE + '…'.len_utf8());
        assert!(s.ends_with('…'));
    }

    #[test]
    fn files_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("app.log");
        for (i, text) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            fs::write(&p, text).unwrap();
            rotate(&p);
            assert!(!p.exists(), "round {i}");
        }
        assert_eq!(fs::read_to_string(rotated(&p, 1)).unwrap(), "e");
        assert_eq!(fs::read_to_string(rotated(&p, 2)).unwrap(), "d");
        assert_eq!(fs::read_to_string(rotated(&p, KEEP_FILES)).unwrap(), "c");
        assert!(!rotated(&p, KEEP_FILES + 1).exists());
    }

    #[test]
    fn the_writer_rotates_past_the_size_limit_and_flush_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::new("rot/te st", Some(dir.path()), LevelFilter::Info, false);
        let chunk = "x".repeat(MAX_LINE - 10);
        let lines = (ROTATE_BYTES / chunk.len() as u64 + 20) as usize;
        for i in 0..lines {
            info(&logger, "rot", &chunk);
            if i % 512 == 0 {
                logger.flush(); // keep the queue from filling (that would drop lines)
            }
        }
        info(&logger, "rot", "the last line");
        logger.flush();
        let current = dir.path().join("rot_te_st.log");
        let text = fs::read_to_string(&current).unwrap();
        assert!(text.ends_with("rot: the last line\n"), "flush wrote everything");
        let old = fs::metadata(dir.path().join("rot_te_st.1.log")).unwrap().len();
        assert!(old >= ROTATE_BYTES && old < ROTATE_BYTES + MAX_LINE as u64 + 64, "{old}");
    }
}
