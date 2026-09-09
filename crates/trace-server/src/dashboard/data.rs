//! Page data: query parameters and the loaders that turn store rows into what each page shows. Loaders are
//! plain functions over `&dyn Store`; handlers run them inside one `spawn_blocking`.

use std::collections::HashMap;

use tracer_proto::{FRAME_BUCKET_BOUNDS_US, FrameStats};

use super::metrics::{self, CustomMetrics, PerfSummary, SessionSummary};
use crate::ingest::DAY_MS;
use crate::store::{
    AppGlance, AppRecord, Breakdown, CatalogEntry, CrashBrief, CrashRow, DropRow, EventRow, IssueFilter, IssueRow, IssueSort, KeyRecord, Store,
    StoreResult, VersionInfo,
};

pub type Params = HashMap<String, String>;

/// A secondary query must not take a whole page down: on failure it is logged and the section is empty.
fn soft<T: Default>(what: &str, result: StoreResult<T>) -> T {
    result.unwrap_or_else(|error| {
        log::error!("could not load {what}: {error}");
        T::default()
    })
}

pub const RANGES: [i64; 3] = [7, 30, 90];
pub const DEFAULT_DAYS: i64 = 30;
pub const KINDS: [&str; 6] = ["panic", "handled", "hang", "unclean_exit", "child_exit", "other"];
pub const STATUSES: [&str; 3] = ["open", "resolved", "ignored"];
const MAX_VERSION_LEN: usize = 64;
const ISSUE_LIMIT: i64 = 200;
const CUSTOM_ROWS: i64 = 2_000;

/// The date range and version filter, from plain GET parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filters {
    pub days: i64,
    pub version: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
}

impl Filters {
    pub fn from_params(params: &Params) -> Self {
        let days = params.get("days").and_then(|text| text.parse::<i64>().ok()).filter(|days| RANGES.contains(days)).unwrap_or(DEFAULT_DAYS);
        let version = params.get("version").map(|text| text.trim()).filter(|text| !text.is_empty() && text.chars().count() <= MAX_VERSION_LEN);
        let pick = |name: &str, allowed: &[&str]| params.get(name).filter(|text| allowed.contains(&text.as_str())).cloned();
        Self { days, version: version.map(str::to_string), kind: pick("kind", &KINDS), status: pick("status", &STATUSES) }
    }

    /// First and last day of the range, ending today.
    pub fn range(&self, now: i64) -> (i64, i64) {
        let to = now.div_euclid(DAY_MS);
        (to - self.days + 1, to)
    }

    pub fn version(&self) -> Option<&str> {
        self.version.as_deref()
    }
}

// ---------------------------------------------------------------------------------------------------------

pub struct Overview {
    pub from: i64,
    pub to: i64,
    pub summary: SessionSummary,
    pub opens: Vec<f64>,
    pub sessions: Vec<f64>,
    pub dau: Vec<f64>,
    pub dau_today: i64,
    pub wau: i64,
    pub mau: i64,
    pub opens_today: i64,
    pub issues: Vec<IssueRow>,
    pub versions: Vec<VersionInfo>,
}

pub fn load_overview(store: &dyn Store, app: &AppRecord, f: &Filters, now: i64) -> StoreResult<Overview> {
    let (from, to) = f.range(now);
    let v = f.version();
    let totals = store.day_totals(app.id, from, to, v)?;
    let dau = store.active_installs_by_day(app.id, from, to, v)?;
    let issues = store.list_issues(
        app.id,
        &IssueFilter { status: Some("open"), version: v, since_ms: 0, sort: IssueSort::Recent, ..IssueFilter::default() },
        5,
    )?;
    Ok(Overview {
        from,
        to,
        summary: SessionSummary::from_days(&totals, &dau),
        opens: metrics::fill_days(from, to, totals.iter().map(|d| (d.day, d.opens as f64))),
        sessions: metrics::fill_days(from, to, totals.iter().map(|d| (d.day, d.sessions as f64))),
        dau: metrics::fill_days(from, to, dau.iter().map(|(d, n)| (*d, *n as f64))),
        dau_today: store.active_installs_between(app.id, to, to, v)?,
        wau: store.active_installs_between(app.id, to - 6, to, v)?,
        mau: store.active_installs_between(app.id, to - 29, to, v)?,
        opens_today: totals.iter().find(|d| d.day == to).map_or(0, |d| d.opens),
        issues,
        versions: store.versions(app.id)?,
    })
}

// ---------------------------------------------------------------------------------------------------------

pub struct CrashList {
    pub issues: Vec<IssueRow>,
    pub versions: Vec<VersionInfo>,
}

pub fn load_crashes(store: &dyn Store, app: &AppRecord, f: &Filters, now: i64) -> StoreResult<CrashList> {
    let (from, _) = f.range(now);
    let filter =
        IssueFilter { kind: f.kind.as_deref(), version: f.version(), status: f.status.as_deref(), since_ms: from * DAY_MS, sort: IssueSort::Recent };
    Ok(CrashList { issues: store.list_issues(app.id, &filter, ISSUE_LIMIT)?, versions: store.versions(app.id)? })
}

pub struct IssueDetail {
    pub issue: IssueRow,
    pub crash: Option<CrashRow>,
    pub recent: Vec<CrashBrief>,
    pub breakdown: Breakdown,
}

pub fn load_issue(store: &dyn Store, app: &AppRecord, id: i64) -> StoreResult<Option<IssueDetail>> {
    let Some(issue) = store.get_issue(app.id, id)? else { return Ok(None) };
    Ok(Some(IssueDetail {
        crash: store.representative_crash(app.id, id)?,
        recent: store.recent_crashes(app.id, id, 25)?,
        breakdown: store.issue_breakdown(app.id, id)?,
        issue,
    }))
}

