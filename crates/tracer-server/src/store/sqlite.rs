//! SQLite implementation of [`Store`].
//!
//! One connection, behind a mutex, takes every write; callers reach it through `spawn_blocking`, so the
//! writer is serialised without a dedicated thread. Reads use a small pool of `query_only` connections.
//! WAL mode lets readers proceed while a write transaction is open.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use tracer_proto::{FRAME_BUCKETS, FrameStats, IngestResponse, SessionStatus};

use super::types::{
    AdminSession, AppGlance, Breakdown, CatalogEntry, CrashBrief, CrashRow, CustomRow, DayTotals, DropRow, EventRow, IssueFilter, IssueRow, PerfRow,
    VersionInfo,
};
use super::{AppRecord, KeyRecord, PruneStats, Store, StoreError, StoreResult, read};
use crate::ingest::{DAY_MS, Prepared, PreparedBody, PreparedCrash, PreparedItem};

/// Schema migrations, applied in order. Add a new file; never edit one that has shipped.
pub const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../../migrations/0001_init.sql")),
    ("0002_dashboard", include_str!("../../migrations/0002_dashboard.sql")),
    ("0003_hardening", include_str!("../../migrations/0003_hardening.sql")),
];

/// `items_seen` rows older than this many days are dropped by retention.
const ITEMS_SEEN_DAYS: i64 = 35;
const READER_POOL: usize = 4;
/// Most distinct catalogs kept per app version, and per app.
const MAX_CATALOGS_PER_VERSION: i64 = 8;
const MAX_CATALOGS_PER_APP: i64 = 200;
/// Most new crash issues an app can create per day; the rest share one overflow issue.
const MAX_NEW_ISSUES_PER_DAY: i64 = 200;
/// Most distinct drop reasons stored per app per day; the rest are counted as `other`.
const MAX_DROP_REASONS_PER_DAY: i64 = 64;
/// A stored drop total never exceeds this.
const DROP_TOTAL_MAX: i64 = 1_000_000_000_000;
/// Most distinct versions, operating systems and event names kept per app in the rollups; the long tail is
/// folded into `other`.
const ROLLUP_MAX_VERSIONS: i64 = 50;
const ROLLUP_MAX_OS: i64 = 20;
const ROLLUP_MAX_EVENTS: i64 = 500;

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(failure, message) = &error
            && failure.code == rusqlite::ErrorCode::ConstraintViolation
        {
            return StoreError::Conflict(message.clone().unwrap_or_else(|| "constraint violated".to_string()));
        }
        StoreError::Backend(error.to_string())
    }
}

pub struct SqliteStore {
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    path: std::path::PathBuf,
}

impl SqliteStore {
    /// Opens (creating if needed) the database at `path` and brings the schema up to date.
    pub fn open(path: &Path) -> StoreResult<Self> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|error| StoreError::Backend(format!("cannot create data directory: {error}")))?;
        }
        let mut writer = Connection::open(path)?;
        configure(&writer)?;
        migrate(&mut writer)?;
        Ok(Self { writer: Mutex::new(writer), readers: Mutex::new(Vec::new()), path: path.to_path_buf() })
    }

    fn write(&self) -> MutexGuard<'_, Connection> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn read<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> StoreResult<T> {
        let pooled = self.readers.lock().unwrap_or_else(PoisonError::into_inner).pop();
        let conn = match pooled {
            Some(conn) => conn,
            None => {
                let conn = Connection::open(&self.path)?;
                configure(&conn)?;
                conn.execute_batch("PRAGMA query_only = ON;")?;
                conn
            }
        };
        let result = f(&conn);
        let mut pool = self.readers.lock().unwrap_or_else(PoisonError::into_inner);
        if pool.len() < READER_POOL {
            pool.push(conn);
        }
        Ok(result?)
    }
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")
}

/// Applies the migrations that the database has not seen, tracked in `PRAGMA user_version`.
pub fn migrate(conn: &mut Connection) -> StoreResult<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let known = MIGRATIONS.len() as i64;
    if current > known {
        return Err(StoreError::Backend(format!("database schema version {current} is newer than this server understands ({known})")));
    }
    if current == 0 {
        // Must be chosen before the first table exists; lets retention return space to the file system.
        conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL;")?;
    }
    for (index, (_name, sql)) in MIGRATIONS.iter().enumerate() {
        let version = index as i64 + 1;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {version};"))?;
        tx.commit()?;
    }
    Ok(())
}

fn app_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppRecord> {
    Ok(AppRecord { id: row.get(0)?, slug: row.get(1)?, name: row.get(2)?, created_at: row.get(3)? })
}

