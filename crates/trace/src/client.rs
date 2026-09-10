//! The public handle and its builder.

use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use ashlar_trace_proto::{CatalogEvent, ConsentCategory, Gpu, ProcessKind, Signal};

use crate::catalog::{Event, EventCategory, Prop};
use crate::config::{Config, LoggerMode};
use crate::pipeline::Pipeline;
use crate::signals::Signals;
use crate::signals::rt::{Counter, Gauge};
use crate::sink::Sink;
use crate::transport::Transport;

/// Configures and starts the SDK. Get one from [`Tracer::builder`].
pub struct Builder {
    config: Config,
    transport: Option<Box<dyn Transport>>,
}

impl Builder {
    /// Where the SDK keeps its files: a `tracer` folder inside the host's own data directory.
    pub fn data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.config.data_dir = dir.into();
        self
    }

    /// Server base URL, such as `https://telemetry.example.com`. Without one nothing is sent.
    pub fn endpoint(mut self, url: impl Into<String>) -> Self {
        self.config.endpoint = Some(url.into());
        self
    }

    /// Build identifier such as a commit hash.
    pub fn build(mut self, build: impl Into<String>) -> Self {
        self.config.build = Some(build.into());
        self
    }

    pub fn process(mut self, kind: ProcessKind) -> Self {
        self.config.process = kind;
        self
    }

    /// Declares the host's events without listening to any. The `listen` calls declare the events
    /// themselves, in any order, so this is only needed to send the catalog for events the host does not
    /// listen to in this build, or with a hand-written [`EventCategory`] that leaves `EVENTS` empty.
    pub fn catalog<E: Event>(mut self) -> Self {
        self.declare_all::<E>();
        self
    }

    /// Listens to every event in a category.
    pub fn listen<C: EventCategory>(mut self, category: C) -> Self {
        self.declare(C::EVENTS.iter().map(|(name, category)| (*name, category.name())));
        self.config.listen_categories.push(category.name().to_owned());
        self
    }

    /// Listens to individual events.
    pub fn listen_events<E: Event>(mut self, events: &[E]) -> Self {
        self.declare_all::<E>();
        self.config.listen_events.extend(events.iter().map(|event| event.name().to_owned()));
        self
    }

    /// Listens to every declared event.
    pub fn listen_all<E: Event>(mut self) -> Self {
        self.declare_all::<E>();
        self.config.listen_categories.extend(<E::Category as EventCategory>::ALL.iter().map(|category| category.name().to_owned()));
        self
    }

    fn declare_all<E: Event>(&mut self) {
        self.declare(E::ALL.iter().map(|event| (event.name(), event.category().name())));
    }

    /// Adds events to the catalog, once each.
    fn declare(&mut self, events: impl Iterator<Item = (&'static str, &'static str)>) {
        for (name, category) in events {
            if !self.config.catalog.events.iter().any(|known| known.name == name) {
                self.config.catalog.events.push(CatalogEvent { name: name.to_owned(), category: category.to_owned() });
            }
        }
    }

    /// Lets the SDK send before [`Tracer::notice_shown`] has ever been called. By default nothing
    /// leaves the machine until the user has seen the host's first-run notice; data is recorded and
    /// kept on disk meanwhile. Set this only when the notice is shown somewhere else, such as in an
    /// installer.
    pub fn send_before_notice(mut self, allowed: bool) -> Self {
        self.config.send_before_notice = allowed;
        self
    }

    /// Whether `DO_NOT_TRACK` in the environment switches the SDK off (the default, `true`). A host
    /// whose telemetry is not optional sets `false`; `TRACER_DISABLED` still switches it off.
    pub fn honor_do_not_track(mut self, honor: bool) -> Self {
        self.config.honor_do_not_track = honor;
        self
    }

    /// For a host whose telemetry is not optional: every category is allowed and the notice counts as
    /// shown from `init`, whatever choice an earlier run saved (the saved file is left as it is), and
    /// `set_consent` and `notice_shown` do nothing. Off by default. Combine with
    /// `honor_do_not_track(false)` to ignore `DO_NOT_TRACK` too; `TRACER_DISABLED` still switches the
    /// SDK off.
    pub fn force_consent(mut self, force: bool) -> Self {
        self.config.force_consent = force;
        self
    }

    /// Makes this process a courier: it only sends what other processes of the host left in the spool,
    /// for a short-lived tool that hands its unsent data to a background process instead of waiting for
    /// the network itself. It reports earlier runs that ended uncleanly, as every process does, but opens
    /// no session of its own (no session start or end, no heartbeats, no marker) and starts no signal
    /// module (no panic hook, watchdog or logger). Use [`Tracer::flush_blocking`], then drop it.
    pub fn courier(mut self, courier: bool) -> Self {
        self.config.courier = courier;
        self
    }

    /// Replaces the set of built-in signals. All are on by default.
    pub fn signals(mut self, signals: &[Signal]) -> Self {
        self.config.signals = signals.to_vec();
        self
    }

    /// Switches one built-in signal off.
    pub fn without(mut self, signal: Signal) -> Self {
        self.config.signals.retain(|enabled| *enabled != signal);
        self
    }

    /// Graphics adapter details, which only the host knows.
    pub fn gpu(mut self, gpu: Gpu) -> Self {
        self.config.gpu = Some(gpu);
        self
    }

    pub fn logger(mut self, mode: LoggerMode) -> Self {
        self.config.logger = mode;
        self
    }

    /// Mirrors everything that would be sent into `outbox.jsonl` in the SDK's folder.
    pub fn inspect(mut self, on: bool) -> Self {
        self.config.inspect = on;
        self
    }

    /// Supplies a transport in place of the built-in HTTPS client.
    pub fn transport(mut self, transport: impl Transport) -> Self {
        self.transport = Some(Box::new(transport));
        self
    }

    /// Adjusts limits and intervals.
    pub fn configure(mut self, edit: impl FnOnce(&mut Config)) -> Self {
        edit(&mut self.config);
        self
    }

    /// Starts the SDK. Never fails: if its folder cannot be used it returns a tracer that records nothing.
    ///
    /// Keep the returned value alive for the life of the process; dropping the last clone ends the
    /// session and sends what is waiting.
    pub fn init(self) -> Tracer {
        if cfg!(target_arch = "wasm32") || self.config.data_dir.as_os_str().is_empty() {
            return Tracer::disabled();
        }
        let mut config = self.config;
        if config.process == ProcessKind::Cli {
            // A command-line process has no main loop to hang and no frames: no watchdog thread, no
            // performance windows, no CPU and memory sampling.
            config.signals.retain(|signal| !CLI_OFF.contains(signal));
        }
        let pipeline = Pipeline::start(&config, self.transport);
        let sink: Arc<dyn Sink> = pipeline.clone();
        let signals = if config.courier {
            // The pipeline keeps the signal list, to report what earlier runs left; nothing here records.
            Signals::start(&Config { signals: Vec::new(), logger: LoggerMode::None, ..config.clone() }, sink)
        } else {
            Signals::start(&config, sink)
        };
        pipeline.attach(signals.clone());
        Tracer { inner: Some(Arc::new(Inner { pipeline, signals })) }
    }
}

