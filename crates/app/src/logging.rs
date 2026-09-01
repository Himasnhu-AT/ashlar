//! Logging: one `log` backend writing to stderr, a rotating file in the logs folder and an
//! in-memory ring (what `app.logs` and crash logs read).
//!
//! The file is written by a thread of its own: a log call on the UI thread formats one line and
//! hands it over, it never waits for the disk. When that thread falls behind, lines are dropped
//! (and counted) rather than blocking.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Mutex, OnceLock, PoisonError};

use log::{Level, LevelFilter, Log, Metadata, Record};
use serde::Serialize;

/// Lines kept in memory.
pub const RING_LINES: usize = 2000;
/// Longest message kept (longer ones are cut on a character boundary).
pub const MAX_LINE: usize = 4096;
/// Size at which the log file is rotated.
pub const ROTATE_BYTES: u64 = 4 << 20;
/// Rotated files kept (`name.1.log` … `name.N.log`).
pub const KEEP_FILES: u32 = 3;
/// Lines waiting for the file thread before new ones are dropped.
const FILE_QUEUE: usize = 4096;

/// One log line, as `app.logs` returns it.
#[derive(Clone, Debug, PartialEq, Serialize)]
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
    if lines.len() >= RING_LINES {
        lines.pop_front();
    }
    lines.push_back(line);
}

fn unix_now() -> f64 {
    #[cfg(not(target_arch = "wasm32"))]
    return std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    #[cfg(target_arch = "wasm32")]
    0.0
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

/// The runtime's logger. Build it with [`Logger::new`], wrap it if needed (telemetry
/// breadcrumbs), then [`install`] it.
pub struct Logger {
    level: LevelFilter,
    stderr: bool,
    file: Option<SyncSender<String>>,
}

impl Logger {
    /// A logger at `level`. With `dir`, lines also go to `<dir>/<name>.log` (rotated).
    pub fn new(name: &str, dir: Option<&Path>, level: LevelFilter, stderr: bool) -> Logger {
        let file = dir.and_then(|d| start_file_writer(d, name));
        Logger { level, stderr, file }
    }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = LogLine {
            unix: unix_now(),
            level: record.level().as_str().to_lowercase(),
            target: record.target().chars().take(128).collect(),
            message: clip(record.args().to_string()),
        };
        let text = format!("{:.3} {:5} {}: {}\n", line.unix, record.level(), line.target, line.message);
        if self.stderr {
            let _ = std::io::stderr().write_all(text.as_bytes());
        }
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(&text.trim_end().into());
        if let Some(tx) = &self.file {
            match tx.try_send(text) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    ring().dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        push_ring(line);
    }

    fn flush(&self) {}
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

/// The level from `<APP>_LOG` or `RUST_LOG` (a plain level name), `info` otherwise.
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

fn start_file_writer(dir: &Path, name: &str) -> Option<SyncSender<String>> {
    if fs::create_dir_all(dir).is_err() {
        return None;
    }
    let stem: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(64).collect();
    let path = dir.join(format!("{stem}.log"));
    let (tx, rx) = sync_channel::<String>(FILE_QUEUE);
    let spawned = std::thread::Builder::new().name("ashlar-log".into()).spawn(move || {
        let mut file = open_append(&path);
        let mut size = file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
        for line in rx {
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

fn open_append(path: &Path) -> Option<File> {
    OpenOptions::new().create(true).append(true).open(path).ok()
}

fn rotated(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!("{stem}.{n}.log"))
}

/// `name.log` → `name.1.log` → … → `name.KEEP.log` (the oldest is deleted).
fn rotate(path: &Path) {
    let _ = fs::remove_file(rotated(path, KEEP_FILES));
    for n in (1..KEEP_FILES).rev() {
        let _ = fs::rename(rotated(path, n), rotated(path, n + 1));
    }
    let _ = fs::rename(path, rotated(path, 1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_newest_lines_filtered_by_level() {
        let logger = Logger::new("t", None, LevelFilter::Trace, false);
        for i in 0..5 {
            logger.log(&Record::builder().args(format_args!("ring-test {i}")).level(Level::Info).target("t").build());
        }
        logger.log(&Record::builder().args(format_args!("ring-test warn")).level(Level::Warn).target("t").build());
        let warn = recent(10, Level::Warn);
        assert!(warn.iter().any(|l| l.message == "ring-test warn"));
        assert!(warn.iter().all(|l| l.level == "warn" || l.level == "error"));
        let last = recent(2, Level::Trace);
        assert_eq!(last.len(), 2);
        assert_eq!(last.last().map(|l| l.message.as_str()), Some("ring-test warn"));
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
        fs::write(&p, "a").unwrap();
        rotate(&p);
        fs::write(&p, "b").unwrap();
        rotate(&p);
        assert!(!p.exists());
        assert_eq!(fs::read_to_string(rotated(&p, 1)).unwrap(), "b");
        assert_eq!(fs::read_to_string(rotated(&p, 2)).unwrap(), "a");
    }
}
