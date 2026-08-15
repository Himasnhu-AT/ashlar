//! Wire types shared by the Tracer SDK and server.
//!
//! The format is a versioned JSON envelope (see `docs/wire-format.md`). Rules that keep old clients and new
//! servers compatible in both directions:
//!
//! - Fields are only ever added, never renamed or repurposed. New fields are optional.
//! - Readers ignore fields they do not know and map item types they do not know to [`ItemBody::Unknown`].
//! - A breaking change bumps [`SCHEMA_VERSION`].
//!
//! This crate does no I/O and builds for `wasm32-unknown-unknown`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Version of the envelope format this crate reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// Largest request body (after decompression) the server accepts on the ingest route.
pub const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;

/// Longest event or category name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// Longest string property value, in bytes.
pub const MAX_PROP_STR_LEN: usize = 64;

/// Most properties on one event.
pub const MAX_PROPS: usize = 16;

/// Upper bounds, in microseconds, of the frame-time histogram buckets. A final overflow bucket follows,
/// so [`FrameStats::buckets`] has `FRAME_BUCKET_BOUNDS_US.len() + 1` entries.
pub const FRAME_BUCKET_BOUNDS_US: [u32; 15] =
    [1_000, 2_000, 4_000, 6_000, 8_333, 11_111, 16_667, 25_000, 33_333, 50_000, 100_000, 250_000, 700_000, 2_000_000, 5_000_000];

/// Number of buckets in [`FrameStats::buckets`].
pub const FRAME_BUCKETS: usize = FRAME_BUCKET_BOUNDS_US.len() + 1;

/// A frame slower than this many microseconds counts as slow.
pub const SLOW_FRAME_US: u32 = 16_667;

/// A frame slower than this many microseconds counts as frozen.
pub const FROZEN_FRAME_US: u32 = 700_000;

/// Route paths, relative to the server base URL.
pub mod routes {
    pub const INGEST: &str = "/v1/ingest";
    pub const FORGET: &str = "/v1/forget";
    pub const HEALTH: &str = "/v1/health";
}

/// Request header carrying the app write key.
pub const HEADER_APP_KEY: &str = "x-tracer-key";

/// Milliseconds since the Unix epoch, UTC.
pub type UnixMs = u64;

/// One batch of items from one session of one install.
///
/// All items in an envelope belong to the session named in the header. A client that has data from an
/// earlier session (for example a crash found on the next launch) sends it in a separate envelope that
/// carries the earlier session's header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub schema: u32,
    pub app: AppInfo,
    pub install_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Device>,
    /// Client clock when the request was sent. The server uses it to correct clock drift.
    pub sent_at: UnixMs,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    /// Build identifier such as a commit hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(default)]
    pub process: ProcessKind,
}

/// What kind of process produced the data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessKind {
    #[default]
    Gui,
    Cli,
    Service,
    Child,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub os_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    pub arch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_brand: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_mb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<Gpu>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Client-generated unique id. The server drops an item whose id it has already stored.
    pub id: String,
    /// Client clock when the item was recorded.
    pub ts: UnixMs,
    #[serde(flatten)]
    pub body: ItemBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemBody {
    SessionStart(SessionStart),
    SessionHeartbeat(SessionTotals),
    SessionEnd(SessionEnd),
    Event(Event),
    Perf(Perf),
    Crash(Crash),
    Drops(Drops),
    /// An item type this version does not know. Readers skip it.
    #[serde(other)]
    Unknown,
}

impl ItemBody {
    /// The `type` tag this body serializes with.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::SessionStart(_) => "session_start",
            Self::SessionHeartbeat(_) => "session_heartbeat",
            Self::SessionEnd(_) => "session_end",
            Self::Event(_) => "event",
            Self::Perf(_) => "perf",
            Self::Crash(_) => "crash",
            Self::Drops(_) => "drops",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStart {
    /// Hash of the declared catalog. Lets the server tell whether it already holds this catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_hash: Option<String>,
    /// The declared catalog. Sent until the server has acknowledged this hash for this app version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<Catalog>,
    /// Categories the host chose to listen to for this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listening: Vec<String>,
    /// Built-in signals enabled for this run, by [`Signal::as_str`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals: Vec<String>,
}

/// Every event the host declared at build time, whether or not it is listened to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub events: Vec<CatalogEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CatalogEvent {
    pub name: String,
    pub category: String,
}

/// Built-in signals a host can switch on or off at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Session,
    Usage,
    Crash,
    Hang,
    Perf,
    Sys,
    Logs,
}

