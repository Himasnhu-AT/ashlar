//! Storage behind a trait, so a second backend can be added. All SQL lives in [`sqlite`].
//!
//! Methods are blocking; async callers use `spawn_blocking` (see [`crate::http::blocking`]).

pub mod read;
pub mod sqlite;
pub mod types;

use tracer_proto::IngestResponse;

use crate::ingest::Prepared;

pub use sqlite::SqliteStore;
pub use types::*;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A uniqueness rule was violated, for example a duplicate app slug.
    #[error("already exists: {0}")]
    Conflict(String),
    #[error("storage failure: {0}")]
    Backend(String),
    /// The write queue is full; the caller should answer 503 with `Retry-After` and let the client retry.
    #[error("the write queue is full")]
    Busy,
}

pub type StoreResult<T> = Result<T, StoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRecord {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRecord {
    pub key: String,
    pub app_id: i64,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

/// Rows removed by one retention run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PruneStats {
    pub events: u64,
    pub perf: u64,
    pub crashes: u64,
    pub sessions: u64,
    pub items_seen: u64,
    pub installs: u64,
    pub issues: u64,
}

pub trait Store: Send + Sync + 'static {
    /// Cheap liveness check for the health route.
    fn health(&self) -> StoreResult<()>;
    /// Size of the database in bytes, for the ingest size ceiling.
    fn size_bytes(&self) -> StoreResult<u64>;

    // ---- apps, keys, admins ----

    /// Creates an app together with its first write key, atomically.
    fn create_app(&self, slug: &str, name: &str, first_key: &str, now: i64) -> StoreResult<AppRecord>;
    fn list_apps(&self) -> StoreResult<Vec<AppRecord>>;
    fn find_app(&self, slug: &str) -> StoreResult<Option<AppRecord>>;
    fn create_key(&self, app_id: i64, key: &str, now: i64) -> StoreResult<()>;
    /// Returns false when no active key matched.
    fn revoke_key(&self, key: &str, now: i64) -> StoreResult<bool>;
    fn list_keys(&self, app_id: i64) -> StoreResult<Vec<KeyRecord>>;
    /// The app an active (not revoked) write key belongs to.
    fn lookup_key(&self, key: &str) -> StoreResult<Option<AppRecord>>;
    fn create_admin(&self, username: &str, password_hash: &str, now: i64) -> StoreResult<()>;
    /// The stored password hash for a dashboard login.
    fn admin_password_hash(&self, username: &str) -> StoreResult<Option<(i64, String)>>;

    // ---- ingest ----

    /// Stores a whole prepared envelope in one transaction and reports the counts.
    fn ingest(&self, app_id: i64, batch: &Prepared) -> StoreResult<IngestResponse>;
    /// Like [`Store::ingest`], taking the batch by value so a backend that queues writes (group commit) needs
    /// no copy. Fails with [`StoreError::Busy`] when the queue is full.
    fn ingest_owned(&self, app_id: i64, batch: Prepared) -> StoreResult<IngestResponse>;
    /// Deletes everything stored for one install of one app. Returns the number of rows removed.
    fn forget(&self, app_id: i64, install_id: &str) -> StoreResult<u64>;

    // ---- jobs ----

    /// Recomputes the `daily_*` rollups (except drops) for days `from_day..=to_day` (days since the epoch)
    /// by delete and insert, one app per transaction.
    fn rollup(&self, from_day: i64, to_day: i64) -> StoreResult<()>;
    /// Recomputes the rollups of the days that received data since they were last rolled up, oldest first,
    /// ignoring days before `oldest_day`. Returns how many (app, day) pairs were recomputed.
    fn rollup_dirty(&self, oldest_day: i64) -> StoreResult<u64>;
    /// Deletes raw rows older than `retention_days` whole days before `now`. Rollups are kept.
    fn prune(&self, now: i64, retention_days: u32) -> StoreResult<PruneStats>;
    /// Marks sessions still `ok` whose last item is older than `older_than` as `abnormal`.
    fn close_stale_sessions(&self, older_than: i64) -> StoreResult<u64>;

    // ---- dashboard sessions ----

    /// Stores a login session, identified by the hash of its cookie token.
    fn admin_session_create(&self, token_hash: &str, admin_id: i64, now: i64, expires_at: i64) -> StoreResult<()>;
    /// The user a live (unexpired) session belongs to.
    fn admin_session_lookup(&self, token_hash: &str, now: i64) -> StoreResult<Option<AdminSession>>;
    fn admin_session_delete(&self, token_hash: &str) -> StoreResult<()>;
    /// Deletes expired sessions. Returns how many.
    fn admin_session_purge(&self, now: i64) -> StoreResult<u64>;
    fn admin_count(&self) -> StoreResult<i64>;

    // ---- dashboard reads (days are whole days since the epoch, ranges inclusive) ----

    fn app_glance(&self, app_id: i64, from_day: i64, to_day: i64) -> StoreResult<AppGlance>;
    /// Versions seen in the session rollups, oldest first.
    fn versions(&self, app_id: i64) -> StoreResult<Vec<VersionInfo>>;
    /// Session rollups per day, summed over versions and operating systems (or one version).
    fn day_totals(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>) -> StoreResult<Vec<DayTotals>>;
    /// Distinct active installs per day, as `(day, installs)`.
    fn active_installs_by_day(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>) -> StoreResult<Vec<(i64, i64)>>;
    /// Distinct active installs over a whole range.
    fn active_installs_between(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>) -> StoreResult<i64>;
    fn list_issues(&self, app_id: i64, filter: &IssueFilter<'_>, limit: i64) -> StoreResult<Vec<IssueRow>>;
    fn get_issue(&self, app_id: i64, issue_id: i64) -> StoreResult<Option<IssueRow>>;
    fn representative_crash(&self, app_id: i64, issue_id: i64) -> StoreResult<Option<CrashRow>>;
    fn recent_crashes(&self, app_id: i64, issue_id: i64, limit: i64) -> StoreResult<Vec<CrashBrief>>;
    fn issue_breakdown(&self, app_id: i64, issue_id: i64) -> StoreResult<Breakdown>;
    /// Sets an issue to `open`, `resolved` or `ignored`. Returns false when the issue is not in this app.
    fn set_issue_status(&self, app_id: i64, issue_id: i64, status: &str) -> StoreResult<bool>;
    fn perf_rows(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>) -> StoreResult<Vec<PerfRow>>;
    /// Counters and gauges from recent raw perf rows.
    fn recent_custom_metrics(&self, app_id: i64, since_day: i64, version: Option<&str>, limit: i64) -> StoreResult<Vec<CustomRow>>;
    /// Events by use. `declared` picks the events a catalog lists (true) or the ones it does not (false).
    fn events_ranked(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>, declared: bool) -> StoreResult<Vec<EventRow>>;
    /// Declared events that have never been fired, from the catalog most installs of `version` reported (or,
    /// without a version, of each version). A catalog nobody else reported cannot add to the list.
    fn never_fired(&self, app_id: i64, version: Option<&str>) -> StoreResult<Vec<CatalogEntry>>;
    fn drops(&self, app_id: i64, from_day: i64, to_day: i64) -> StoreResult<Vec<DropRow>>;
}
