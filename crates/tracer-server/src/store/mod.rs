//! Storage behind a trait, so a second backend can be added. All SQL lives in [`sqlite`].
//!
//! Methods are blocking; async callers use `spawn_blocking` (see [`crate::http::blocking`]).

pub mod sqlite;

use tracer_proto::IngestResponse;

use crate::ingest::Prepared;

pub use sqlite::SqliteStore;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A uniqueness rule was violated, for example a duplicate app slug.
    #[error("already exists: {0}")]
    Conflict(String),
    #[error("storage failure: {0}")]
    Backend(String),
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
    /// Deletes everything stored for one install of one app. Returns the number of rows removed.
    fn forget(&self, app_id: i64, install_id: &str) -> StoreResult<u64>;

    // ---- jobs ----

    /// Recomputes the `daily_*` rollups (except drops) for days `from_day..=to_day` (days since the epoch)
    /// by delete and insert inside one transaction.
    fn rollup(&self, from_day: i64, to_day: i64) -> StoreResult<()>;
    /// Deletes raw rows older than `retention_days` whole days before `now`. Rollups are kept.
    fn prune(&self, now: i64, retention_days: u32) -> StoreResult<PruneStats>;
    /// Marks sessions still `ok` whose last item is older than `older_than` as `abnormal`.
    fn close_stale_sessions(&self, older_than: i64) -> StoreResult<u64>;
}
