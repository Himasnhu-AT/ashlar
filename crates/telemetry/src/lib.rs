//! Always-on crash, performance and usage telemetry to a self-hosted ashlar-trace server.
//!
//! One [`Telemetry`] client per process, started once from a [`TelemetryConfig`]. It is **on
//! whenever an endpoint is configured**: every category is allowed and there is no notice
//! (`force_consent`), `DO_NOT_TRACK` is ignored, and only `TRACER_DISABLED` (a switch for developer
//! machines and CI) turns it off. With no endpoint, or with a bad one, no key, no data folder, or
//! on the web, [`Telemetry::start`] returns a disabled client: every call is a no-op, nothing is
//! recorded and no request is ever made.
//!
//! What an app wires, a few lines each:
//!
//! | Hook | How |
//! |---|---|
//! | crashes | start the client before the app's own panic hook; the SDK's hook chains to it |
//! | log lines → crash breadcrumbs (redacted) | [`Telemetry::wrap_logger`], e.g. with `ashlar_log::logging::install_wrapped` |
//! | commands (id, ok, ms; top level; rapid repeats folded) | `journal.set_observer(telemetry.command_observer())` (`ashlar_command::Journal`) |
//! | saves | [`TelemetryConfig::save_commands`], or [`Telemetry::saved`] |
//! | frames, first frame, focus, input, hang watchdog | [`Telemetry::turn`], [`Telemetry::frame`], [`Telemetry::input`]; `ashlar_ui::telemetry::FrameProbe` does it for an egui loop |
//! | native dialogs | [`Telemetry::pause_hangs_in_dialogs`] (the `ashlar_native::modal` hook) |
//! | quit | [`Telemetry::quit`] (sends what is waiting, at most [`QUIT_FLUSH`]) |
//! | a command-line run ends | [`Telemetry::end`] before every exit, then [`Telemetry::hand_off`] when something is left; [`serve_courier`] first thing in `main` |
//! | privacy page | [`Telemetry::status`] (read-only) |
//!
//! Never sent: command parameters, file names, paths or typed text. Event names and property text
//! are fixed program strings; log lines are redacted on the device ([`redact`]).

mod fold;
mod redact;
#[cfg(feature = "testing")]
pub mod testing;
#[cfg(not(target_arch = "wasm32"))]
mod transport;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ashlar_command::{CommandRun, RunObserver};
/// The ashlar-trace SDK, for tests (`Tracer`, `Config`, transports) and hosts that need more.
pub use ashlar_trace as sdk;
pub use ashlar_trace::{ProcessKind, TurnGuard};
use ashlar_trace::{Prop, Tracer};
pub use fold::REPEAT_WINDOW;
pub use redact::redact;
#[cfg(not(target_arch = "wasm32"))]
pub use transport::Https;

/// How long [`Telemetry::quit`] waits for the last send.
pub const QUIT_FLUSH: Duration = Duration::from_millis(1500);
/// The developer's switch: set (to anything but empty, `0` or `false`), telemetry is off.
pub const DISABLED_ENV: &str = "TRACER_DISABLED";
/// Set in the environment of the background process [`Telemetry::hand_off`] starts.
pub const COURIER_ENV: &str = "ASHLAR_TELEMETRY_COURIER";
/// How long a courier keeps trying to deliver; what is left then waits for the next process.
pub const COURIER_BUDGET: Duration = Duration::from_secs(20);
/// After its first clean delivery a courier stays this long, picking up what runs that end meanwhile
/// leave behind (a script calling the tool many times in a row), so they need not start one each.
pub const COURIER_LINGER: Duration = Duration::from_millis(1500);
/// The file in the SDK's folder a running courier holds locked: one courier per data folder.
#[cfg(not(target_arch = "wasm32"))]
const COURIER_LOCK: &str = "courier.lock";

ashlar_trace::events! {
    pub enum Category {
        Command = "command",
        App = "app",
    }
    pub enum AppEvent {
        /// A top-level command ran: `id` (the registered id), `ok`, `ms`, `repeats` (runs of the
        /// same id folded into this one, see [`REPEAT_WINDOW`]).
        CommandRun = "command.run" in Command,
        /// A document was saved: `kind` (the save command's id, when a command saved), `ms`.
        DocumentSaved = "app.document_saved" in App,
    }
}