fn n(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl Store for SqliteStore {
    fn health(&self) -> StoreResult<()> {
        self.read(|conn| conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0)).map(|_| ()))
    }

    fn create_app(&self, slug: &str, name: &str, first_key: &str, now: i64) -> StoreResult<AppRecord> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        tx.execute("INSERT INTO apps (slug, name, created_at) VALUES (?1, ?2, ?3)", params![slug, name, now])?;
        let id = tx.last_insert_rowid();
        tx.execute("INSERT INTO app_keys (key, app_id, created_at) VALUES (?1, ?2, ?3)", params![first_key, id, now])?;
        tx.commit()?;
        Ok(AppRecord { id, slug: slug.to_string(), name: name.to_string(), created_at: now })
    }

    fn list_apps(&self) -> StoreResult<Vec<AppRecord>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached("SELECT id, slug, name, created_at FROM apps ORDER BY slug")?;
            stmt.query_map([], app_row)?.collect()
        })
    }

    fn find_app(&self, slug: &str) -> StoreResult<Option<AppRecord>> {
        self.read(|conn| conn.query_row("SELECT id, slug, name, created_at FROM apps WHERE slug = ?1", [slug], app_row).optional())
    }

    fn create_key(&self, app_id: i64, key: &str, now: i64) -> StoreResult<()> {
        self.write().execute("INSERT INTO app_keys (key, app_id, created_at) VALUES (?1, ?2, ?3)", params![key, app_id, now])?;
        Ok(())
    }

    fn revoke_key(&self, key: &str, now: i64) -> StoreResult<bool> {
        let changed = self.write().execute("UPDATE app_keys SET revoked_at = ?2 WHERE key = ?1 AND revoked_at IS NULL", params![key, now])?;
        Ok(changed > 0)
    }

    fn list_keys(&self, app_id: i64) -> StoreResult<Vec<KeyRecord>> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare_cached("SELECT key, app_id, created_at, revoked_at FROM app_keys WHERE app_id = ?1 ORDER BY created_at, key")?;
            stmt.query_map([app_id], |row| Ok(KeyRecord { key: row.get(0)?, app_id: row.get(1)?, created_at: row.get(2)?, revoked_at: row.get(3)? }))?
                .collect()
        })
    }

    fn lookup_key(&self, key: &str) -> StoreResult<Option<AppRecord>> {
        self.read(|conn| {
            conn.query_row(
                "SELECT a.id, a.slug, a.name, a.created_at FROM app_keys k JOIN apps a ON a.id = k.app_id WHERE k.key = ?1 AND k.revoked_at IS NULL",
                [key],
                app_row,
            )
            .optional()
        })
    }

    fn create_admin(&self, username: &str, password_hash: &str, now: i64) -> StoreResult<()> {
        self.write()
            .execute("INSERT INTO admins (username, password_hash, created_at) VALUES (?1, ?2, ?3)", params![username, password_hash, now])?;
        Ok(())
    }

    fn admin_password_hash(&self, username: &str) -> StoreResult<Option<(i64, String)>> {
        self.read(|conn| {
            conn.query_row("SELECT id, password_hash FROM admins WHERE username = ?1", [username], |row| Ok((row.get(0)?, row.get(1)?))).optional()
        })
    }

    fn ingest(&self, app_id: i64, batch: &Prepared) -> StoreResult<IngestResponse> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        let response = ingest_tx(&tx, app_id, batch)?;
        tx.commit()?;
        Ok(response)
    }

    fn forget(&self, app_id: i64, install_id: &str) -> StoreResult<u64> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        let mut removed = 0usize;
        // Keep issue counts honest before the crash rows go.
        tx.execute(
            "UPDATE issues SET count = max(0, count - (SELECT count(*) FROM crashes WHERE issue_id = issues.id AND install_id = ?2))
             WHERE app_id = ?1 AND id IN (SELECT issue_id FROM crashes WHERE app_id = ?1 AND install_id = ?2)",
            params![app_id, install_id],
        )?;
        for table in ["events", "perf", "crashes", "sessions", "daily_installs", "installs"] {
            removed += tx.execute(&format!("DELETE FROM {table} WHERE app_id = ?1 AND install_id = ?2"), params![app_id, install_id])?;
        }
        removed +=
            tx.execute("DELETE FROM issues WHERE app_id = ?1 AND NOT EXISTS (SELECT 1 FROM crashes WHERE crashes.issue_id = issues.id)", [app_id])?;
        tx.commit()?;
        Ok(removed as u64)
    }

    fn rollup(&self, from_day: i64, to_day: i64) -> StoreResult<()> {
        // One app per transaction. Raw rows are read on a reader connection, so the writer lock is held only
        // for the deletes and inserts of one app, and ingest for the others is never stalled for long.
        for app in self.list_apps()? {
            let (keep, perf) = self.read(|conn| {
                let keep = load_keep(conn, app.id, from_day, to_day)?;
                let perf = load_perf(conn, app.id, from_day, to_day, &keep)?;
                Ok((keep, perf))
            })?;
            let mut conn = self.write();
            let tx = conn.transaction()?;
            rollup_app_tx(&tx, app.id, from_day, to_day, &keep, perf)?;
            tx.commit()?;
        }
        Ok(())
    }

    fn size_bytes(&self) -> StoreResult<u64> {
        self.read(|conn| {
            let pages: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
            let size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
            Ok(u64::try_from(pages.saturating_mul(size)).unwrap_or(0))
        })
    }

    fn prune(&self, now: i64, retention_days: u32) -> StoreResult<PruneStats> {
        let mut conn = self.write();
        let tx = conn.transaction()?;
        let cutoff_day = now.div_euclid(DAY_MS) - i64::from(retention_days);
        let cutoff_ms = cutoff_day.saturating_mul(DAY_MS);
        let stats = PruneStats {
            events: tx.execute("DELETE FROM events WHERE day < ?1", [cutoff_day])? as u64,
            perf: tx.execute("DELETE FROM perf WHERE day < ?1", [cutoff_day])? as u64,
            crashes: tx.execute("DELETE FROM crashes WHERE day < ?1", [cutoff_day])? as u64,
            sessions: tx.execute("DELETE FROM sessions WHERE day < ?1", [cutoff_day])? as u64,
            issues: tx.execute(
                "DELETE FROM issues WHERE last_seen < ?1 AND NOT EXISTS (SELECT 1 FROM crashes WHERE crashes.issue_id = issues.id)",
                [cutoff_ms],
            )? as u64,
            items_seen: tx.execute("DELETE FROM items_seen WHERE received_at < ?1", [now - ITEMS_SEEN_DAYS * DAY_MS])? as u64,
            installs: tx.execute("DELETE FROM installs WHERE last_seen < ?1", [cutoff_ms])? as u64,
        };
        tx.execute("DELETE FROM admin_sessions WHERE expires_at < ?1", [now])?;
        tx.execute("DELETE FROM daily_drops WHERE day < ?1", [cutoff_day])?;
        // Catalogs not seen within retention go; a client that still needs one is asked for it again.
        tx.execute("DELETE FROM catalogs WHERE received_at < ?1", [cutoff_ms])?;
        tx.execute(
            "DELETE FROM catalog_events WHERE NOT EXISTS (
                 SELECT 1 FROM catalogs c WHERE c.app_id = catalog_events.app_id AND c.version = catalog_events.version AND c.hash = catalog_events.hash)",
            [],
        )?;
        tx.commit()?;
        // Give space back to the file system and fold the WAL into the main file.
        conn.execute_batch("PRAGMA incremental_vacuum; PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(stats)
    }

    fn close_stale_sessions(&self, older_than: i64) -> StoreResult<u64> {
        let changed = self.write().execute("UPDATE sessions SET status = 'abnormal' WHERE status = 'ok' AND last_seen < ?1", [older_than])?;
        Ok(changed as u64)
    }

    fn admin_session_create(&self, token_hash: &str, admin_id: i64, now: i64, expires_at: i64) -> StoreResult<()> {
        self.write().execute(
            "INSERT INTO admin_sessions (token_hash, admin_id, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![token_hash, admin_id, now, expires_at],
        )?;
        Ok(())
    }

    fn admin_session_lookup(&self, token_hash: &str, now: i64) -> StoreResult<Option<AdminSession>> {
        self.read(|conn| {
            conn.query_row(
                "SELECT a.id, a.username FROM admin_sessions s JOIN admins a ON a.id = s.admin_id WHERE s.token_hash = ?1 AND s.expires_at > ?2",
                params![token_hash, now],
                |row| Ok(AdminSession { admin_id: row.get(0)?, username: row.get(1)? }),
            )
            .optional()
        })
    }

    fn admin_session_delete(&self, token_hash: &str) -> StoreResult<()> {
        self.write().execute("DELETE FROM admin_sessions WHERE token_hash = ?1", [token_hash])?;
        Ok(())
    }

    fn admin_session_purge(&self, now: i64) -> StoreResult<u64> {
        Ok(self.write().execute("DELETE FROM admin_sessions WHERE expires_at <= ?1", [now])? as u64)
    }

    fn admin_count(&self) -> StoreResult<i64> {
        self.read(|conn| conn.query_row("SELECT count(*) FROM admins", [], |row| row.get(0)))
    }

    fn app_glance(&self, app_id: i64, from_day: i64, to_day: i64) -> StoreResult<AppGlance> {
        self.read(|conn| read::app_glance(conn, app_id, from_day, to_day))
    }

    fn versions(&self, app_id: i64) -> StoreResult<Vec<VersionInfo>> {
        self.read(|conn| read::versions(conn, app_id))
    }

    fn day_totals(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>) -> StoreResult<Vec<DayTotals>> {
        self.read(|conn| read::day_totals(conn, app_id, from_day, to_day, version))
    }

    fn active_installs_by_day(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>) -> StoreResult<Vec<(i64, i64)>> {
        self.read(|conn| read::active_installs_by_day(conn, app_id, from_day, to_day, version))
    }

    fn active_installs_between(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>) -> StoreResult<i64> {
        self.read(|conn| read::active_installs_between(conn, app_id, from_day, to_day, version))
    }

    fn list_issues(&self, app_id: i64, filter: &IssueFilter<'_>, limit: i64) -> StoreResult<Vec<IssueRow>> {
        self.read(|conn| read::list_issues(conn, app_id, filter, limit))
    }

    fn get_issue(&self, app_id: i64, issue_id: i64) -> StoreResult<Option<IssueRow>> {
        self.read(|conn| read::get_issue(conn, app_id, issue_id))
    }

    fn representative_crash(&self, app_id: i64, issue_id: i64) -> StoreResult<Option<CrashRow>> {
        self.read(|conn| read::representative_crash(conn, app_id, issue_id))
    }

    fn recent_crashes(&self, app_id: i64, issue_id: i64, limit: i64) -> StoreResult<Vec<CrashBrief>> {
        self.read(|conn| read::recent_crashes(conn, app_id, issue_id, limit))
    }

    fn issue_breakdown(&self, app_id: i64, issue_id: i64) -> StoreResult<Breakdown> {
        self.read(|conn| read::issue_breakdown(conn, app_id, issue_id))
    }

    fn set_issue_status(&self, app_id: i64, issue_id: i64, status: &str) -> StoreResult<bool> {
        Ok(read::set_issue_status(&self.write(), app_id, issue_id, status)?)
    }

    fn perf_rows(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>) -> StoreResult<Vec<PerfRow>> {
        self.read(|conn| read::perf_rows(conn, app_id, from_day, to_day, version))
    }

    fn recent_custom_metrics(&self, app_id: i64, since_day: i64, version: Option<&str>, limit: i64) -> StoreResult<Vec<CustomRow>> {
        self.read(|conn| read::recent_custom_metrics(conn, app_id, since_day, version, limit))
    }

    fn events_ranked(&self, app_id: i64, from_day: i64, to_day: i64, version: Option<&str>, declared: bool) -> StoreResult<Vec<EventRow>> {
        self.read(|conn| read::events_ranked(conn, app_id, from_day, to_day, version, declared))
    }

    fn never_fired(&self, app_id: i64, version: Option<&str>) -> StoreResult<Vec<CatalogEntry>> {
        self.read(|conn| read::never_fired(conn, app_id, version))
    }

    fn drops(&self, app_id: i64, from_day: i64, to_day: i64) -> StoreResult<Vec<DropRow>> {
        self.read(|conn| read::drops(conn, app_id, from_day, to_day))
    }
}

