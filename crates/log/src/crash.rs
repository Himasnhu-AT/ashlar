//! Crash safety: a panic hook that writes a crash log and keeps the last panic for display, and a
//! launch guard that notices when the previous launch died before its first frame.
//!
//! The hook is installed once per process and chains to the hook installed before it (the
//! default one prints to stderr; a telemetry hook reports the crash). Where the log goes, what it
//! is called, whether panics append to one file and whether a backtrace is captured are set by a
//! [`CrashConfig`]; calling [`install_with`] again only replaces the configuration.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

/// Crash logs kept in the logs folder (older `crash-*.log` files are deleted).
pub const KEEP_CRASH_LOGS: usize = 20;
/// Longest panic message kept, in characters.
pub const MAX_MESSAGE: usize = 4096;
/// Recent log lines copied into a crash log.
pub const RECENT_LINES: usize = 200;

/// The last panic seen by the hook.
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PanicRecord {
    pub message: String,
    /// `file:line:column`.
    pub location: String,
    pub thread: String,
    /// Empty when the configuration turned capture off.
    pub backtrace: String,
    pub unix: i64,
    /// Where the crash log was written, if it was.
    pub log_file: Option<PathBuf>,
}

/// Names a crash log after the panic it records. Must not panic (it runs inside the panic hook,
/// where a second panic aborts the process). A name that is not a plain file name (empty, a
/// separator, `..`) falls back to [`default_file_name`].
pub type FileNameFn = Box<dyn Fn(&PanicRecord) -> String + Send + Sync>;

/// How the panic hook records a panic.
pub struct CrashConfig {
    /// Named in the first line of the log (`<app> <version> crashed`).
    pub app: String,
    pub version: String,
    /// Where crash logs go; `None` writes none (the panic is still logged and kept).
    pub logs_dir: Option<PathBuf>,
    /// The log's file name in `logs_dir`.
    pub file_name: FileNameFn,
    /// Append to the file (one per day, say) instead of replacing it.
    pub append: bool,
    /// Capture a backtrace (slow with many frames; off for a CLI that only wants the message).
    pub capture_backtrace: bool,
}

impl CrashConfig {
    /// Today's behaviour: `crash-<unix>-<pid>.log`, one file per panic, with a backtrace.
    pub fn new(app: &str, version: &str, logs_dir: Option<&Path>) -> CrashConfig {
        CrashConfig {
            app: app.to_string(),
            version: version.to_string(),
            logs_dir: logs_dir.map(Path::to_path_buf),
            file_name: Box::new(default_file_name),
            append: false,
            capture_backtrace: true,
        }
    }
}

impl std::fmt::Debug for CrashConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrashConfig")
            .field("app", &self.app)
            .field("version", &self.version)
            .field("logs_dir", &self.logs_dir)
            .field("append", &self.append)
            .field("capture_backtrace", &self.capture_backtrace)
            .finish_non_exhaustive()
    }
}

/// `crash-<unix>-<pid>.log`.
pub fn default_file_name(r: &PanicRecord) -> String {
    format!("crash-{}-{}.log", r.unix, std::process::id())
}

static LAST: Mutex<Option<PanicRecord>> = Mutex::new(None);
static STATE: Mutex<Option<CrashConfig>> = Mutex::new(None);
static INSTALLED: OnceLock<()> = OnceLock::new();

/// The most recent panic (in any thread), if one happened.
pub fn last_panic() -> Option<PanicRecord> {
    LAST.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Take the most recent panic, clearing it.
pub fn take_last_panic() -> Option<PanicRecord> {
    LAST.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// [`install_with`] the default configuration ([`CrashConfig::new`]).
pub fn install(logs_dir: Option<&Path>, app: &str, version: &str) {
    install_with(CrashConfig::new(app, version, logs_dir));
}

/// Install the panic hook once per process (later calls only replace the configuration). The
/// hook runs first and then chains to the hook that was installed before it.
pub fn install_with(config: CrashConfig) {
    *STATE.lock().unwrap_or_else(PoisonError::into_inner) = Some(config);
    if INSTALLED.set(()).is_err() || std::thread::panicking() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        on_panic(info);
        previous(info);
    }));
}

fn payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(non-text panic payload)".into())
}

/// Never panics and never waits for a lock another thread may hold forever.
fn on_panic(info: &std::panic::PanicHookInfo<'_>) {
    let message: String = payload_message(info.payload()).chars().take(MAX_MESSAGE).collect();
    let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
    let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
    let unix = crate::unix_now() as i64;
    let mut record = PanicRecord { message, location, thread, backtrace: String::new(), unix, log_file: None };
    if let Ok(state) = STATE.try_lock()
        && let Some(cfg) = state.as_ref()
    {
        #[cfg(not(target_arch = "wasm32"))]
        if cfg.capture_backtrace {
            record.backtrace = std::backtrace::Backtrace::force_capture().to_string();
        }
        if let Some(dir) = &cfg.logs_dir {
            record.log_file = write_crash_log(dir, cfg, &record);
        }
    }
    log::error!("panic in thread `{}` at {}: {}", record.thread, record.location, record.message);
    if let Ok(mut last) = LAST.try_lock() {
        *last = Some(record);
    }
}

