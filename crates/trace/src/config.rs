//! Everything the host sets once at startup. Built through [`crate::Builder`].

use std::path::PathBuf;
use std::time::Duration;

use ashlar_trace_proto::{Catalog, Gpu, ProcessKind, Signal};

/// How the SDK captures log lines as crash breadcrumbs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LoggerMode {
    /// Do not touch the global logger. The host can still feed lines through [`crate::Tracer::logger`].
    #[default]
    None,
    /// Install the SDK's logger as the global `log` logger. Use when the host has none of its own.
    Install,
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Write key for this app, issued by the server.
    pub app_key: String,
    pub app_name: String,
    pub app_version: String,
    pub build: Option<String>,
    pub process: ProcessKind,
    /// Server base URL. `None` records locally and sends nothing.
    pub endpoint: Option<String>,
    /// The host's data directory. The SDK keeps its files in a `tracer` folder inside it.
    pub data_dir: PathBuf,
    /// Built-in signals the host enabled.
    pub signals: Vec<Signal>,
    /// Every event the host declared.
    pub catalog: Catalog,
    /// Category names the host listens to.
    pub listen_categories: Vec<String>,
    /// Individual event names the host listens to, on top of the categories.
    pub listen_events: Vec<String>,
    pub gpu: Option<Gpu>,
    pub logger: LoggerMode,
    /// Mirror everything that would be sent into `outbox.jsonl` for the user to read.
    pub inspect: bool,
    /// Send before the host has called [`crate::Tracer::notice_shown`]. Off by default: data is recorded
    /// and kept on disk, but nothing leaves the machine until the user has seen the notice.
    pub send_before_notice: bool,
    /// Honour `DO_NOT_TRACK` in the environment (on by default). `TRACER_DISABLED` is always honoured.
    pub honor_do_not_track: bool,
    /// Every category allowed and the notice counted as shown from `init`, whatever a saved choice says,
    /// and `set_consent` / `notice_shown` do nothing. For hosts whose telemetry is not optional.
    pub force_consent: bool,
    /// Only deliver: send what other processes of the host left in the spool (reporting the ones that
    /// ended uncleanly, as any process does), and record nothing of this process's own: no session, no
    /// marker, no crash hook, no watchdog, no logger. See [`crate::Builder::courier`].
    pub courier: bool,

    /// Items the in-memory queue holds before dropping.
    pub queue_capacity: usize,
    /// Send after this many items are waiting.
    pub batch_items: usize,
    /// Send at least this often while items are waiting.
    pub flush_interval: Duration,
    /// How long shutdown waits for a final send.
    pub shutdown_timeout: Duration,
    /// How often the worker wakes to account session time, tick the signal modules and check whether a
    /// send is due. Every other interval is rounded up to a multiple of this.
    pub tick_interval: Duration,
    /// Shortest wait before retrying a failed send. Doubles on each failure, with jitter.
    pub retry_min: Duration,
    /// Longest wait between retries of a failed send.
    pub retry_max: Duration,
    /// Largest size of the on-disk spool; the oldest data is dropped first.
    pub spool_max_bytes: u64,
    /// Spooled data older than this is dropped.
    pub spool_max_age: Duration,
    /// Time between session heartbeats.
    pub heartbeat_interval: Duration,
    /// No input for this long counts as idle.
    pub idle_after: Duration,
    /// Length of one performance window.
    pub perf_window: Duration,
    /// Time between CPU and memory samples.
    pub sys_interval: Duration,
    /// The main loop is hung when it has not beaten for this long.
    pub hang_threshold: Duration,
    /// Recent log lines kept as breadcrumbs.
    pub breadcrumb_capacity: usize,
    /// Lowest level kept as a breadcrumb.
    pub breadcrumb_level: log::Level,
}

impl Config {
    pub(crate) fn new(app_key: &str, app_name: &str, app_version: &str) -> Self {
        Self {
            app_key: app_key.to_owned(),
            app_name: app_name.to_owned(),
            app_version: app_version.to_owned(),
            build: None,
            process: ProcessKind::Gui,
            endpoint: None,
            data_dir: PathBuf::new(),
            signals: Signal::ALL.to_vec(),
            catalog: Catalog::default(),
            listen_categories: Vec::new(),
            listen_events: Vec::new(),
            gpu: None,
            logger: LoggerMode::None,
            inspect: false,
            send_before_notice: false,
            honor_do_not_track: true,
            force_consent: false,
            courier: false,
            queue_capacity: 1024,
            batch_items: 50,
            flush_interval: Duration::from_secs(60),
            shutdown_timeout: Duration::from_secs(3),
            tick_interval: Duration::from_secs(1),
            retry_min: Duration::from_secs(5),
            retry_max: Duration::from_secs(15 * 60),
            spool_max_bytes: 8 * 1024 * 1024,
            spool_max_age: Duration::from_secs(30 * 24 * 60 * 60),
            heartbeat_interval: Duration::from_secs(60),
            idle_after: Duration::from_secs(120),
            perf_window: Duration::from_secs(60),
            sys_interval: Duration::from_secs(10),
            hang_threshold: Duration::from_secs(2),
            breadcrumb_capacity: 100,
            breadcrumb_level: log::Level::Info,
        }
    }
}
