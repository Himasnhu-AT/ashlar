//! Sessions: which app successive commands talk to, kept in a small file between invocations; the
//! control-key discovery that lets `connect` and `--app` find a running app; and `open`, which
//! launches one.
//!
//! Layout under the state folder ([`Env::home`]):
//!
//! ```text
//! sessions/<name>.json        {port, key_dir, pid?, exe?, log?, launched, trace?}
//! sessions/<name>/key/        the key folder handed to a launched app (0700)
//! sessions/<name>/app.log     a launched app's stdout and stderr
//! screenshots/                default place for `screenshot` without a path
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use ashlar_control::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The environment variable a launched app reads to know where to write its control key.
pub use ashlar_control::key::KEY_DIR_ENV;

/// Where state lives and where apps keep their settings. [`Env::from_os`] for the real thing; tests
/// point both somewhere private.
#[derive(Debug, Clone)]
pub struct Env {
    /// State folder: sessions and default screenshots.
    pub home: PathBuf,
    /// Folders searched (a few levels deep) for `control-key.local-<port>` files.
    pub config_roots: Vec<PathBuf>,
    /// The session used when `--session` is not given.
    pub session: String,
    /// `ASHLAR_CONTROL_KEY_DIR`, if set: the key folder for `connect` / `--port` without `--key-dir`.
    pub key_dir: Option<PathBuf>,
    /// `ASHLAR_DEBUG_NO_RAISE`: screenshots never raise a covered window (automated runs must not
    /// steal focus from the person at the machine).
    pub no_raise: bool,
}

impl Env {
    /// `ASHLAR_DEBUG_HOME` (or the user cache folder + `ashlar-debug`), the OS settings folders,
    /// `ASHLAR_DEBUG_SESSION` (or `default`), `ASHLAR_DEBUG_NO_RAISE`.
    pub fn from_os() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        let base = directories::BaseDirs::new();
        let home = var("ASHLAR_DEBUG_HOME").map(PathBuf::from).unwrap_or_else(|| {
            base.as_ref().map(|b| b.cache_dir().join("ashlar-debug")).unwrap_or_else(|| std::env::temp_dir().join("ashlar-debug"))
        });
        let mut config_roots: Vec<PathBuf> = Vec::new();
        if let Some(b) = &base {
            for d in [b.config_dir(), b.config_local_dir(), b.preference_dir(), b.data_dir()] {
                if !config_roots.iter().any(|r| r == d) {
                    config_roots.push(d.to_path_buf());
                }
            }
        }
        let session = var("ASHLAR_DEBUG_SESSION").and_then(|s| s.into_string().ok()).unwrap_or_else(|| "default".into());
        let no_raise = var("ASHLAR_DEBUG_NO_RAISE").is_some_and(|v| truthy(&v.to_string_lossy()));
        Self { home, config_roots, session, key_dir: var(KEY_DIR_ENV).map(PathBuf::from), no_raise }
    }

    /// The folders of `from_os` under `root` instead (tests): state in `<root>/home`, settings
    /// searched in `<root>/config`, nothing taken from the environment.
    pub fn under(root: &Path) -> Self {
        Self { home: root.join("home"), config_roots: vec![root.join("config")], session: "default".into(), key_dir: None, no_raise: false }
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.home.join("sessions")
    }

    pub fn session_file(&self, name: &str) -> PathBuf {
        self.sessions_dir().join(format!("{name}.json"))
    }

    /// The folder holding a launched app's key folder and log.
    pub fn session_dir(&self, name: &str) -> PathBuf {
        self.sessions_dir().join(name)
    }
}

/// An environment flag's value: anything but empty, `0`, `false`, `no` or `off` turns it on.
pub fn truthy(v: &str) -> bool {
    !matches!(v.trim().to_ascii_lowercase().as_str(), "" | "0" | "false" | "no" | "off")
}