// ---------------------------------------------------------------------------------------------------------
// ingest

fn ingest_tx(tx: &Transaction<'_>, app_id: i64, batch: &Prepared) -> rusqlite::Result<IngestResponse> {
    let device = batch.device.as_ref();
    let nonempty = |text: &str| (!text.is_empty()).then(|| text.to_string());
    let os_name = device.and_then(|d| nonempty(&d.os_name));
    let arch = device.and_then(|d| nonempty(&d.arch));
    tx.prepare_cached(
        "INSERT INTO installs (app_id, install_id, first_seen, last_seen, first_version, last_version,
                               os_name, os_version, arch, cpu_brand, cpu_cores, ram_mb, gpu_name, gpu_backend, locale)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (app_id, install_id) DO UPDATE SET
             first_version = CASE WHEN excluded.first_seen < installs.first_seen THEN excluded.first_version ELSE installs.first_version END,
             last_version  = CASE WHEN excluded.last_seen >= installs.last_seen THEN excluded.last_version ELSE installs.last_version END,
             first_seen    = min(installs.first_seen, excluded.first_seen),
             last_seen     = max(installs.last_seen, excluded.last_seen),
             os_name       = COALESCE(excluded.os_name, installs.os_name),
             os_version    = COALESCE(excluded.os_version, installs.os_version),
             arch          = COALESCE(excluded.arch, installs.arch),
             cpu_brand     = COALESCE(excluded.cpu_brand, installs.cpu_brand),
             cpu_cores     = COALESCE(excluded.cpu_cores, installs.cpu_cores),
             ram_mb        = COALESCE(excluded.ram_mb, installs.ram_mb),
             gpu_name      = COALESCE(excluded.gpu_name, installs.gpu_name),
             gpu_backend   = COALESCE(excluded.gpu_backend, installs.gpu_backend),
             locale        = COALESCE(excluded.locale, installs.locale)",
    )?
    .execute(params![
        app_id,
        batch.install_id,
        batch.started_at,
        batch.last_seen,
        batch.version,
        os_name,
        device.and_then(|d| d.os_version.clone()),
        arch,
        device.and_then(|d| d.cpu_brand.clone()),
        device.and_then(|d| d.cpu_cores),
        device.and_then(|d| d.ram_mb).map(n),
        device.and_then(|d| d.gpu.as_ref()).map(|g| g.name.clone()),
        device.and_then(|d| d.gpu.as_ref()).and_then(|g| g.backend.clone()),
        device.and_then(|d| d.locale.clone()),
    ])?;

    let known_os: Option<String> = tx
        .query_row("SELECT os_name FROM installs WHERE app_id = ?1 AND install_id = ?2", params![app_id, batch.install_id], |row| row.get(0))
        .optional()?
        .flatten();
    let os_name = os_name.or(known_os);
    let os_version = device.and_then(|d| d.os_version.clone());
    let device_json = batch.device.as_ref().and_then(|d| serde_json::to_string(d).ok());

    // A session row exists only for envelopes that carry a session item. Crashes, performance windows and
    // events from a user who switched session reporting off keep their session id as plain text.
    if let Some((first, last)) = batch.session_span {
        tx.prepare_cached(
            "INSERT INTO sessions (app_id, session_id, install_id, version, build, process, os_name, started_at, day, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8 / 86400000, ?9)
             ON CONFLICT (app_id, session_id) DO UPDATE SET
                 started_at = min(sessions.started_at, excluded.started_at),
                 day        = min(sessions.started_at, excluded.started_at) / 86400000,
                 last_seen  = max(sessions.last_seen, excluded.last_seen)
             WHERE sessions.install_id = excluded.install_id",
        )?
        .execute(params![app_id, batch.session_id, batch.install_id, batch.version, batch.build, batch.process, os_name, first, last])?;
    }

    let ctx = Ctx {
        app_id,
        batch,
        os_name: os_name.as_deref(),
        os_version: os_version.as_deref(),
        arch: arch.as_deref(),
        device_json: device_json.as_deref(),
    };
    let mut response = IngestResponse { rejected: batch.rejected, ..IngestResponse::default() };
    for item in &batch.items {
        let seen: bool = tx
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM items_seen WHERE app_id = ?1 AND item_id = ?2)")?
            .query_row(params![app_id, item.id], |row| row.get(0))?;
        if seen {
            response.duplicates += 1;
            continue;
        }
        if store_item(tx, &ctx, item, &mut response)? {
            tx.prepare_cached("INSERT INTO items_seen (app_id, item_id, received_at) VALUES (?1, ?2, ?3)")?.execute(params![
                app_id,
                item.id,
                batch.received_at
            ])?;
            response.accepted += 1;
        } else {
            response.rejected += 1;
        }
    }
    Ok(response)
}