/// What to start. Everything the app knows at build time; [`Telemetry::start`] adds the
/// environment overrides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelemetryConfig {
    /// The app's name on the server (lower case).
    pub app_id: String,
    pub version: String,
    /// The server's base URL (no path): `https://…`, or `http://` to a loopback address. `None`:
    /// no telemetry at all.
    pub endpoint: Option<String>,
    /// The app's write key (not a secret: it only allows sending).
    pub write_key: String,
    /// Environment variables that override [`Self::endpoint`] and [`Self::write_key`] (testing).
    pub url_env: Option<String>,
    pub key_env: Option<String>,
    pub process: ProcessKind,
    /// Commands whose success counts as a save (`app.document_saved`).
    pub save_commands: Vec<&'static str>,
}

impl TelemetryConfig {
    pub fn new(app_id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            version: version.into(),
            endpoint: None,
            write_key: String::new(),
            url_env: None,
            key_env: None,
            process: ProcessKind::Gui,
            save_commands: Vec::new(),
        }
    }

    /// The server and its write key; `endpoint = None` means no telemetry.
    pub fn endpoint(mut self, endpoint: Option<&str>, write_key: &str) -> Self {
        self.endpoint = endpoint.map(str::to_string);
        self.write_key = write_key.to_string();
        self
    }

    /// Names of environment variables that override the endpoint and the key.
    pub fn env_overrides(mut self, url: &str, key: &str) -> Self {
        self.url_env = Some(url.to_string());
        self.key_env = Some(key.to_string());
        self
    }

    pub fn process(mut self, process: ProcessKind) -> Self {
        self.process = process;
        self
    }

    pub fn save_commands(mut self, ids: &[&'static str]) -> Self {
        self.save_commands = ids.to_vec();
        self
    }
}

/// Why telemetry is off in this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Off {
    /// No endpoint configured.
    NoEndpoint,
    /// An endpoint without a write key.
    NoKey,
    /// An endpoint that is neither `https://` nor `http://` to this computer.
    BadEndpoint,
    /// `TRACER_DISABLED` is set.
    Disabled,
    /// No data folder to keep the spool in.
    NoDataDir,
    /// The web build, or the app chose to start none (tests).
    NotStarted,
}

impl Off {
    /// A plain sentence; apps may word it their own way.
    pub fn describe(self) -> &'static str {
        match self {
            Off::NoEndpoint => "Telemetry is off in this build: no server is configured, so nothing is recorded or sent.",
            Off::NoKey => "Telemetry is off: a server is configured without a write key, so nothing is recorded or sent.",
            Off::BadEndpoint => "Telemetry is off: the server address must start with https:// (http:// only on this computer).",
            Off::Disabled => "Telemetry is off: TRACER_DISABLED is set (a developer switch), so nothing is recorded or sent.",
            Off::NoDataDir => "Telemetry is off: there is no data folder to keep it in.",
            Off::NotStarted => "Telemetry is not part of this process.",
        }
    }
}

/// What a read-only privacy page shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// A client runs and sends to `server`.
    pub active: bool,
    /// The server's host name (never a path or key), when one is configured.
    pub server: Option<String>,
    /// Why nothing is sent, when `active` is false.
    pub off: Option<Off>,
}

/// A configured server: base URL, write key and host name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub endpoint: String,
    pub key: String,
    pub host: String,
}

/// Whether an environment switch is on: set, and not empty, `0` or `false` (as tracer reads it).
fn switch_on(v: Option<String>) -> bool {
    v.is_some_and(|v| {
        let v = v.trim();
        !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false"))
    })
}

/// The host of an `https://` URL, or of an `http://` URL on this computer; `None` for anything else.
pub fn endpoint_host(url: &str) -> Option<String> {
    let (secure, rest) = if let Some(r) = url.strip_prefix("https://") { (true, r) } else { (false, url.strip_prefix("http://")?) };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || rest.contains(['?', '#']) {
        return None;
    }
    let host =
        if authority.starts_with('[') { authority.split(']').next().map(|h| format!("{h}]"))? } else { authority.split(':').next()?.to_string() };
    let loopback = matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]");
    (secure || loopback).then_some(host)
}

