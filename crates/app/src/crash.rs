//! Crash safety: a panic hook that writes a crash log and keeps the last panic for display, and a
//! launch guard that notices when the previous launch died before its first frame.
//!
//! Panics in commands, jobs and views are caught by the runtime (the session survives); the hook
//! still records each one, so the user and the developer can see what went wrong.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use serde::Serialize;

/// Crash logs kept in the logs folder (older ones are deleted).
pub const KEEP_CRASH_LOGS: usize = 20;

/// The last panic seen by the hook.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PanicRecord {
    pub message: String,
    /// `file:line:column`.
    pub location: String,
    pub thread: String,
    pub backtrace: String,
    pub unix: i64,
    /// Where the crash log was written, if it was.
    pub log_file: Option<PathBuf>,
}

struct HookState {
    dir: Option<PathBuf>,
    app: String,
    version: String,
}

static LAST: Mutex<Option<PanicRecord>> = Mutex::new(None);
static STATE: Mutex<Option<HookState>> = Mutex::new(None);
static INSTALLED: OnceLock<()> = OnceLock::new();

/// The most recent panic (in any thread), if one happened.
pub fn last_panic() -> Option<PanicRecord> {
    LAST.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Take the most recent panic, clearing it.
pub fn take_last_panic() -> Option<PanicRecord> {
    LAST.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Install the panic hook once per process (later calls only update the folder and app name).
/// The hook runs first and then chains to the hook that was installed before it (the default
/// one prints to stderr; a telemetry hook reports the crash).
pub fn install(logs_dir: Option<&Path>, app: &str, version: &str) {
    *STATE.lock().unwrap_or_else(PoisonError::into_inner) =
        Some(HookState { dir: logs_dir.map(Path::to_path_buf), app: app.to_string(), version: version.to_string() });
    if INSTALLED.set(()).is_err() || std::thread::panicking() {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            on_panic(info);
            previous(info);
        }));
    }
    #[cfg(target_arch = "wasm32")]
    {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            on_panic(info);
            web_sys::console::error_1(&format!("panic: {info}").into());
            previous(info);
        }));
    }
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
    let message: String = payload_message(info.payload()).chars().take(4096).collect();
    let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
    let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
    #[cfg(not(target_arch = "wasm32"))]
    let backtrace = std::backtrace::Backtrace::force_capture().to_string();
    #[cfg(target_arch = "wasm32")]
    let backtrace = String::new();
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mut record = PanicRecord { message, location, thread, backtrace, unix, log_file: None };
    if let Ok(state) = STATE.try_lock()
        && let Some(s) = state.as_ref()
        && let Some(dir) = &s.dir
    {
        record.log_file = write_crash_log(dir, &s.app, &s.version, &record);
    }
    log::error!("panic in thread `{}` at {}: {}", record.thread, record.location, record.message);
    if let Ok(mut last) = LAST.try_lock() {
        *last = Some(record);
    }
}

fn write_crash_log(dir: &Path, app: &str, version: &str, r: &PanicRecord) -> Option<PathBuf> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (dir, app, version, r);
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(format!("crash-{}-{}.log", r.unix, std::process::id()));
        let mut text = String::new();
        let _ = writeln!(text, "{app} {version} crashed");
        let _ = writeln!(text, "os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
        let _ = writeln!(text, "thread: {}", r.thread);
        let _ = writeln!(text, "at: {}", r.location);
        let _ = writeln!(text, "message: {}\n", r.message);
        let _ = writeln!(text, "backtrace:\n{}\n", r.backtrace);
        let _ = writeln!(text, "recent log lines:");
        for l in crate::logging::try_recent(200) {
            let _ = writeln!(text, "{:.3} {:5} {}: {}", l.unix, l.level, l.target, l.message);
        }
        ashlar_fs::atomic_write(&path, text.as_bytes()).ok()?;
        prune(dir);
        Some(path)
    }
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
        let armed = std::fs::create_dir_all(dir).is_ok() && ashlar_fs::atomic_write(&path, next.to_string().as_bytes()).is_ok();
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
    fn the_hook_writes_a_crash_log_and_keeps_the_last_panic() {
        let dir = tempfile::tempdir().unwrap();
        install(Some(dir.path()), "hooktest", "0.0.1");
        let r = std::panic::catch_unwind(|| {
            let v: Vec<u8> = Vec::new();
            #[allow(clippy::panic)]
            if v.is_empty() {
                panic!("boom from the crash test");
            }
        });
        assert!(r.is_err());
        // the only test in this binary that installs the hook
        let last = last_panic().expect("recorded");
        assert!(last.message.contains("boom from the crash test"));
        let text = std::fs::read_to_string(last.log_file.expect("written")).unwrap();
        assert!(text.contains("hooktest 0.0.1 crashed"));
        assert!(text.contains("boom from the crash test"));
    }
}