struct Ctx<'a> {
    app_id: i64,
    batch: &'a Prepared,
    os_name: Option<&'a str>,
    os_version: Option<&'a str>,
    arch: Option<&'a str>,
    device_json: Option<&'a str>,
}

/// Stores one item. `false` means it was refused (and its id is not recorded).
fn store_item(tx: &Transaction<'_>, ctx: &Ctx<'_>, item: &PreparedItem, response: &mut IngestResponse) -> rusqlite::Result<bool> {
    let batch = ctx.batch;
    let app_id = ctx.app_id;
    let day = item.ts.div_euclid(DAY_MS);
    match &item.body {
        PreparedBody::SessionStart { catalog_hash, catalog } => {
            if let Some(hash) = catalog_hash {
                tx.prepare_cached("UPDATE sessions SET catalog_hash = ?4 WHERE app_id = ?1 AND session_id = ?2 AND install_id = ?3")?
                    .execute(params![app_id, batch.session_id, batch.install_id, hash])?;
            }
            let held: bool = match catalog_hash {
                Some(hash) => tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM catalogs WHERE app_id = ?1 AND version = ?2 AND hash = ?3)",
                    params![app_id, batch.version, hash],
                    |row| row.get(0),
                )?,
                None => true,
            };
            match (catalog, held) {
                // Catalogs are additive and keyed by hash: a new one is stored next to the others, never in
                // place of one. Past the caps it is ignored (the session still counts).
                (Some(events), false) => {
                    let hash = catalog_hash.as_deref().unwrap_or("unhashed");
                    let per_version: i64 =
                        tx.query_row("SELECT count(*) FROM catalogs WHERE app_id = ?1 AND version = ?2", params![app_id, batch.version], |row| {
                            row.get(0)
                        })?;
                    let per_app: i64 = tx.query_row("SELECT count(*) FROM catalogs WHERE app_id = ?1", params![app_id], |row| row.get(0))?;
                    if per_version < MAX_CATALOGS_PER_VERSION && per_app < MAX_CATALOGS_PER_APP {
                        tx.execute(
                            "INSERT INTO catalogs (app_id, version, hash, received_at) VALUES (?1, ?2, ?3, ?4)",
                            params![app_id, batch.version, hash, batch.received_at],
                        )?;
                        let mut insert = tx.prepare_cached(
                            "INSERT OR IGNORE INTO catalog_events (app_id, version, hash, name, category) VALUES (?1, ?2, ?3, ?4, ?5)",
                        )?;
                        for event in events {
                            insert.execute(params![app_id, batch.version, hash, event.name, event.category])?;
                        }
                    }
                }
                (_, true) => {
                    if let Some(hash) = catalog_hash {
                        tx.execute(
                            "UPDATE catalogs SET received_at = max(received_at, ?4) WHERE app_id = ?1 AND version = ?2 AND hash = ?3",
                            params![app_id, batch.version, hash, batch.received_at],
                        )?;
                    }
                }
                (None, false) => response.need_catalog = true,
            }
        }
        PreparedBody::Heartbeat(totals) => {
            tx.prepare_cached(
                "UPDATE sessions SET duration_s = max(duration_s, ?4), focused_s = max(focused_s, ?5), active_s = max(active_s, ?6),
                        idle_s = max(idle_s, ?7), last_seen = max(last_seen, ?8)
                 WHERE app_id = ?1 AND session_id = ?2 AND install_id = ?3",
            )?
            .execute(params![
                app_id,
                batch.session_id,
                batch.install_id,
                n(totals.duration_s),
                n(totals.focused_s),
                n(totals.active_s),
                n(totals.idle_s),
                item.ts
            ])?;
        }
        PreparedBody::End { status, totals, errors } => {
            tx.prepare_cached(
                "UPDATE sessions SET duration_s = max(duration_s, ?4), focused_s = max(focused_s, ?5), active_s = max(active_s, ?6),
                        idle_s = max(idle_s, ?7), errors = max(errors, ?8), last_seen = max(last_seen, ?9),
                        status = CASE WHEN status = 'ok' THEN ?10 ELSE status END
                 WHERE app_id = ?1 AND session_id = ?2 AND install_id = ?3",
            )?
            .execute(params![
                app_id,
                batch.session_id,
                batch.install_id,
                n(totals.duration_s),
                n(totals.focused_s),
                n(totals.active_s),
                n(totals.idle_s),
                i64::from(*errors),
                item.ts,
                status.as_str()
            ])?;
            if *status == SessionStatus::Crashed {
                mark_last_panic_fatal(tx, app_id, &batch.session_id, &batch.install_id)?;
            }
        }
        PreparedBody::Event(event) => {
            // An event is never refused for being undeclared: the catalog is client input and must not be
            // able to cause data loss. It is stored with `declared = 0` and shown apart.
            let held: bool = tx
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM catalog_events WHERE app_id = ?1 AND version = ?2)")?
                .query_row(params![app_id, batch.version], |row| row.get(0))?;
            let declared = if event.dynamic {
                true
            } else if held {
                tx.prepare_cached("SELECT EXISTS(SELECT 1 FROM catalog_events WHERE app_id = ?1 AND version = ?2 AND name = ?3)")?
                    .query_row(params![app_id, batch.version, event.name], |row| row.get(0))?
            } else {
                response.need_catalog = true;
                true
            };
            let props = (!event.props.is_empty()).then(|| serde_json::to_string(&event.props).ok()).flatten();
            tx.prepare_cached(
                "INSERT INTO events (app_id, install_id, session_id, ts, day, version, name, category, dynamic, props, declared)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?
            .execute(params![
                app_id,
                batch.install_id,
                batch.session_id,
                item.ts,
                day,
                batch.version,
                event.name,
                event.category,
                event.dynamic,
                props,
                declared
            ])?;
        }
        PreparedBody::Perf(perf) => {
            let frames = perf.frames.clone().unwrap_or_default();
            let buckets = perf.frames.as_ref().and_then(|f| serde_json::to_string(&f.buckets).ok());
            let counters = (!perf.counters.is_empty()).then(|| serde_json::to_string(&perf.counters).ok()).flatten();
            let gauges = (!perf.gauges.is_empty()).then(|| serde_json::to_string(&perf.gauges).ok()).flatten();
            tx.prepare_cached(
                "INSERT INTO perf (app_id, install_id, session_id, ts, day, version, os_name, window_s, frame_count, frame_sum_us, frame_max_us,
                                   slow, frozen, buckets, cpu_pct, rss_mb, rss_peak_mb, startup_ms, counters, gauges)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            )?
            .execute(params![
                app_id,
                batch.install_id,
                batch.session_id,
                item.ts,
                day,
                batch.version,
                ctx.os_name,
                i64::from(perf.window_s),
                n(frames.count),
                n(frames.sum_us),
                i64::from(frames.max_us),
                n(frames.slow),
                n(frames.frozen),
                buckets,
                perf.cpu_pct,
                perf.rss_mb,
                perf.rss_peak_mb,
                perf.startup_ms,
                counters,
                gauges
            ])?;
        }
        PreparedBody::Crash(crash) => store_crash(tx, ctx, item, crash, day)?,
        PreparedBody::Drops(reasons) => {
            let mut upsert = tx.prepare_cached(
                "INSERT INTO daily_drops (app_id, day, reason, count) VALUES (?1, ?2, ?3, min(?4, ?5))
                 ON CONFLICT (app_id, day, reason) DO UPDATE SET count = min(count + excluded.count, ?5)",
            )?;
            for (reason, count) in reasons {
                let known: bool = tx
                    .prepare_cached("SELECT EXISTS(SELECT 1 FROM daily_drops WHERE app_id = ?1 AND day = ?2 AND reason = ?3)")?
                    .query_row(params![app_id, day, reason], |row| row.get(0))?;
                let reason = if known {
                    reason.as_str()
                } else {
                    let distinct: i64 =
                        tx.query_row("SELECT count(*) FROM daily_drops WHERE app_id = ?1 AND day = ?2", params![app_id, day], |row| row.get(0))?;
                    if distinct >= MAX_DROP_REASONS_PER_DAY { "other" } else { reason.as_str() }
                };
                upsert.execute(params![app_id, day, reason, n(*count), DROP_TOTAL_MAX])?;
            }
        }
    }
    Ok(true)
}