/// A trace being recorded: a screenshot and a snapshot after every action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trace {
    pub dir: PathBuf,
    /// Steps recorded so far.
    pub step: u32,
}

/// What a session remembers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub port: u16,
    pub key_dir: PathBuf,
    /// The app's process, when `open` started it.
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub exe: Option<String>,
    /// Where a launched app's output goes.
    #[serde(default)]
    pub log: Option<PathBuf>,
    /// Started by `open` (so `close` quits it), not found by `connect`.
    #[serde(default)]
    pub launched: bool,
    #[serde(default)]
    pub trace: Option<Trace>,
    /// Screenshots in this session never raise the window (`open --no-raise`).
    #[serde(default)]
    pub no_raise: bool,
}

/// A session name: `[A-Za-z0-9_-]{1,64}` (it becomes a file name).
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("session name `{name}`: use 1–64 letters, digits, `_` or `-`"));
    }
    Ok(())
}

/// The session `name`, if one is recorded.
pub fn load(env: &Env, name: &str) -> Result<Option<Session>, String> {
    check_name(name)?;
    let p = env.session_file(name);
    let text = match std::fs::read(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", p.display())),
    };
    if text.len() > 1024 * 1024 {
        return Err(format!("{} is not a session file (too large)", p.display()));
    }
    serde_json::from_slice(&text).map(Some).map_err(|e| format!("{} is damaged ({e}); `close` it or delete the file", p.display()))
}

/// Record a session (written to a temporary file, then renamed, so a reader never sees half).
pub fn save(env: &Env, name: &str, s: &Session) -> Result<(), String> {
    check_name(name)?;
    let dir = env.sessions_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let p = env.session_file(name);
    let tmp = dir.join(format!(".{name}.json.{}", std::process::id()));
    let text = serde_json::to_vec_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("cannot write {}: {e}", p.display())
    })
}

/// Forget a session: its file, and the folder `open` made for it.
pub fn remove(env: &Env, name: &str) -> Result<(), String> {
    check_name(name)?;
    let _ = std::fs::remove_dir_all(env.session_dir(name));
    match std::fs::remove_file(env.session_file(name)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot remove the session file: {e}")),
    }
}

/// Every recorded session name, sorted.
pub fn list(env: &Env) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(env.sessions_dir()) else { return Vec::new() };
    let mut names: Vec<String> = rd
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(String::from))
        .filter(|n| check_name(n).is_ok())
        .collect();
    names.sort();
    names
}

/// A control key file found on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct FoundKey {
    pub port: u16,
    /// The folder holding it (what [`Client::connect`] takes).
    pub dir: PathBuf,
    pub modified: SystemTime,
}

const KEY_PREFIX: &str = "control-key.local-";
/// How deep under a settings root a key may sit (`<root>/<org>/<app>/config/control-key…`).
const SCAN_DEPTH: usize = 4;
/// Most directory entries looked at in one scan (a huge settings folder cannot stall a command).
const SCAN_BUDGET: usize = 50_000;

/// Key files under `roots`, newest first: only for `port` when given, only in folders whose path
/// contains `app` (case-insensitive) when given.
pub fn find_keys(roots: &[PathBuf], port: Option<u16>, app: Option<&str>) -> Vec<FoundKey> {
    let mut found = Vec::new();
    let mut budget = SCAN_BUDGET;
    for r in roots {
        scan(r, SCAN_DEPTH, &mut budget, &mut found);
    }
    let app = app.map(str::to_lowercase);
    found.retain(|k| port.is_none_or(|p| p == k.port) && app.as_deref().is_none_or(|a| k.dir.to_string_lossy().to_lowercase().contains(a)));
    found.sort_by_key(|k| std::cmp::Reverse(k.modified));
    found.dedup_by(|a, b| a.port == b.port && a.dir == b.dir);
    found
}

