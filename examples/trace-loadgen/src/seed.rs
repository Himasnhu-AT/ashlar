//! Bulk-seeds a large database with deterministic pseudo-random rows, by `INSERT ... SELECT` over a
//! recursive counter, in chunks. Direct SQL is orders of magnitude faster than going through ingest.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use ashlar_trace_server::store::{SqliteStore, Store};
use rusqlite::Connection;

const DAY: i64 = 86_400_000;
const CHUNK: i64 = 500_000;

pub struct Sizes {
    pub installs: i64,
    pub sessions: i64,
    pub events: i64,
    pub perf: i64,
    pub crashes: i64,
    pub issues: i64,
    pub items_seen: i64,
    pub versions: i64,
    pub days: i64,
}

impl Sizes {
    pub fn scaled(scale: f64) -> Self {
        let n = |full: i64| ((full as f64 * scale) as i64).max(10);
        Self {
            installs: n(200_000),
            sessions: n(1_000_000),
            events: n(10_000_000),
            perf: n(2_000_000),
            crashes: n(100_000),
            issues: n(5_000).min(n(100_000)),
            items_seen: n(12_000_000),
            versions: 50,
            days: 90,
        }
    }
}

/// A cheap deterministic hash of the row counter `i`, different for every `salt`.
fn r(salt: i64) -> String {
    format!("((((i + {salt}) * 2654435761) % 4294967296) / 16)")
}