/// Where telemetry goes, from `cfg` and the environment (`env`), or why it is off. `DO_NOT_TRACK`
/// is not read: telemetry is not optional.
pub fn resolve(cfg: &TelemetryConfig, env: &dyn Fn(&str) -> Option<String>) -> Result<Target, Off> {
    if switch_on(env(DISABLED_ENV)) {
        return Err(Off::Disabled);
    }
    let set = |name: &Option<String>| name.as_deref().and_then(env).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let endpoint = set(&cfg.url_env).or_else(|| cfg.endpoint.clone()).ok_or(Off::NoEndpoint)?;
    let key = set(&cfg.key_env).unwrap_or_else(|| cfg.write_key.clone());
    if key.is_empty() {
        return Err(Off::NoKey);
    }
    let host = endpoint_host(&endpoint).ok_or(Off::BadEndpoint)?;
    Ok(Target { endpoint: endpoint.trim_end_matches('/').to_string(), key, host })
}

/// The process's telemetry client. Cheap to clone; every method is a no-op when it is off.
#[derive(Clone)]
pub struct Telemetry {
    tracer: Tracer,
    status: Status,
    save_commands: Arc<[&'static str]>,
    first_frame: Arc<AtomicBool>,
    repeats: Arc<Mutex<fold::Repeats>>,
    /// The process-wide dialog hook this client installed (it holds a clone of the client until
    /// [`Telemetry::quit`] removes it).
    dialog_hook: Arc<Mutex<Option<ashlar_native::modal::ModalHook>>>,
    /// The courier lock of the data folder, for a client [`Telemetry::start`] started (the only kind
    /// that can hand off).
    courier_lock: Option<PathBuf>,
}

impl Default for Telemetry {
    fn default() -> Self {
        Telemetry::off(Off::NotStarted)
    }
}

impl Telemetry {
    /// A client that records and sends nothing.
    pub fn off(why: Off) -> Telemetry {
        Telemetry {
            tracer: Tracer::disabled(),
            status: Status { active: false, server: None, off: Some(why) },
            save_commands: Arc::from(Vec::new()),
            first_frame: Arc::default(),
            repeats: Arc::default(),
            dialog_hook: Arc::default(),
            courier_lock: None,
        }
    }

    /// Start from `cfg` and the process environment; the spool goes in `<data_dir>/tracer`. Off
    /// (see [`Off`]) without an endpoint, with `TRACER_DISABLED`, without a data folder, on the web.
    /// Apps start none under their test harness: call [`Telemetry::off`] there.
    pub fn start(cfg: &TelemetryConfig, data_dir: Option<&Path>) -> Telemetry {
        let target = match resolve(cfg, &|k| std::env::var(k).ok()) {
            Ok(t) => t,
            Err(why) => return Telemetry::off(why),
        };
        let Some(dir) = data_dir else { return Telemetry::off(Off::NoDataDir) };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let transport = Https::new(&target.endpoint);
            let mut telemetry = Telemetry::with_transport(cfg, &target, dir, transport, |_| {});
            if telemetry.is_enabled() {
                telemetry.courier_lock = Some(courier_lock(dir));
            }
            telemetry
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (target, dir);
            Telemetry::off(Off::NotStarted)
        }
    }