fn scan(dir: &Path, depth: usize, budget: &mut usize, out: &mut Vec<FoundKey>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.filter_map(Result::ok) {
        if *budget == 0 {
            return;
        }
        *budget = budget.saturating_sub(1);
        let Ok(ft) = e.file_type() else { continue };
        let name = e.file_name();
        let name = name.to_string_lossy();
        if ft.is_file() {
            if let Some(port) = name.strip_prefix(KEY_PREFIX).and_then(|p| p.parse::<u16>().ok()) {
                let modified = e.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
                out.push(FoundKey { port, dir: dir.to_path_buf(), modified });
            }
        } else if ft.is_dir() && depth > 0 && !name.starts_with('.') {
            scan(&e.path(), depth.saturating_sub(1), budget, out);
        }
    }
}

/// Connect and check the app answers `app.ping`.
pub fn ping(port: u16, key_dir: &Path, timeout: Duration) -> Result<Client, String> {
    let mut c = Client::connect(port, key_dir).map_err(|e| e.to_string())?;
    c.set_timeout(timeout);
    c.call("app.ping", json!({})).map_err(|e| e.to_string())?;
    Ok(c)
}

/// A port nobody listens on right now (the OS picks it).
pub fn free_port() -> Result<u16, String> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("cannot find a free port: {e}"))?;
    l.local_addr().map(|a| a.port()).map_err(|e| e.to_string())
}

/// The last `n` lines of a file (at most the last 64 KiB are read).
pub fn tail(p: &Path, n: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(p) else { return String::new() };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let _ = f.seek(SeekFrom::Start(len.saturating_sub(64 * 1024)));
    let mut buf = Vec::new();
    let _ = f.take(64 * 1024).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();
    lines.get(lines.len().saturating_sub(n)..).unwrap_or(&[]).join("\n")
}

fn private_dir(p: &Path) -> Result<(), String> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(p).map_err(|e| format!("cannot create {}: {e}", p.display()))
}

/// Start `exe args… --control <port>` with its key folder in the session folder and its output in
/// the session log, and wait until it answers `app.ping` (or exits, or `wait` passes). An app that
/// ignores `ASHLAR_CONTROL_KEY_DIR` is still found: its key is looked for in the settings folders.
pub fn launch(env: &Env, name: &str, exe: &str, args: &[String], wait: Duration) -> Result<(Session, Client), String> {
    check_name(name)?;
    let dir = env.session_dir(name);
    let _ = std::fs::remove_dir_all(&dir);
    let key_dir = dir.join("key");
    private_dir(&key_dir)?;
    let log_path = dir.join("app.log");
    let log = std::fs::File::create(&log_path).map_err(|e| format!("cannot create {}: {e}", log_path.display()))?;
    let log2 = log.try_clone().map_err(|e| e.to_string())?;
    let port = free_port()?;
    let mut cmd = Command::new(exe);
    cmd.args(args).arg("--control").arg(port.to_string()).env(KEY_DIR_ENV, &key_dir).stdin(Stdio::null()).stdout(log).stderr(log2);
    // Its own process group: Ctrl-C in this terminal does not reach the app we leave running.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(|e| format!("cannot start {exe}: {e}"))?;
    let started = Instant::now();
    let mut last = String::from("no control key yet");
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let out = tail(&log_path, 20);
            let out = if out.is_empty() { String::new() } else { format!("; its last output:\n{out}") };
            return Err(format!("{exe} exited ({status}) before answering on its control port{out}"));
        }
        let mut dirs = vec![key_dir.clone()];
        if !ashlar_control::key::key_path(&key_dir, port).exists() {
            dirs.extend(find_keys(&env.config_roots, Some(port), None).into_iter().map(|k| k.dir));
        }
        for d in &dirs {
            if !ashlar_control::key::key_path(d, port).exists() {
                continue;
            }
            match ping(port, d, Duration::from_secs(5)) {
                Ok(c) => {
                    let s = Session {
                        port,
                        key_dir: d.clone(),
                        pid: Some(child.id()),
                        exe: Some(exe.to_string()),
                        log: Some(log_path),
                        launched: true,
                        trace: None,
                        no_raise: false,
                    };
                    return Ok((s, c));
                }
                Err(e) => last = e,
            }
        }
        if started.elapsed() >= wait {
            let _ = child.kill();
            let _ = child.wait();
            let out = tail(&log_path, 20);
            let out = if out.is_empty() { String::new() } else { format!("\nits last output:\n{out}") };
            return Err(format!(
                "{exe} did not answer on 127.0.0.1:{port} within {} ms ({last}); was it built with a control port (`--control <port>`)?{out}",
                wait.as_millis()
            ));
        }
        std::thread::sleep(Duration::from_millis(40));
    }
}