fn insert_chunked(conn: &Connection, label: &str, total: i64, select: &dyn Fn() -> String, columns: &str, table: &str) -> Result<(), String> {
    let started = Instant::now();
    let mut lo = 1;
    while lo <= total {
        let hi = (lo + CHUNK - 1).min(total);
        let sql = format!(
            "INSERT INTO {table} ({columns}) WITH RECURSIVE n(i) AS (SELECT {lo} UNION ALL SELECT i + 1 FROM n WHERE i < {hi}) SELECT {} FROM n",
            select()
        );
        conn.execute_batch(&format!("BEGIN; {sql}; COMMIT;")).map_err(|e| format!("{label}: {e}"))?;
        lo = hi + 1;
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").map_err(|e| e.to_string())?;
    eprintln!("  {label}: {total} rows in {:.1}s", started.elapsed().as_secs_f64());
    Ok(())
}

pub fn seed(db: &Path, sizes: &Sizes, now_ms: i64) -> Result<(), String> {
    let today = now_ms / DAY;
    let first_day = today - sizes.days + 1;
    {
        let store = SqliteStore::open(db).map_err(|e| e.to_string())?;
        if store.find_app("loadgen").map_err(|e| e.to_string())?.is_none() {
            store.create_app("loadgen", "Load generator", "tk_seeded_loadgen_key", now_ms).map_err(|e| e.to_string())?;
        }
        let hash = ashlar_trace_server::cli::hash_password("seeded admin password").map_err(|e| e.to_string())?;
        let _ = store.create_admin("admin", &hash, now_ms);
    }
    let conn = Connection::open(db).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA synchronous = OFF; PRAGMA cache_size = -262144; PRAGMA temp_store = MEMORY; PRAGMA wal_autocheckpoint = 0;")
        .map_err(|e| e.to_string())?;
    // Rows are laid out in time order, as ingest writes them: a day's rows are contiguous on disk. (Random days
    // would scatter every day over the whole table and make any one-day query touch every page.)
    let day_of = |total: i64| format!("({first_day} + (i - 1) * {} / {total})", sizes.days);
    let day = |_salt: i64| day_of(sizes.sessions);
    let version = |salt: i64| format!("'1.' || ({} % {}) || '.0'", r(salt), sizes.versions);
    let inst = |salt: i64| format!("'inst' || (1 + {} % {})", r(salt), sizes.installs);
    let os = "CASE (i % 3) WHEN 0 THEN 'macos' WHEN 1 THEN 'windows' ELSE 'linux' END";

    insert_chunked(
        &conn,
        "installs",
        sizes.installs,
        &|| format!("1, 'inst' || i, {first_day} * {DAY}, {today} * {DAY}, '1.0.0', '1.{}.0', {os}, 'x86_64'", sizes.versions - 1),
        "app_id, install_id, first_seen, last_seen, first_version, last_version, os_name, arch",
        "installs",
    )?;
    insert_chunked(
        &conn,
        "sessions",
        sizes.sessions,
        &|| {
            format!(
                "1, 'sess' || i, {}, {}, 'gui', {os}, ({} * {DAY} + {} % {DAY}), {}, \
                 CASE WHEN {} % 100 < 94 THEN 'exited' WHEN {} % 100 < 97 THEN 'crashed' WHEN {} % 100 < 99 THEN 'abnormal' ELSE 'ok' END, \
                 60 + {} % 3000, 40 + {} % 2000, 30 + {} % 1500, {} % 600, ({} % 20) / 19, 'cat-1',
                 ({} * {DAY} + {} % {DAY}) / {DAY}",
                inst(1),
                version(2),
                day(3),
                r(4),
                day(3),
                r(5),
                r(5),
                r(5),
                r(6),
                r(7),
                r(8),
                r(9),
                r(10),
                day(3),
                r(4)
            )
            .replace(&format!("{}, {}, 'gui'", inst(1), version(2)), &format!("{}, {}, 'gui'", inst(1), version(2)))
        },
        "app_id, session_id, install_id, version, process, os_name, started_at, last_seen, status, duration_s, focused_s, active_s, idle_s, errors, catalog_hash, day",
        "sessions",
    )?;
    insert_chunked(
        &conn,
        "events",
        sizes.events,
        &|| {
            let d = day_of(sizes.events);
            format!(
                "1, {}, 'sess' || (1 + {} % {}), ({d} * {DAY} + {} % {DAY}), {d}, {}, 'ui.action' || printf('%02d', {} % 30), 'ui', 0, 1",
                inst(11),
                r(12),
                sizes.sessions,
                r(14),
                version(15),
                r(16)
            )
        },
        "app_id, install_id, session_id, ts, day, version, name, category, dynamic, declared",
        "events",
    )?;
    insert_chunked(
        &conn,
        "perf",
        sizes.perf,
        &|| {
            let d = day_of(sizes.perf);
            format!(
                "1, {}, 'sess' || (1 + {} % {}), ({d} * {DAY} + {} % {DAY}), {d}, {}, {os}, 60, 500, 4000000, 30000, 20, 1, \
                 '[0,5,300,0,100,50,20,10,5,3,3,2,1,1,0,0]', 20 + {} % 50, 300 + {} % 200, 600",
                inst(21),
                r(22),
                sizes.sessions,
                r(24),
                version(25),
                r(26),
                r(27)
            )
        },
        "app_id, install_id, session_id, ts, day, version, os_name, window_s, frame_count, frame_sum_us, frame_max_us, slow, frozen, buckets, cpu_pct, rss_mb, rss_peak_mb",
        "perf",
    )?;
    insert_chunked(
        &conn,
        "issues",
        sizes.issues,
        &|| {
            format!(
                "1, 'fp' || i, 'panic', 'index out of bounds in module' || (i % 97), {first_day} * {DAY}, {today} * {DAY}, '1.0.0', '1.1.0', 20, 'open', {first_day}"
            )
        },
        "app_id, fingerprint, kind, title, first_seen, last_seen, first_version, last_version, count, status, created_day",
        "issues",
    )?;
    let backtrace = (0..30).map(|n| format!("{n}: demo::module::function{n}")).collect::<Vec<_>>().join("\n");
    insert_chunked(
        &conn,
        "crashes",
        sizes.crashes,
        &|| {
            let d = day_of(sizes.crashes);
            format!(
                "1, 1 + {} % {}, {}, 'sess' || (1 + {} % {}), ({d} * {DAY} + {} % {DAY}), {d}, {}, 'macos', 'panic', 0, 'index out of bounds', '{backtrace}'",
                r(31),
                sizes.issues,
                inst(32),
                r(33),
                sizes.sessions,
                r(35),
                version(36)
            )
        },
        "app_id, issue_id, install_id, session_id, ts, day, version, os_name, kind, fatal, message, backtrace",
        "crashes",
    )?;
    // The dedup table at the size a 35-day window holds.
    insert_chunked(
        &conn,
        "items_seen",
        sizes.items_seen,
        &|| format!("1, printf('%012x', i), {} + (i - 1) * {} / {}", now_ms - 35 * DAY, 35 * DAY, sizes.items_seen),
        "app_id, item_id, received_at",
        "items_seen",
    )?;
    conn.execute_batch("PRAGMA optimize; PRAGMA wal_checkpoint(TRUNCATE);").map_err(|e| e.to_string())?;
    Ok(())
}

pub fn command(opts: &HashMap<String, String>) -> Result<(), String> {
    let db = opts.get("db").ok_or("--db PATH is required")?;
    let scale: f64 = opts.get("scale").map_or(Ok(1.0), |s| s.parse().map_err(|_| "bad --scale"))?;
    let sizes = Sizes::scaled(scale);
    eprintln!("seeding {db} at scale {scale}");
    let started = Instant::now();
    seed(Path::new(db), &sizes, ashlar_trace_server::now_ms())?;
    eprintln!("seeded in {:.1}s", started.elapsed().as_secs_f64());
    Ok(())
}