fn store_crash(tx: &Transaction<'_>, ctx: &Ctx<'_>, item: &PreparedItem, prepared: &PreparedCrash, day: i64) -> rusqlite::Result<()> {
    let batch = ctx.batch;
    let app_id = ctx.app_id;
    let crash = &prepared.crash;
    let kind = crash.kind.as_str();
    // A flood of distinct fingerprints cannot create unbounded issues: past the daily cap, new ones share
    // one overflow issue for the day instead of being dropped silently.
    let received_day = batch.received_at.div_euclid(DAY_MS);
    let known: bool =
        tx.query_row("SELECT EXISTS(SELECT 1 FROM issues WHERE app_id = ?1 AND fingerprint = ?2)", params![app_id, prepared.fingerprint], |row| {
            row.get(0)
        })?;
    let created_today: i64 =
        tx.query_row("SELECT count(*) FROM issues WHERE app_id = ?1 AND created_day = ?2", params![app_id, received_day], |row| row.get(0))?;
    let overflow = !known && created_today >= MAX_NEW_ISSUES_PER_DAY;
    let (fingerprint, title) = if overflow {
        (format!("overflow:{received_day}"), "Too many distinct crashes".to_string())
    } else {
        (prepared.fingerprint.clone(), prepared.title.clone())
    };
    let issue_id: i64 = tx
        .prepare_cached(
            "INSERT INTO issues (app_id, fingerprint, kind, title, first_seen, last_seen, first_version, last_version, count, created_day)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?6, 1, ?7)
             ON CONFLICT (app_id, fingerprint) DO UPDATE SET
                 first_version = CASE WHEN excluded.first_seen < issues.first_seen THEN excluded.first_version ELSE issues.first_version END,
                 last_version  = CASE WHEN excluded.last_seen >= issues.last_seen THEN excluded.last_version ELSE issues.last_version END,
                 first_seen    = min(issues.first_seen, excluded.first_seen),
                 last_seen     = max(issues.last_seen, excluded.last_seen),
                 count         = issues.count + 1
             RETURNING id",
        )?
        .query_row(params![app_id, fingerprint, kind, title, item.ts, batch.version, received_day], |row| row.get(0))?;
    let breadcrumbs = (!crash.breadcrumbs.is_empty()).then(|| serde_json::to_string(&crash.breadcrumbs).ok()).flatten();
    let perf = crash.perf.as_ref().and_then(|perf| serde_json::to_string(perf).ok());
    tx.prepare_cached(
        "INSERT INTO crashes (app_id, issue_id, install_id, session_id, ts, day, version, build, os_name, os_version, arch, kind, fatal,
                              message, location, thread, backtrace, blocked_ms, exit_code, signal, context, breadcrumbs, perf, device)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
    )?
    .execute(params![
        app_id,
        issue_id,
        batch.install_id,
        batch.session_id,
        item.ts,
        day,
        batch.version,
        batch.build,
        ctx.os_name,
        ctx.os_version,
        ctx.arch,
        kind,
        crash.fatal,
        crash.message,
        crash.location,
        crash.thread,
        crash.backtrace,
        crash.blocked_ms,
        crash.exit_code,
        crash.signal,
        crash.context,
        breadcrumbs,
        perf,
        ctx.device_json
    ])?;
    if crash.fatal && crash.kind == tracer_proto::CrashKind::Panic {
        tx.prepare_cached("UPDATE sessions SET status = 'crashed' WHERE app_id = ?1 AND session_id = ?2 AND install_id = ?3 AND status = 'ok'")?
            .execute(params![app_id, batch.session_id, batch.install_id])?;
    }
    if crash.kind == tracer_proto::CrashKind::Panic {
        // The session may already be known to have ended in a crash (the end arrived first).
        let ended_crashed: bool = tx
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM sessions WHERE app_id = ?1 AND session_id = ?2 AND install_id = ?3 AND status = 'crashed')")?
            .query_row(params![app_id, batch.session_id, batch.install_id], |row| row.get(0))?;
        if ended_crashed {
            mark_last_panic_fatal(tx, app_id, &batch.session_id, &batch.install_id)?;
        }
    }
    Ok(())
}