/// Stop a process we started, when asking it to quit did not work.
pub fn kill(pid: u32) {
    #[cfg(unix)]
    let _ = Command::new("kill").arg(pid.to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    #[cfg(windows)]
    let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new().prefix(&format!("ashlar-debug-session-{tag}-")).tempdir().unwrap()
    }

    fn env(home: &Path) -> Env {
        Env { home: home.to_path_buf(), config_roots: vec![home.join("config")], session: "default".into(), key_dir: None, no_raise: false }
    }

    #[test]
    fn sessions_round_trip_and_are_listed() {
        let tmp = temp("store");
        let home = tmp.path();
        let env = env(home);
        assert_eq!(load(&env, "default").unwrap(), None);
        let s = Session { port: 7, key_dir: home.join("k"), pid: Some(3), exe: None, log: None, launched: true, trace: None, no_raise: false };
        save(&env, "default", &s).unwrap();
        save(&env, "other", &s).unwrap();
        assert_eq!(load(&env, "default").unwrap(), Some(s));
        assert_eq!(list(&env), ["default", "other"]);
        remove(&env, "default").unwrap();
        remove(&env, "default").unwrap();
        assert_eq!(list(&env), ["other"]);
        std::fs::write(env.session_file("bad"), "{").unwrap();
        assert!(load(&env, "bad").unwrap_err().contains("damaged"));
        for bad in ["", "../x", "a b", &"x".repeat(65)] {
            assert!(load(&env, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keys_are_found_by_port_and_app() {
        let tmp = temp("keys");
        let home = tmp.path();
        let notes = home.join("config/org.example.Notes/config");
        let other = home.join("config/Paint");
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(notes.join("control-key.local-7001"), "k").unwrap();
        std::fs::write(other.join("control-key.local-7002"), "k").unwrap();
        std::fs::write(other.join("control-key.local-nope"), "k").unwrap();
        let roots = [home.join("config")];
        let ports = |p, a| find_keys(&roots, p, a).into_iter().map(|k| k.port).collect::<Vec<_>>();
        let mut all = ports(None, None);
        all.sort();
        assert_eq!(all, [7001, 7002]);
        assert_eq!(ports(Some(7002), None), [7002]);
        assert_eq!(ports(None, Some("notes")), [7001]);
        assert_eq!(ports(Some(7001), Some("paint")), Vec::<u16>::new());
        assert_eq!(find_keys(&roots, Some(7001), None)[0].dir, notes);
        // Too deep: not found.
        let deep = home.join("config/a/b/c/d/e");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("control-key.local-7003"), "k").unwrap();
        assert_eq!(ports(Some(7003), None), Vec::<u16>::new());
    }

    #[test]
    fn flags_from_the_environment() {
        for on in ["1", "true", "YES", "on"] {
            assert!(truthy(on), "{on}");
        }
        for off in ["", "0", "false", "No", " off "] {
            assert!(!truthy(off), "{off:?}");
        }
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let tmp = temp("tail");
        let home = tmp.path();
        let p = home.join("log");
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        assert_eq!(tail(&p, 2), "b\nc");
        assert_eq!(tail(&p, 9), "a\nb\nc");
        assert_eq!(tail(&home.join("missing"), 2), "");
    }
}