impl Signal {
    pub const ALL: [Signal; 7] = [Signal::Session, Signal::Usage, Signal::Crash, Signal::Hang, Signal::Perf, Signal::Sys, Signal::Logs];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Usage => "usage",
            Self::Crash => "crash",
            Self::Hang => "hang",
            Self::Perf => "perf",
            Self::Sys => "sys",
            Self::Logs => "logs",
        }
    }

    /// The consent category an end user switches this signal off with.
    pub fn consent(self) -> ConsentCategory {
        match self {
            Self::Crash | Self::Hang | Self::Logs => ConsentCategory::Crash,
            Self::Perf | Self::Sys => ConsentCategory::Performance,
            Self::Session | Self::Usage => ConsentCategory::Usage,
        }
    }
}

/// What an end user can switch off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentCategory {
    Crash,
    Performance,
    Usage,
}

/// Running totals for a session, in whole seconds since it started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTotals {
    /// Heartbeat counter, starting at 1.
    #[serde(default)]
    pub seq: u32,
    pub duration_s: u64,
    /// Time the app window had focus.
    #[serde(default)]
    pub focused_s: u64,
    /// Focused time with recent user input.
    #[serde(default)]
    pub active_s: u64,
    /// Time without focus or without recent input.
    #[serde(default)]
    pub idle_s: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnd {
    pub status: SessionStatus,
    #[serde(flatten)]
    pub totals: SessionTotals,
    /// Handled errors reported during the session.
    #[serde(default)]
    pub errors: u32,
}

/// How a session ended. A terminal status is never changed once stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// Still running, as far as the server knows.
    Ok,
    /// Ended normally.
    Exited,
    /// Ended by a panic.
    Crashed,
    /// Ended without saying goodbye: killed, out of memory, native crash, power loss.
    Abnormal,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Exited => "exited",
            Self::Crashed => "crashed",
            Self::Abnormal => "abnormal",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Ok)
    }
}

/// One usage event.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub name: String,
    pub category: String,
    /// True for an id recorded through the string escape hatch, which is not in the catalog.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dynamic: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub props: BTreeMap<String, PropValue>,
}

/// A property value. Deliberately small: no nesting and no free-form text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

/// Performance over one reporting window.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Perf {
    /// Length of the window in seconds.
    pub window_s: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<FrameStats>,
    /// Process CPU use over the window, where 100 is one full core.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<f32>,
    /// Resident memory at the end of the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_mb: Option<u32>,
    /// Highest resident memory seen during the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss_peak_mb: Option<u32>,
    /// Time from process start to the first frame. Sent once per session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_ms: Option<u32>,
    /// Host-defined counters, as increments over the window.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub counters: BTreeMap<String, u64>,
    /// Host-defined gauges, as last value in the window.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub gauges: BTreeMap<String, f64>,
}

/// Frame times over one window, as a histogram that can be merged by adding buckets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameStats {
    pub count: u64,
    pub sum_us: u64,
    pub max_us: u32,
    /// Frames slower than [`SLOW_FRAME_US`].
    pub slow: u64,
    /// Frames slower than [`FROZEN_FRAME_US`].
    pub frozen: u64,
    /// Counts per bucket of [`FRAME_BUCKET_BOUNDS_US`], plus one overflow bucket.
    pub buckets: Vec<u64>,
}

impl FrameStats {
    pub fn empty() -> Self {
        Self { buckets: vec![0; FRAME_BUCKETS], ..Self::default() }
    }

    /// Index of the bucket a frame of `us` microseconds falls in.
    pub fn bucket_index(us: u32) -> usize {
        FRAME_BUCKET_BOUNDS_US.iter().position(|bound| us <= *bound).unwrap_or(FRAME_BUCKET_BOUNDS_US.len())
    }

    pub fn record(&mut self, us: u32) {
        if self.buckets.len() != FRAME_BUCKETS {
            self.buckets.resize(FRAME_BUCKETS, 0);
        }
        self.count += 1;
        self.sum_us += u64::from(us);
        self.max_us = self.max_us.max(us);
        if us > SLOW_FRAME_US {
            self.slow += 1;
        }
        if us > FROZEN_FRAME_US {
            self.frozen += 1;
        }
        if let Some(slot) = self.buckets.get_mut(Self::bucket_index(us)) {
            *slot += 1;
        }
    }