// ---------------------------------------------------------------------------------------------------------

pub struct PerfPage {
    pub from: i64,
    pub to: i64,
    pub daily: Vec<PerfSummary>,
    pub overall: PerfSummary,
    pub by_version: Vec<(String, PerfSummary)>,
    pub by_os: Vec<(String, PerfSummary)>,
    /// Frame counts per histogram bucket: `(label, frames)`.
    pub histogram: Vec<(String, f64)>,
    pub custom: CustomMetrics,
    pub versions: Vec<VersionInfo>,
}

/// Labels for the frame-time histogram buckets: `<= 1 ms` ... `> 5000 ms`.
pub fn bucket_labels() -> Vec<String> {
    let ms = |us: u32| {
        let value = f64::from(us) / 1000.0;
        if value.fract() == 0.0 { format!("{value:.0}") } else { format!("{value:.1}") }
    };
    let mut labels: Vec<String> = FRAME_BUCKET_BOUNDS_US.iter().map(|bound| format!("\u{2264}{}", ms(*bound))).collect();
    labels.push(format!(">{}", ms(FRAME_BUCKET_BOUNDS_US[FRAME_BUCKET_BOUNDS_US.len() - 1])));
    labels
}

pub fn load_performance(store: &dyn Store, app: &AppRecord, f: &Filters, now: i64) -> StoreResult<PerfPage> {
    let (from, to) = f.range(now);
    let rows = store.perf_rows(app.id, from, to, f.version())?;
    let (acc, overall) = metrics::summarize_perf(rows.iter());
    let frames: &FrameStats = &acc.frames;
    let histogram = bucket_labels().into_iter().zip(frames.buckets.iter()).map(|(label, count)| (label, *count as f64)).collect();
    let custom = metrics::custom_metrics(&store.recent_custom_metrics(app.id, from, f.version(), CUSTOM_ROWS)?);
    Ok(PerfPage {
        from,
        to,
        daily: metrics::perf_by_day(&rows, from, to),
        overall,
        by_version: metrics::perf_grouped(&rows, |r| r.version.as_str()),
        by_os: metrics::perf_grouped(&rows, |r| r.os_name.as_str()),
        histogram,
        custom,
        versions: store.versions(app.id)?,
    })
}

// ---------------------------------------------------------------------------------------------------------

pub struct UsagePage {
    pub from: i64,
    pub to: i64,
    pub summary: SessionSummary,
    pub dau: Vec<f64>,
    pub sessions: Vec<f64>,
    pub events: Vec<EventRow>,
    /// Events the catalog does not list. Client-supplied, so shown apart from the declared ones.
    pub undeclared: Vec<EventRow>,
    pub never_fired: Vec<CatalogEntry>,
    pub versions: Vec<VersionInfo>,
}

pub fn load_usage(store: &dyn Store, app: &AppRecord, f: &Filters, now: i64) -> StoreResult<UsagePage> {
    let (from, to) = f.range(now);
    let v = f.version();
    let totals = store.day_totals(app.id, from, to, v)?;
    let dau = store.active_installs_by_day(app.id, from, to, v)?;
    Ok(UsagePage {
        from,
        to,
        summary: SessionSummary::from_days(&totals, &dau),
        dau: metrics::fill_days(from, to, dau.iter().map(|(d, n)| (*d, *n as f64))),
        sessions: metrics::fill_days(from, to, totals.iter().map(|d| (d.day, d.sessions as f64))),
        events: store.events_ranked(app.id, from, to, v, true)?,
        undeclared: soft("undeclared events", store.events_ranked(app.id, from, to, v, false)),
        never_fired: soft("never-fired events", store.never_fired(app.id, v)),
        versions: store.versions(app.id)?,
    })
}

// ---------------------------------------------------------------------------------------------------------

pub struct SettingsPage {
    pub keys: Vec<KeyRecord>,
    pub drops: Vec<DropRow>,
    pub glance: AppGlance,
    pub from: i64,
    pub to: i64,
}

pub fn load_settings(store: &dyn Store, app: &AppRecord, f: &Filters, now: i64) -> StoreResult<SettingsPage> {
    let (from, to) = f.range(now);
    Ok(SettingsPage {
        keys: store.list_keys(app.id)?,
        drops: soft("drop counts", store.drops(app.id, from, to)),
        glance: store.app_glance(app.id, to - 6, to)?,
        from,
        to,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Params {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn filters_accept_only_known_values() {
        let f = Filters::from_params(&params(&[("days", "7"), ("version", " 1.2.0 "), ("kind", "panic"), ("status", "open")]));
        assert_eq!((f.days, f.version.as_deref(), f.kind.as_deref(), f.status.as_deref()), (7, Some("1.2.0"), Some("panic"), Some("open")));
        let f = Filters::from_params(&params(&[("days", "5"), ("version", ""), ("kind", "x'; DROP"), ("status", "OPEN")]));
        assert_eq!((f.days, f.version, f.kind, f.status), (30, None, None, None));
        assert_eq!(Filters::from_params(&params(&[("version", &"v".repeat(65))])).version, None);
        assert_eq!(Filters::from_params(&params(&[("days", "-1")])).days, 30);
    }

    #[test]
    fn range_ends_today_and_spans_the_requested_days() {
        let f = Filters::from_params(&params(&[("days", "7")]));
        assert_eq!(f.range(10 * DAY_MS + 5), (4, 10));
    }

    #[test]
    fn bucket_labels_match_the_histogram_size() {
        let labels = bucket_labels();
        assert_eq!(labels.len(), tracer_proto::FRAME_BUCKETS);
        assert_eq!(labels[0], "\u{2264}1");
        assert_eq!(labels[4], "\u{2264}8.3");
        assert_eq!(labels[15], ">5000");
    }
}