/// `name` when it is one plain file name, else `None`.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn plain_file_name(name: &str) -> Option<&str> {
    let ok = !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.chars().any(|c| c == '/' || c == '\\' || c == ':' || c.is_control());
    ok.then_some(name)
}

fn write_crash_log(dir: &Path, cfg: &CrashConfig, r: &PanicRecord) -> Option<PathBuf> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (dir, cfg, r);
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::fmt::Write as _;
        use std::io::Write as _;
        std::fs::create_dir_all(dir).ok()?;
        let wanted = (cfg.file_name)(r);
        let name = plain_file_name(&wanted).map_or_else(|| default_file_name(r), str::to_string);
        let path = dir.join(name);
        let mut text = String::new();
        if cfg.append {
            let _ = writeln!(text, "=== unix {} pid {}", r.unix, std::process::id());
        }
        let _ = writeln!(text, "{} {} crashed", cfg.app, cfg.version);
        let _ = writeln!(text, "os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
        let _ = writeln!(text, "thread: {}", r.thread);
        let _ = writeln!(text, "at: {}", r.location);
        let _ = writeln!(text, "message: {}\n", r.message);
        if cfg.capture_backtrace {
            let _ = writeln!(text, "backtrace:\n{}\n", r.backtrace);
        }
        let _ = writeln!(text, "recent log lines:");
        for l in crate::logging::try_recent(RECENT_LINES) {
            let _ = writeln!(text, "{:.3} {:5} {}: {}", l.unix, l.level, l.target, l.message);
        }
        if cfg.append {
            text.push('\n');
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok()?;
            f.write_all(text.as_bytes()).ok()?;
            let _ = f.sync_all();
        } else {
            replace_file(&path, text.as_bytes()).ok()?;
        }
        prune(dir);
        Some(path)
    }
}

/// Write a temp file next to `path`, then rename it over `path`: a crash mid-write leaves no
/// torn log.
#[cfg(not(target_arch = "wasm32"))]
fn replace_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let r = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

#[cfg(not(target_arch = "wasm32"))]
fn prune(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut logs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("crash-") && n.ends_with(".log")))
        .collect();
    if logs.len() <= KEEP_CRASH_LOGS {
        return;
    }
    // names start with the unix time: lexical order is close enough, and mtime breaks ties
    logs.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    let excess = logs.len() - KEEP_CRASH_LOGS;
    for p in logs.into_iter().take(excess) {
        let _ = std::fs::remove_file(p);
    }
}

// ---------------------------------------------------------------------------------------------
// Launch guard
// ---------------------------------------------------------------------------------------------

/// How the previous launch ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviousLaunch {
    /// It reached its first frame (or this is the first launch).
    Clean,
    /// It died before its first frame, `count` launches in a row.
    CrashedAtStartup { count: u32 },
}

/// A marker file written at launch and removed once the first frame is on screen. Finding it
/// at the next launch means that launch never got that far (a GPU driver crash, a corrupt
/// layout file…): the runtime then offers safe mode.
#[derive(Debug)]
pub struct LaunchGuard {
    path: Option<PathBuf>,
}

impl LaunchGuard {
    /// Arm the guard in `dir`.
    pub fn begin(dir: &Path) -> (LaunchGuard, PreviousLaunch) {
        let path = dir.join("launch.marker");
        let count = std::fs::read_to_string(&path).ok().map(|s| s.trim().parse::<u32>().unwrap_or(1));
        let previous = match count {
            Some(n) => PreviousLaunch::CrashedAtStartup { count: n.max(1) },
            None => PreviousLaunch::Clean,
        };
        let next = count.unwrap_or(0).saturating_add(1);
        #[cfg(not(target_arch = "wasm32"))]
        let armed = std::fs::create_dir_all(dir).is_ok() && replace_file(&path, next.to_string().as_bytes()).is_ok();
        #[cfg(target_arch = "wasm32")]
        let armed = {
            let _ = next;
            false
        };
        (LaunchGuard { path: armed.then_some(path) }, previous)
    }

    /// A guard that does nothing (no folder to keep the marker in).
    pub fn disarmed() -> LaunchGuard {
        LaunchGuard { path: None }
    }

    /// The app is up: remove the marker.
    pub fn started(&mut self) {
        if let Some(p) = self.path.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_guard_counts_launches_that_never_started() {
        let dir = tempfile::tempdir().unwrap();
        let (g, prev) = LaunchGuard::begin(dir.path());
        assert_eq!(prev, PreviousLaunch::Clean);
        drop(g); // died before `started`
        let (g, prev) = LaunchGuard::begin(dir.path());
        assert_eq!(prev, PreviousLaunch::CrashedAtStartup { count: 1 });
        drop(g);
        let (mut g, prev) = LaunchGuard::begin(dir.path());
        assert_eq!(prev, PreviousLaunch::CrashedAtStartup { count: 2 });
        g.started();
        let (_g, prev) = LaunchGuard::begin(dir.path());
        assert_eq!(prev, PreviousLaunch::Clean);
    }

    #[test]
    fn only_plain_file_names_are_used() {
        for bad in ["", ".", "..", "../x.log", "a/b.log", "a\\b.log", "c:x", "a\nb", &"x".repeat(256)] {
            assert_eq!(plain_file_name(bad), None, "{bad:?}");
        }
        assert_eq!(plain_file_name("crash-2024.log"), Some("crash-2024.log"));
    }
}