    pub fn merge(&mut self, other: &FrameStats) {
        if self.buckets.len() < FRAME_BUCKETS {
            self.buckets.resize(FRAME_BUCKETS, 0);
        }
        self.count += other.count;
        self.sum_us += other.sum_us;
        self.max_us = self.max_us.max(other.max_us);
        self.slow += other.slow;
        self.frozen += other.frozen;
        for (slot, add) in self.buckets.iter_mut().zip(&other.buckets) {
            *slot += *add;
        }
    }

    /// Estimated frame time at quantile `q` in `0.0..=1.0`, in microseconds: the upper bound of the bucket
    /// holding that quantile, capped at the largest frame seen. `None` when no frames were recorded.
    pub fn quantile_us(&self, q: f64) -> Option<u32> {
        if self.count == 0 {
            return None;
        }
        let rank = (q.clamp(0.0, 1.0) * self.count as f64).ceil().max(1.0) as u64;
        let mut seen = 0u64;
        for (index, bucket) in self.buckets.iter().enumerate() {
            seen += *bucket;
            if seen >= rank {
                let bound = FRAME_BUCKET_BOUNDS_US.get(index).copied().unwrap_or(self.max_us);
                return Some(bound.min(self.max_us));
            }
        }
        Some(self.max_us)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Crash {
    pub kind: CrashKind,
    /// True when the process could not continue.
    #[serde(default)]
    pub fatal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Source location, `file:line:column`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backtrace: Option<String>,
    /// How long the main loop was blocked, for a hang.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_ms: Option<u32>,
    /// Exit code of a child process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Signal that ended a child process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// Short host-supplied label, such as the name of the child program or the command that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Recent log lines, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub breadcrumbs: Vec<Breadcrumb>,
    /// The last performance window before the crash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<Perf>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashKind {
    /// An uncaught panic.
    #[default]
    Panic,
    /// An error or caught panic the host reported and survived.
    Handled,
    /// The main loop stopped responding.
    Hang,
    /// The previous run ended without a clean shutdown.
    UncleanExit,
    /// A child process ended abnormally.
    ChildExit,
    #[serde(other)]
    Other,
}

impl CrashKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Panic => "panic",
            Self::Handled => "handled",
            Self::Hang => "hang",
            Self::UncleanExit => "unclean_exit",
            Self::ChildExit => "child_exit",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breadcrumb {
    pub ts: UnixMs,
    /// Lower-case log level: `error`, `warn`, `info`, `debug`, `trace`.
    pub level: String,
    pub target: String,
    pub message: String,
}

/// What the client threw away, by reason, since its last report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drops {
    pub reasons: BTreeMap<String, u64>,
}

/// Reply to an ingest request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestResponse {
    /// Items stored.
    pub accepted: u32,
    /// Items skipped because their id was already stored.
    #[serde(default)]
    pub duplicates: u32,
    /// Items refused, for example an event name that is not in the catalog.
    #[serde(default)]
    pub rejected: u32,
    /// True when the server does not hold the catalog for this app version and wants it resent.
    #[serde(default)]
    pub need_catalog: bool,
}

/// Body of a request to delete everything stored for one install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetRequest {
    pub schema: u32,
    pub install_id: String,
}

/// Why a name was refused. Shared by the build-time catalog check and the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    /// Must start with a lower-case letter.
    BadStart,
    /// Only lower-case letters, digits, `_` and `.` are allowed.
    BadChar,
    /// Must not end with, or repeat, a separator.
    BadSeparator,
}

/// Checks an event or category name: lower-case `snake_case`, optionally dotted, at most
/// [`MAX_NAME_LEN`] bytes. `const` so the catalog macro can run it at build time.
pub const fn validate_name(name: &str) -> Result<(), NameError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return Err(NameError::Empty);
    }
    if bytes.len() > MAX_NAME_LEN {
        return Err(NameError::TooLong);
    }
    if !bytes[0].is_ascii_lowercase() {
        return Err(NameError::BadStart);
    }
    let mut index = 0;
    let mut previous_separator = false;
    while index < bytes.len() {
        let byte = bytes[index];
        let separator = byte == b'_' || byte == b'.';
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || separator) {
            return Err(NameError::BadChar);
        }
        if separator && previous_separator {
            return Err(NameError::BadSeparator);
        }
        previous_separator = separator;
        index += 1;
    }
    if previous_separator {
        return Err(NameError::BadSeparator);
    }
    Ok(())
}

/// Byte-wise string equality usable in `const` contexts.
pub const fn const_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut index = 0;
    while index < a.len() {
        if a[index] != b[index] {
            return false;
        }
        index += 1;
    }
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}