/// The built-in signals a [`ProcessKind::Cli`] process never starts, whatever the builder says.
pub const CLI_OFF: [Signal; 3] = [Signal::Hang, Signal::Perf, Signal::Sys];

struct Inner {
    pipeline: Arc<Pipeline>,
    signals: Arc<Signals>,
}

impl Inner {
    /// Ends the session once; later calls (and the drop) only report how the first one went.
    fn shutdown(&self, wait: Option<Duration>) -> bool {
        self.signals.shutdown();
        self.pipeline.shutdown_with(wait)
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown(None);
    }
}

/// Handle to the running SDK. Cheap to clone and safe to share across threads.
///
/// Every method is a no-op on a [disabled](Tracer::disabled) tracer, so hosts can pass one through
/// their layers unconditionally.
#[derive(Clone)]
pub struct Tracer {
    inner: Option<Arc<Inner>>,
}

impl Tracer {
    pub fn builder(app_key: &str, app_name: &str, app_version: &str) -> Builder {
        Builder { config: Config::new(app_key, app_name, app_version), transport: None }
    }

    /// A tracer that records nothing. For tests and for builds without telemetry.
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    // ---- usage ----

    /// Records a declared event, if the host listens to it and the user allows usage data.
    pub fn track<E: Event>(&self, event: E) {
        self.track_with(event, &[]);
    }