    /// Start with a given transport and config tweaks (tests use a fake one with short intervals).
    pub fn with_transport(
        cfg: &TelemetryConfig,
        target: &Target,
        data_dir: &Path,
        transport: impl ashlar_trace::transport::Transport,
        configure: impl FnOnce(&mut ashlar_trace::Config),
    ) -> Telemetry {
        let tracer = Tracer::builder(&target.key, &cfg.app_id, &cfg.version)
            .data_dir(data_dir)
            .endpoint(&target.endpoint)
            .process(cfg.process)
            .listen_all::<AppEvent>()
            // not optional: every category on, no notice, DO_NOT_TRACK ignored
            .force_consent(true)
            .honor_do_not_track(false)
            .transport(transport)
            .configure(|c| {
                // quitting must stay quick, whatever the network does
                c.shutdown_timeout = Duration::from_secs(1);
                configure(c);
            })
            .init();
        let status = if tracer.is_enabled() {
            Status { active: true, server: Some(target.host.clone()), off: None }
        } else {
            Status { active: false, server: None, off: Some(Off::NoDataDir) }
        };
        Telemetry {
            tracer,
            status,
            save_commands: Arc::from(cfg.save_commands.clone()),
            first_frame: Arc::default(),
            repeats: Arc::default(),
            dialog_hook: Arc::default(),
            courier_lock: None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.tracer.is_enabled()
    }

    /// What a read-only privacy page shows.
    pub fn status(&self) -> Status {
        self.status.clone()
    }

    /// The SDK client, for what this crate does not wrap.
    pub fn tracer(&self) -> &Tracer {
        &self.tracer
    }

    /// The command chokepoint's observer (`ashlar_command::Journal::set_observer`); `None` when off.
    /// Reports the registered id, ok and milliseconds, never parameters; runs of one id within
    /// [`REPEAT_WINDOW`] are folded into the next report.
    pub fn command_observer(&self) -> Option<RunObserver> {
        if !self.tracer.is_enabled() {
            return None;
        }
        let me = self.clone();
        Some(Arc::new(move |run: CommandRun| me.record(run)))
    }

    /// Record one command by hand (hosts that do not use the journal observer).
    pub fn command(&self, id: &'static str, ok: bool, took: Duration) {
        self.record(CommandRun { id, ok, took });
    }

    fn record(&self, run: CommandRun) {
        if !self.tracer.is_enabled() {
            return;
        }
        let Some((name, folded)) = self.repeats.lock().unwrap_or_else(PoisonError::into_inner).admit(run.id, Instant::now()) else { return };
        let ms = run.took.as_secs_f64() * 1000.0;
        self.tracer.track_with(
            AppEvent::CommandRun,
            &[("id", Prop::Str(run.id)), ("ok", Prop::Bool(run.ok)), ("ms", Prop::Float(ms)), ("repeats", Prop::Int(i64::from(folded)))],
        );
        if let Some(name) = name {
            self.tracer.track_id(Category::Command, name);
        }
        if run.ok && self.save_commands.contains(&run.id) {
            self.tracer.track_with(AppEvent::DocumentSaved, &[("kind", Prop::Str(run.id)), ("ms", Prop::Float(ms))]);
        }
    }

    /// A document was saved (not through one of [`TelemetryConfig::save_commands`]).
    pub fn saved(&self) {
        self.tracer.track(AppEvent::DocumentSaved);
    }

    /// Wrap the app's logger: every record still reaches it, and the lines it keeps at info or
    /// above also become crash breadcrumbs, redacted ([`redact`]).
    pub fn wrap_logger(&self, inner: Box<dyn log::Log>) -> Box<dyn log::Log> {
        if !self.tracer.is_enabled() {
            return inner;
        }
        Box::new(redact::Breadcrumbs { inner, crumbs: self.tracer.logger(None) })
    }

    /// One turn of the main loop (logic or a UI pass): a turn still open after the hang threshold
    /// is reported as a hang. Ends when the guard is dropped.
    pub fn turn(&self) -> TurnGuard {
        self.tracer.turn()
    }

    /// [`Telemetry::turn`] without the guard, for loops that open and close turns in different
    /// callbacks. A second `begin_turn` keeps the first start; `end_turn` without one does nothing.
    pub fn begin_turn(&self) {
        self.tracer.begin_turn();
    }

    pub fn end_turn(&self) {
        self.tracer.end_turn();
    }

    /// The time one UI pass took; the first call after [`Telemetry::start`] also records the
    /// startup time.
    pub fn frame(&self, took: Duration) {
        self.tracer.frame(took);
        if !self.first_frame.swap(true, Ordering::Relaxed) {
            self.tracer.first_frame();
        }
    }

    /// Window focus, and whether the user did something this turn (active versus idle time).
    pub fn input(&self, focused: bool, active: bool) {
        self.tracer.set_focused(focused);
        if active {
            self.tracer.note_input();
        }
    }

    /// Run `f` (a blocking native call) with hang detection paused.
    pub fn paused<R>(&self, f: impl FnOnce() -> R) -> R {
        self.pause_hangs();
        let r = f();
        self.resume_hangs();
        r
    }

    /// Pause hang detection (pauses nest; each needs its [`Telemetry::resume_hangs`]).
    pub fn pause_hangs(&self) {
        self.tracer.pause_hang_detection();
    }

    pub fn resume_hangs(&self) {
        self.tracer.resume_hang_detection();
    }

    /// Pause hang detection around every native file dialog and message box
    /// (`ashlar_native::modal`). Process-wide; nothing to do when off. The hook keeps the client
    /// alive until [`Telemetry::quit`] removes it, so call `quit` before the process ends: the
    /// session ends (and its last performance window is sent) when the last clone is dropped.
    pub fn pause_hangs_in_dialogs(&self) {
        if !self.tracer.is_enabled() {
            return;
        }
        let tracer = self.tracer.clone();
        let hook: ashlar_native::modal::ModalHook = Arc::new(move |open| {
            if open {
                tracer.pause_hang_detection();
            } else {
                tracer.resume_hang_detection();
            }
        });
        ashlar_native::modal::set_hook(Some(hook.clone()));
        *self.dialog_hook.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    /// Send what is waiting, for at most `timeout`. True when nothing is left that could be sent
    /// (what could not stays on disk and goes with the next launch).
    pub fn flush(&self, timeout: Duration) -> bool {
        self.tracer.flush_blocking(timeout)
    }

    /// A short-lived process (a command-line run) is about to exit, by `std::process::exit` or by
    /// returning from `main`: end the session now, as `exited`, and send what is waiting for at most
    /// `flush`. The end is on disk before the wait, so a short `flush` loses nothing: what could not
    /// be sent stays in the spool, for [`Telemetry::hand_off`], the next run or the app. Call it on
    /// every exit path; later calls, and dropping the clones, do nothing more. Afterwards nothing is
    /// recorded. True when nothing is left that could be sent.
    pub fn end(&self, flush: Duration) -> bool {
        if let Some(hook) = self.dialog_hook.lock().unwrap_or_else(PoisonError::into_inner).take() {
            ashlar_native::modal::clear_if(&hook);
        }
        self.tracer.shutdown_within(flush)
    }

    /// After [`Telemetry::end`] left something unsent: start this program again in the background as
    /// a courier ([`COURIER_ENV`] set, no arguments, no standard streams), which delivers it and exits,
    /// so this process need not wait for the network. Unless one is already running for this data
    /// folder: it takes this run's data too. The program must call [`serve_courier`] first thing in
    /// `main`. Only for a client [`Telemetry::start`] started; true when a courier is on its way.
    pub fn hand_off(&self) -> bool {
        spawn_courier(self.courier_lock.as_deref())
    }

    /// The app is quitting: send what is waiting, for at most [`QUIT_FLUSH`], and remove the
    /// dialog hook this client installed, so that dropping the app's own clones ends the session.
    pub fn quit(&self) {
        if let Some(hook) = self.dialog_hook.lock().unwrap_or_else(PoisonError::into_inner).take() {
            ashlar_native::modal::clear_if(&hook);
        }
        if self.tracer.is_enabled() {
            let _ = self.flush(QUIT_FLUSH);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn courier_lock(data_dir: &Path) -> PathBuf {
    data_dir.join("tracer").join(COURIER_LOCK)
}

#[cfg(target_arch = "wasm32")]
fn spawn_courier(_lock: Option<&Path>) -> bool {
    false
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_courier(lock: Option<&Path>) -> bool {
    let Some(lock) = lock else { return false };
    if courier_running(lock) {
        return true;
    }
    let Ok(program) = std::env::current_exe() else { return false };
    let mut command = std::process::Command::new(program);
    command.env(COURIER_ENV, "1").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        // Its own process group: a Ctrl-C meant for the tool, or a caller ending the tool's group,
        // does not cut the delivery short.
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().is_ok()
}

/// Whether a courier holds the lock at `path` now.
#[cfg(not(target_arch = "wasm32"))]
fn courier_running(path: &Path) -> bool {
    std::fs::OpenOptions::new().read(true).write(true).open(path).is_ok_and(|file| matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
}

/// First thing in `main` of a program that calls [`Telemetry::hand_off`]: in the courier that call
/// started, deliver what the program's runs left in `data_dir` and return true, and the program exits
/// at once (it is not a run of its own). False in every other process, at the cost of one environment
/// lookup. A courier records nothing of its own, starts no courier, and gives up after
/// [`COURIER_BUDGET`]; a second one for the same folder ends at once.
pub fn serve_courier(cfg: &TelemetryConfig, data_dir: Option<&Path>) -> bool {
    if !switch_on(std::env::var(COURIER_ENV).ok()) {
        return false;
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let (Ok(target), Some(dir)) = (resolve(cfg, &|k| std::env::var(k).ok()), data_dir) {
        let transport = Https::new(&target.endpoint);
        run_courier(cfg, &target, dir, transport, |_| {}, COURIER_BUDGET, COURIER_LINGER);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (cfg, data_dir);
    true
}

/// The courier's work, with a given transport (tests).
#[cfg(not(target_arch = "wasm32"))]
fn run_courier(
    cfg: &TelemetryConfig,
    target: &Target,
    data_dir: &Path,
    transport: impl ashlar_trace::transport::Transport,
    configure: impl FnOnce(&mut ashlar_trace::Config),
    budget: Duration,
    linger: Duration,
) -> bool {
    let path = courier_lock(data_dir);
    if path.parent().is_none_or(|dir| std::fs::create_dir_all(dir).is_err()) {
        return false;
    }
    let Ok(lock) = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path) else { return false };
    if lock.try_lock().is_err() {
        return false;
    }
    let tracer = Tracer::builder(&target.key, &cfg.app_id, &cfg.version)
        .data_dir(data_dir)
        .endpoint(&target.endpoint)
        .process(cfg.process)
        .catalog::<AppEvent>()
        .courier(true)
        .force_consent(true)
        .honor_do_not_track(false)
        .transport(transport)
        .configure(|c| {
            c.shutdown_timeout = Duration::from_secs(1);
            configure(c);
        })
        .init();
    let started = Instant::now();
    let left = || budget.saturating_sub(started.elapsed());
    let mut clean = tracer.flush_blocking(left());
    let lingering = Instant::now();
    // Each pass with nothing new to send makes no request.
    while clean && lingering.elapsed() < linger && !left().is_zero() {
        std::thread::sleep(Duration::from_millis(250).min(linger));
        clean = tracer.flush_blocking(left());
    }
    tracer.shutdown_within(Duration::from_secs(1));
    drop(lock);
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    fn cfg() -> TelemetryConfig {
        TelemetryConfig::new("demo", "1.0.0")
            .endpoint(Some("https://t.example.com"), "tk_x")
            .env_overrides("DEMO_TELEMETRY_URL", "DEMO_TELEMETRY_KEY")
    }

    #[test]
    fn no_endpoint_means_off() {
        assert_eq!(resolve(&TelemetryConfig::new("demo", "1"), &env(&[])), Err(Off::NoEndpoint));
        let blank = TelemetryConfig::new("demo", "1").env_overrides("DEMO_TELEMETRY_URL", "DEMO_TELEMETRY_KEY");
        assert_eq!(resolve(&blank, &env(&[("DEMO_TELEMETRY_URL", "  ")])), Err(Off::NoEndpoint));
        let t = Telemetry::start(&TelemetryConfig::new("demo", "1"), Some(Path::new("/nonexistent")));
        assert!(!t.is_enabled() && t.command_observer().is_none() && !t.status().active);
        assert_eq!(t.status().off, Some(Off::NoEndpoint));
    }

    #[test]
    fn do_not_track_is_ignored_and_tracer_disabled_stops_it() {
        for v in ["1", "true", "yes"] {
            let e = move |k: &str| (k == "DO_NOT_TRACK").then(|| v.to_string());
            assert!(resolve(&cfg(), &e).is_ok(), "{v}");
            let e = move |k: &str| (k == DISABLED_ENV).then(|| v.to_string());
            assert_eq!(resolve(&cfg(), &e), Err(Off::Disabled), "{v}");
        }
        for v in ["", "0", "false", "FALSE"] {
            let e = move |k: &str| (k == DISABLED_ENV).then(|| v.to_string());
            assert!(resolve(&cfg(), &e).is_ok(), "{v}");
        }
    }

    #[test]
    fn endpoints_must_be_https_or_loopback() {
        assert_eq!(endpoint_host("https://t.example.com").as_deref(), Some("t.example.com"));
        assert_eq!(endpoint_host("https://t.example.com:8443/").as_deref(), Some("t.example.com"));
        assert_eq!(endpoint_host("http://127.0.0.1:8090").as_deref(), Some("127.0.0.1"));
        assert_eq!(endpoint_host("http://localhost:1").as_deref(), Some("localhost"));
        assert_eq!(endpoint_host("http://[::1]:8090").as_deref(), Some("[::1]"));
        for bad in
            ["http://t.example.com", "ftp://x", "https://", "https://user@t.example.com", "t.example.com", "https://x?y", "http://127.0.0.1.evil.com"]
        {
            assert_eq!(endpoint_host(bad), None, "{bad}");
        }
        let plain = TelemetryConfig::new("demo", "1").endpoint(Some("http://t.example.com"), "tk_x");
        assert_eq!(resolve(&plain, &env(&[])), Err(Off::BadEndpoint));
        let nokey = TelemetryConfig::new("demo", "1").endpoint(Some("https://t.example.com"), "");
        assert_eq!(resolve(&nokey, &env(&[])), Err(Off::NoKey));
        let t = resolve(&cfg(), &env(&[("DEMO_TELEMETRY_URL", "http://127.0.0.1:9/"), ("DEMO_TELEMETRY_KEY", "tk_env")])).unwrap();
        assert_eq!(t, Target { endpoint: "http://127.0.0.1:9".into(), key: "tk_env".into(), host: "127.0.0.1".into() });
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_courier_delivers_what_ended_runs_left_and_only_one_runs_per_folder() {
        use ashlar_trace::proto::{ItemBody, SessionStatus};
        use ashlar_trace::transport::{Reply, Request, Transport, TransportError};
        struct Offline;
        impl Transport for Offline {
            fn send(&mut self, _: &Request<'_>) -> Result<Reply, TransportError> {
                Err(TransportError::Network("offline".into()))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let cfg = cfg().process(ProcessKind::Cli);
        let target = Target { endpoint: "http://127.0.0.1:9".into(), key: "tk_test".into(), host: "127.0.0.1".into() };
        let run = Telemetry::with_transport(&cfg, &target, dir.path(), Offline, |_| {});
        let session = run.tracer().session_id();
        run.command("doc.edit", true, Duration::from_millis(2));
        assert!(!run.end(Duration::from_millis(50)));
        // not started by `start`: nothing to hand off to
        assert!(!run.hand_off());
        std::mem::forget(run);

        // A courier is running: a second one for the same folder ends at once, and a run sees it.
        let lock_path = courier_lock(dir.path());
        let held = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path).unwrap();
        held.lock().unwrap();
        assert!(courier_running(&lock_path));
        let idle = crate::testing::Recorder::default();
        assert!(!run_courier(&cfg, &target, dir.path(), idle.clone(), |_| {}, Duration::from_secs(5), Duration::ZERO));
        assert!(idle.items().is_empty());
        drop(held);
        assert!(!courier_running(&lock_path));

        let fake = crate::testing::Recorder::default();
        let started = std::time::Instant::now();
        assert!(run_courier(
            &cfg,
            &target,
            dir.path(),
            fake.clone(),
            |c| c.tick_interval = Duration::from_millis(10),
            Duration::from_secs(5),
            Duration::from_millis(300)
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
        let of_run: Vec<ItemBody> = fake.envelopes().into_iter().filter(|e| e.session_id == session).flat_map(|e| e.items).map(|i| i.body).collect();
        assert!(of_run.iter().any(|b| matches!(b, ItemBody::SessionEnd(end) if end.status == SessionStatus::Exited)), "{of_run:?}");
        assert!(of_run.iter().any(|b| matches!(b, ItemBody::Event(e) if e.name == "command.run")));
        // nothing of the courier's own: every envelope is the run's
        assert!(fake.envelopes().iter().all(|e| e.session_id == session));
        // and outside a courier, serving is a no-op
        assert!(!serve_courier(&cfg, Some(dir.path())));
    }

    #[test]
    fn a_disabled_client_touches_nothing() {
        let t = Telemetry::off(Off::Disabled);
        struct Null;
        impl log::Log for Null {
            fn enabled(&self, _: &log::Metadata<'_>) -> bool {
                true
            }
            fn log(&self, _: &log::Record<'_>) {}
            fn flush(&self) {}
        }
        let wrapped = t.wrap_logger(Box::new(Null));
        wrapped.log(&log::Record::builder().args(format_args!("x")).build());
        assert_eq!(t.paused(|| 7), 7);
        t.command("doc.edit", true, Duration::from_millis(1));
        t.frame(Duration::from_millis(1));
        t.input(true, true);
        drop(t.turn());
        t.pause_hangs_in_dialogs();
        t.quit();
        assert!(t.end(Duration::from_millis(1)) && !t.hand_off());
        assert!(t.tracer().flush_blocking(Duration::from_millis(1)));
        assert_eq!(t.tracer().install_id(), "");
    }
}