/// A session that ended `crashed` took its most recent panic with it: that crash is fatal. The client cannot
/// know at capture time whether the host survives, so the server decides when the end arrives.
fn mark_last_panic_fatal(tx: &Transaction<'_>, app_id: i64, session_id: &str, install_id: &str) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "UPDATE crashes SET fatal = 1 WHERE id = (
             SELECT id FROM crashes WHERE app_id = ?1 AND session_id = ?2 AND install_id = ?3 AND kind = 'panic'
             ORDER BY ts DESC, id DESC LIMIT 1)",
    )?
    .execute(params![app_id, session_id, install_id])?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------
// rollups

/// Which values keep their own rollup rows for one app: the most used ones. Everything else is folded into
/// `other`, so a flood of made-up versions, operating systems or event names cannot grow the rollups.
struct Keep {
    versions: Vec<String>,
    os: Vec<String>,
    events: Vec<String>,
}

fn top_values(conn: &Connection, sql: &str, app_id: i64, from: i64, to: i64, limit: i64) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    stmt.query_map(params![app_id, from, to, limit], |row| row.get(0))?.collect()
}

fn load_keep(conn: &Connection, app_id: i64, from: i64, to: i64) -> rusqlite::Result<Keep> {
    let versions = top_values(
        conn,
        "SELECT v FROM (
             SELECT version AS v FROM sessions WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3
             UNION ALL SELECT version FROM perf WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3
             UNION ALL SELECT version FROM events WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3)
         GROUP BY v ORDER BY count(*) DESC, v LIMIT ?4",
        app_id,
        from,
        to,
        ROLLUP_MAX_VERSIONS,
    )?;
    let os = top_values(
        conn,
        "SELECT o FROM (
             SELECT COALESCE(os_name, 'unknown') AS o FROM sessions WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3
             UNION ALL SELECT COALESCE(os_name, 'unknown') FROM perf WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3)
         GROUP BY o ORDER BY count(*) DESC, o LIMIT ?4",
        app_id,
        from,
        to,
        ROLLUP_MAX_OS,
    )?;
    let events = top_values(
        conn,
        "SELECT name FROM events WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3 GROUP BY name ORDER BY count(*) DESC, name LIMIT ?4",
        app_id,
        from,
        to,
        ROLLUP_MAX_EVENTS,
    )?;
    Ok(Keep { versions, os, events })
}

