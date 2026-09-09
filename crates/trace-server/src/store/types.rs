//! Row types returned by the dashboard's read queries. Plain data, no behaviour.

use serde::Serialize;
use tracer_proto::FrameStats;

/// A logged-in dashboard user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSession {
    pub admin_id: i64,
    pub username: String,
}

/// One day of session rollups, summed over versions and operating systems.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DayTotals {
    pub day: i64,
    pub sessions: i64,
    pub opens: i64,
    /// Installs seen in sessions that day, summed over version and OS groups.
    pub installs: i64,
    pub exited: i64,
    pub crashed: i64,
    pub abnormal: i64,
    pub errored: i64,
    pub crashed_installs: i64,
    pub duration_s: i64,
    pub focused_s: i64,
    pub active_s: i64,
    pub idle_s: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    pub version: String,
    pub first_day: i64,
    pub last_day: i64,
    pub sessions: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IssueSort {
    #[default]
    Recent,
    Count,
}

/// Filters for the issue list. `None` means no filter.
#[derive(Debug, Clone, Copy, Default)]
pub struct IssueFilter<'a> {
    pub kind: Option<&'a str>,
    pub version: Option<&'a str>,
    pub status: Option<&'a str>,
    /// Only issues last seen at or after this time (ms).
    pub since_ms: i64,
    pub sort: IssueSort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IssueRow {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub first_version: String,
    pub last_version: String,
    pub count: i64,
    pub status: String,
    /// Distinct installs that hit this issue.
    pub installs: i64,
    pub versions: Vec<String>,
}

/// One stored crash, with every detail column. Text fields hold client-supplied data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrashRow {
    pub id: i64,
    pub ts: i64,
    pub install_id: String,
    pub session_id: String,
    pub version: String,
    pub build: Option<String>,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub arch: Option<String>,
    pub kind: String,
    pub fatal: bool,
    pub message: Option<String>,
    pub location: Option<String>,
    pub thread: Option<String>,
    pub backtrace: Option<String>,
    pub blocked_ms: Option<i64>,
    pub exit_code: Option<i64>,
    pub signal: Option<i64>,
    pub context: Option<String>,
    /// JSON array of breadcrumbs.
    pub breadcrumbs: Option<String>,
    /// JSON of the last perf window.
    pub perf: Option<String>,
    /// JSON of the device.
    pub device: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashBrief {
    pub id: i64,
    pub ts: i64,
    pub version: String,
    pub os_name: Option<String>,
    pub os_version: Option<String>,
    pub install_id: String,
    pub fatal: bool,
}

/// Crash counts for one issue by version and by operating system, largest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Breakdown {
    pub versions: Vec<(String, i64)>,
    pub os: Vec<(String, i64)>,
}

/// One `daily_perf` row.
#[derive(Debug, Clone, Default)]
pub struct PerfRow {
    pub day: i64,
    pub version: String,
    pub os_name: String,
    pub frames: FrameStats,
    pub cpu_sum: f64,
    pub cpu_n: i64,
    pub rss_sum: i64,
    pub rss_n: i64,
    pub rss_peak_mb: i64,
    pub startup_sum: i64,
    pub startup_n: i64,
}

/// Host-defined counters and gauges of one raw perf row, as stored JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomRow {
    pub counters: Option<String>,
    pub gauges: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventRow {
    pub name: String,
    pub category: String,
    pub count: i64,
    /// Sum over days of the installs that fired the event: install-days.
    pub installs: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogEntry {
    pub name: String,
    pub category: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DropRow {
    pub reason: String,
    pub count: i64,
}

/// A line of numbers shown beside an app on the picker.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppGlance {
    pub opens: i64,
    pub open_issues: i64,
}