    /// Records a declared event with a few typed properties.
    pub fn track_with<E: Event>(&self, event: E, props: &[(&'static str, Prop)]) {
        if let Some(inner) = &self.inner {
            inner.pipeline.track(event.name(), event.category().name(), false, props);
        }
    }

    /// Records an id that is not in the catalog, under a declared category. For hosts that dispatch
    /// commands by string id. The id must be a fixed program string, never user content.
    pub fn track_id<C: EventCategory>(&self, category: C, id: &'static str) {
        if let Some(inner) = &self.inner {
            inner.pipeline.track(id, category.name(), true, &[]);
        }
    }

    // ---- session activity ----

    /// Tells the SDK whether the app window has focus.
    pub fn set_focused(&self, focused: bool) {
        if let Some(inner) = &self.inner {
            inner.pipeline.set_focused(focused);
        }
    }

    /// Tells the SDK the user just did something. Call on input; it is one atomic store.
    pub fn note_input(&self) {
        if let Some(inner) = &self.inner {
            inner.pipeline.note_input();
        }
    }

    // ---- performance ----

    /// Records how long one frame took to produce.
    pub fn frame(&self, duration: Duration) {
        if let Some(inner) = &self.inner {
            inner.signals.frame(duration);
        }
    }

    /// Marks the first frame on screen, which closes the startup-time measurement.
    pub fn first_frame(&self) {
        if let Some(inner) = &self.inner {
            inner.signals.first_frame();
        }
    }

    /// Registers a counter that is safe to bump from a realtime thread.
    pub fn counter(&self, name: &'static str) -> Counter {
        match &self.inner {
            Some(inner) => inner.signals.counter(name),
            None => Counter::detached(),
        }
    }

    /// Registers a gauge that is safe to set from a realtime thread.
    pub fn gauge(&self, name: &'static str) -> Gauge {
        match &self.inner {
            Some(inner) => inner.signals.gauge(name),
            None => Gauge::detached(),
        }
    }

    // ---- hangs ----

    /// Marks the start of one turn of the host's main loop: handling an event, running an update,
    /// painting a frame. A turn left open for longer than `hang_threshold` is reported as a hang, once.
    /// The time between turns, when the loop waits for something to do, is never a hang.
    ///
    /// This is the preferred way to drive hang detection, and the only one that works for a loop that
    /// runs only on input or that a GUI framework owns. It is one atomic update. Calling it again while
    /// a turn is open keeps the original start. Once a host has begun a turn, [`Tracer::heartbeat`] no
    /// longer counts.
    pub fn begin_turn(&self) {
        if let Some(inner) = &self.inner {
            inner.signals.begin_turn();
        }
    }

    /// Marks the end of the turn [`Tracer::begin_turn`] opened. One atomic update. Without an open turn
    /// it does nothing.
    pub fn end_turn(&self) {
        if let Some(inner) = &self.inner {
            inner.signals.end_turn();
        }
    }

    /// Begins a turn and ends it when the returned guard is dropped, including on an early return or a
    /// panic that unwinds.
    pub fn turn(&self) -> TurnGuard {
        self.begin_turn();
        TurnGuard { tracer: self.clone() }
    }

    /// Tells the hang watchdog the main loop is alive, for a loop that runs continuously: call once per
    /// iteration. A loop that waits for input between iterations must use [`Tracer::begin_turn`] and
    /// [`Tracer::end_turn`] instead, or every wait longer than `hang_threshold` would look like a hang.
    pub fn heartbeat(&self) {
        if let Some(inner) = &self.inner {
            inner.signals.heartbeat();
        }
    }

    /// Suspends hang detection, for example while a native modal dialog blocks the main loop.
    pub fn pause_hang_detection(&self) {
        if let Some(inner) = &self.inner {
            inner.signals.pause_hang_detection();
        }
    }

    pub fn resume_hang_detection(&self) {
        if let Some(inner) = &self.inner {
            inner.signals.resume_hang_detection();
        }
    }

    // ---- errors ----

    /// Reports an error the host handled and survived. `context` is a fixed label such as a command id.
    pub fn report_handled(&self, error: &dyn std::fmt::Display, context: Option<&'static str>) {
        if let Some(inner) = &self.inner {
            inner.signals.report_handled(&error.to_string(), context);
        }
    }

    /// Reports how a child process ended. Recorded only when it did not exit successfully.
    pub fn report_child_exit(&self, program: &'static str, status: ExitStatus) {
        if let Some(inner) = &self.inner {
            inner.signals.report_child_exit(program, status);
        }
    }

    // ---- logs ----

    /// A logger that keeps recent lines as crash breadcrumbs and forwards every record to `inner`.
    /// For hosts that install their own global logger: wrap it with this before installing.
    pub fn logger(&self, inner_logger: Option<Box<dyn log::Log>>) -> Box<dyn log::Log> {
        match (&self.inner, inner_logger) {
            (Some(inner), wrapped) => inner.signals.logger(wrapped),
            (None, Some(wrapped)) => wrapped,
            (None, None) => Box::new(Silent),
        }
    }

    // ---- consent ----

    pub fn consent(&self, category: ConsentCategory) -> bool {
        self.inner.as_ref().is_some_and(|inner| inner.pipeline.consent(category))
    }

    /// Switches a category on or off for this user. Saved, and applied at once.
    pub fn set_consent(&self, category: ConsentCategory, allowed: bool) {
        if let Some(inner) = &self.inner {
            inner.pipeline.set_consent(category, allowed);
        }
    }

    /// True until the host has shown its first-run notice and called [`Tracer::notice_shown`].
    pub fn notice_pending(&self) -> bool {
        self.inner.as_ref().is_some_and(|inner| inner.pipeline.notice_pending())
    }

    pub fn notice_shown(&self) {
        if let Some(inner) = &self.inner {
            inner.pipeline.notice_shown();
        }
    }

    /// Deletes everything stored locally, asks the server to delete this install's data, and issues a
    /// fresh install id.
    pub fn forget(&self) {
        if let Some(inner) = &self.inner {
            inner.pipeline.forget();
        }
    }

    // ---- identity ----

    pub fn install_id(&self) -> String {
        self.inner.as_ref().map(|inner| inner.pipeline.install_id()).unwrap_or_default()
    }

    pub fn session_id(&self) -> String {
        self.inner.as_ref().map(|inner| inner.pipeline.session_id()).unwrap_or_default()
    }

    // ---- delivery ----

    /// Sends what is waiting and returns when done or when `timeout` passes. For short-lived processes.
    /// True when nothing is left to send.
    pub fn flush_blocking(&self, timeout: Duration) -> bool {
        self.inner.as_ref().is_none_or(|inner| inner.pipeline.flush_blocking(timeout))
    }

    /// Ends the session now, as dropping the last clone would: the open performance window is closed,
    /// the session's end is written to disk as `exited`, and the worker gets `shutdown_timeout` for one
    /// last send. For processes that end without dropping their handles, such as through
    /// `std::process::exit`. Idempotent: later calls, and the final drop, do nothing more. After it,
    /// every call on any clone records nothing. True when the last send left nothing that could be sent.
    pub fn shutdown(&self) -> bool {
        self.inner.as_ref().is_none_or(|inner| inner.shutdown(None))
    }

    /// [`Tracer::shutdown`], waiting at most `wait` for the last send instead of `shutdown_timeout`.
    /// The session's end is on disk before the wait starts, so a short `wait` loses nothing: what
    /// could not be sent stays in the spool for the next process of the same host.
    pub fn shutdown_within(&self, wait: Duration) -> bool {
        self.inner.as_ref().is_none_or(|inner| inner.shutdown(Some(wait)))
    }

    /// Writes a human-readable report (session, device, recent logs, last crash) the user can attach to
    /// a bug report. Works without a server.
    pub fn write_report(&self, path: &Path) -> std::io::Result<()> {
        match &self.inner {
            Some(inner) => inner.signals.write_report(path),
            None => Ok(()),
        }
    }
}

/// One turn of the host's main loop, from [`Tracer::turn`]. The turn ends when this is dropped.
#[must_use = "the turn ends as soon as the guard is dropped"]
pub struct TurnGuard {
    tracer: Tracer,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.tracer.end_turn();
    }
}

struct Silent;

impl log::Log for Silent {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        false
    }

    fn log(&self, _: &log::Record<'_>) {}

    fn flush(&self) {}
}