fn json_list(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}

#[derive(Default)]
struct PerfAcc {
    frames: FrameStats,
    cpu_sum: f64,
    cpu_n: i64,
    rss_sum: i64,
    rss_n: i64,
    rss_peak: i64,
    startup_sum: i64,
    startup_n: i64,
}

type PerfKey = (i64, String, String);

fn fold(value: String, keep: &[String]) -> String {
    if keep.contains(&value) { value } else { "other".to_string() }
}

/// Merges the raw performance rows of one app into per-day, per-version, per-OS accumulators. Runs on a
/// reader connection, so the writer is not held while rows are loaded.
fn load_perf(conn: &Connection, app_id: i64, from: i64, to: i64, keep: &Keep) -> rusqlite::Result<BTreeMap<PerfKey, PerfAcc>> {
    let mut groups: BTreeMap<PerfKey, PerfAcc> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT day, version, COALESCE(os_name, 'unknown'), frame_count, frame_sum_us, frame_max_us, slow, frozen, buckets,
                cpu_pct, rss_mb, rss_peak_mb, startup_ms
         FROM perf WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3",
    )?;
    let mut rows = stmt.query(params![app_id, from, to])?;
    while let Some(row) = rows.next()? {
        let key: PerfKey = (row.get(0)?, fold(row.get(1)?, &keep.versions), fold(row.get(2)?, &keep.os));
        let buckets: Option<String> = row.get(8)?;
        let buckets: Vec<u64> = buckets.and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        let non_negative = |value: i64| u64::try_from(value).unwrap_or(0);
        let frames = FrameStats {
            count: non_negative(row.get(3)?),
            sum_us: non_negative(row.get(4)?),
            max_us: u32::try_from(row.get::<_, i64>(5)?.max(0)).unwrap_or(u32::MAX),
            slow: non_negative(row.get(6)?),
            frozen: non_negative(row.get(7)?),
            buckets,
        };
        let acc = groups.entry(key).or_default();
        acc.frames.merge(&frames);
        if let Some(cpu) = row.get::<_, Option<f64>>(9)? {
            acc.cpu_sum += cpu;
            acc.cpu_n += 1;
        }
        if let Some(rss) = row.get::<_, Option<i64>>(10)? {
            acc.rss_sum += rss;
            acc.rss_n += 1;
        }
        let peak = row.get::<_, Option<i64>>(11)?.or(row.get::<_, Option<i64>>(10)?);
        acc.rss_peak = acc.rss_peak.max(peak.unwrap_or(0));
        if let Some(startup) = row.get::<_, Option<i64>>(12)? {
            acc.startup_sum += startup;
            acc.startup_n += 1;
        }
    }
    Ok(groups)
}

/// Replaces one app's rollup rows for the day range. Runs in its own transaction, so the writer is held for
/// one app at a time.
fn rollup_app_tx(tx: &Transaction<'_>, app_id: i64, from: i64, to: i64, keep: &Keep, perf: BTreeMap<PerfKey, PerfAcc>) -> rusqlite::Result<()> {
    for table in ["daily_installs", "daily_sessions", "daily_events", "daily_perf"] {
        tx.execute(&format!("DELETE FROM {table} WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3"), params![app_id, from, to])?;
    }
    tx.execute(
        "INSERT INTO daily_installs (app_id, day, install_id)
         SELECT app_id, day, install_id FROM sessions WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3
         UNION SELECT app_id, day, install_id FROM events WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3
         UNION SELECT app_id, day, install_id FROM perf WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3",
        params![app_id, from, to],
    )?;
    let (versions, os, events) = (json_list(&keep.versions), json_list(&keep.os), json_list(&keep.events));
    tx.execute(
        "INSERT INTO daily_sessions (app_id, day, version, os_name, sessions, opens, installs, exited, crashed, abnormal, errored,
                                     crashed_installs, duration_s, focused_s, active_s, idle_s)
         SELECT app_id, day,
                CASE WHEN version IN (SELECT value FROM json_each(?4)) THEN version ELSE 'other' END,
                CASE WHEN COALESCE(os_name, 'unknown') IN (SELECT value FROM json_each(?5)) THEN COALESCE(os_name, 'unknown') ELSE 'other' END,
                count(*),
                sum(process = 'gui'),
                count(DISTINCT install_id),
                sum(status = 'exited'),
                sum(status = 'crashed'),
                sum(status = 'abnormal'),
                sum(errors > 0),
                count(DISTINCT CASE WHEN status IN ('crashed', 'abnormal') THEN install_id END),
                sum(duration_s), sum(focused_s), sum(active_s), sum(idle_s)
         FROM sessions WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3
         GROUP BY app_id, day, 3, 4",
        params![app_id, from, to, versions, os],
    )?;
    tx.execute(
        "INSERT INTO daily_events (app_id, day, version, name, declared, category, count, installs)
         SELECT app_id, day,
                CASE WHEN version IN (SELECT value FROM json_each(?4)) THEN version ELSE 'other' END,
                CASE WHEN name IN (SELECT value FROM json_each(?5)) THEN name ELSE 'other' END,
                declared,
                CASE WHEN name IN (SELECT value FROM json_each(?5)) THEN min(category) ELSE 'other' END,
                count(*), count(DISTINCT install_id)
         FROM events WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3
         GROUP BY app_id, day, 3, 4, declared",
        params![app_id, from, to, versions, events],
    )?;
    let mut insert = tx.prepare(
        "INSERT INTO daily_perf (app_id, day, version, os_name, frame_count, frame_sum_us, frame_max_us, slow, frozen, buckets,
                                 cpu_sum, cpu_n, rss_sum, rss_n, rss_peak_mb, startup_sum, startup_n)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
    )?;
    for ((day, version, os_name), mut acc) in perf {
        acc.frames.buckets.resize(FRAME_BUCKETS, 0);
        let buckets = serde_json::to_string(&acc.frames.buckets).unwrap_or_else(|_| "[]".to_string());
        insert.execute(params![
            app_id,
            day,
            version,
            os_name,
            n(acc.frames.count),
            n(acc.frames.sum_us),
            i64::from(acc.frames.max_us),
            n(acc.frames.slow),
            n(acc.frames.frozen),
            buckets,
            acc.cpu_sum,
            acc.cpu_n,
            acc.rss_sum,
            acc.rss_n,
            acc.rss_peak,
            acc.startup_sum,
            acc.startup_n
        ])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_to_an_empty_database_and_are_tracked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let store = SqliteStore::open(&path).unwrap();
        let version: i64 = store.write().query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        drop(store);
        // Reopening applies nothing twice.
        let store = SqliteStore::open(&path).unwrap();
        let tables: i64 = store.write().query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table'", [], |row| row.get(0)).unwrap();
        assert!(tables >= 18, "{tables} tables");
    }

    #[test]
    fn a_database_from_the_future_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        Connection::open(&path).unwrap().execute_batch("PRAGMA user_version = 99;").unwrap();
        assert!(SqliteStore::open(&path).is_err());
    }
}
