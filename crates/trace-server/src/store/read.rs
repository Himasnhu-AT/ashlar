//! Read queries for the dashboard. Every statement is scoped by `app_id`, takes its inputs as bound
//! parameters, and ends in a `LIMIT`. Anything over time reads the `daily_*` rollups, which outlive
//! retention; raw tables are read only for detail views and recent rows.

use ashlar_trace_proto::{FRAME_BUCKETS, FrameStats};
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::types::{
    AppGlance, Breakdown, CatalogEntry, CrashBrief, CrashRow, CustomRow, DayTotals, DropRow, EventRow, IssueFilter, IssueRow, IssueSort, PerfRow,
    VersionInfo,
};

pub const MAX_DAYS: i64 = 800;
pub const MAX_VERSIONS: i64 = 200;
pub const MAX_PERF_ROWS: i64 = 20_000;
pub const MAX_EVENTS: i64 = 200;
pub const MAX_DROPS: i64 = 50;

pub fn day_totals(conn: &Connection, app_id: i64, from: i64, to: i64, version: Option<&str>) -> rusqlite::Result<Vec<DayTotals>> {
    let mut stmt = conn.prepare_cached(
        "SELECT day, sum(sessions), sum(opens), sum(installs), sum(exited), sum(crashed), sum(abnormal), sum(errored), sum(crashed_installs),
                sum(duration_s), sum(focused_s), sum(active_s), sum(idle_s)
         FROM daily_sessions
         WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3 AND (?4 IS NULL OR version = ?4)
         GROUP BY day ORDER BY day LIMIT ?5",
    )?;
    stmt.query_map(params![app_id, from, to, version, MAX_DAYS], |row| {
        Ok(DayTotals {
            day: row.get(0)?,
            sessions: row.get(1)?,
            opens: row.get(2)?,
            installs: row.get(3)?,
            exited: row.get(4)?,
            crashed: row.get(5)?,
            abnormal: row.get(6)?,
            errored: row.get(7)?,
            crashed_installs: row.get(8)?,
            duration_s: row.get(9)?,
            focused_s: row.get(10)?,
            active_s: row.get(11)?,
            idle_s: row.get(12)?,
        })
    })?
    .collect()
}

pub fn versions(conn: &Connection, app_id: i64) -> rusqlite::Result<Vec<VersionInfo>> {
    let mut stmt = conn.prepare_cached(
        "SELECT version, min(day), max(day), sum(sessions) FROM daily_sessions WHERE app_id = ?1
         GROUP BY version ORDER BY min(day), version LIMIT ?2",
    )?;
    stmt.query_map(params![app_id, MAX_VERSIONS], |row| {
        Ok(VersionInfo { version: row.get(0)?, first_day: row.get(1)?, last_day: row.get(2)?, sessions: row.get(3)? })
    })?
    .collect()
}

/// Distinct installs per day. Without a version this is the install rollup; with one it is read from raw
/// sessions, since the install rollup does not carry versions.
pub fn active_installs_by_day(conn: &Connection, app_id: i64, from: i64, to: i64, version: Option<&str>) -> rusqlite::Result<Vec<(i64, i64)>> {
    let sql = if version.is_some() {
        "SELECT day, count(DISTINCT install_id) FROM sessions WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3 AND version = ?4
         GROUP BY day ORDER BY day LIMIT ?5"
    } else {
        "SELECT day, dau FROM daily_active WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3 AND ?4 IS NULL ORDER BY day LIMIT ?5"
    };
    let mut stmt = conn.prepare_cached(sql)?;
    stmt.query_map(params![app_id, from, to, version, MAX_DAYS], |row| Ok((row.get(0)?, row.get(1)?)))?.collect()
}

pub fn active_installs_between(conn: &Connection, app_id: i64, from: i64, to: i64, version: Option<&str>) -> rusqlite::Result<i64> {
    // Daily, weekly and monthly actives ending on `to` are stored by the rollup.
    if version.is_none() {
        let column = match to - from {
            0 => Some("dau"),
            6 => Some("wau"),
            29 => Some("mau"),
            _ => None,
        };
        if let Some(column) = column {
            let stored: Option<i64> = conn
                .prepare_cached(&format!("SELECT {column} FROM daily_active WHERE app_id = ?1 AND day = ?2"))?
                .query_row(params![app_id, to], |row| row.get(0))
                .optional()?;
            // A negative figure means the rollup did not compute it for that (older) day.
            if let Some(stored) = stored.filter(|stored| *stored >= 0) {
                return Ok(stored);
            }
        }
    }
    let sql = if version.is_some() {
        "SELECT count(DISTINCT install_id) FROM sessions WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3 AND version = ?4"
    } else {
        "SELECT count(DISTINCT install_id) FROM daily_installs WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3 AND ?4 IS NULL"
    };
    conn.prepare_cached(sql)?.query_row(params![app_id, from, to, version], |row| row.get(0))
}

const ISSUE_COLUMNS: &str = "i.id, i.kind, i.title, i.first_seen, i.last_seen, i.first_version, i.last_version, i.count, i.status,
        (SELECT count(DISTINCT c.install_id) FROM crashes c WHERE c.issue_id = i.id),
        (SELECT group_concat(DISTINCT c.version) FROM crashes c WHERE c.issue_id = i.id)";

fn issue_row(row: &Row<'_>) -> rusqlite::Result<IssueRow> {
    let versions: Option<String> = row.get(10)?;
    Ok(IssueRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        first_seen: row.get(3)?,
        last_seen: row.get(4)?,
        first_version: row.get(5)?,
        last_version: row.get(6)?,
        count: row.get(7)?,
        status: row.get(8)?,
        installs: row.get(9)?,
        versions: versions.map(|text| text.split(',').take(12).map(str::to_string).collect()).unwrap_or_default(),
    })
}

pub fn list_issues(conn: &Connection, app_id: i64, filter: &IssueFilter<'_>, limit: i64) -> rusqlite::Result<Vec<IssueRow>> {
    let order = match filter.sort {
        IssueSort::Recent => "i.last_seen DESC, i.id DESC",
        IssueSort::Count => "i.count DESC, i.last_seen DESC, i.id DESC",
    };
    // The per-issue counts are correlated subqueries over `crashes`; they must run only for the rows that are
    // returned, so the filtering, ordering and limit happen in an inner query first.
    let sql = format!(
        "SELECT {ISSUE_COLUMNS} FROM (
             SELECT * FROM issues i
             WHERE i.app_id = ?1 AND (?2 IS NULL OR i.kind = ?2) AND (?3 IS NULL OR i.status = ?3) AND i.last_seen >= ?4
               AND (?5 IS NULL OR EXISTS (SELECT 1 FROM crashes c WHERE c.issue_id = i.id AND c.version = ?5))
             ORDER BY {order} LIMIT ?6) AS i
         ORDER BY {order}"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    stmt.query_map(params![app_id, filter.kind, filter.status, filter.since_ms, filter.version, limit.clamp(1, 500)], issue_row)?.collect()
}

pub fn get_issue(conn: &Connection, app_id: i64, id: i64) -> rusqlite::Result<Option<IssueRow>> {
    let sql = format!("SELECT {ISSUE_COLUMNS} FROM issues i WHERE i.app_id = ?1 AND i.id = ?2 LIMIT 1");
    conn.prepare_cached(&sql)?.query_row(params![app_id, id], issue_row).optional()
}

fn crash_row(row: &Row<'_>) -> rusqlite::Result<CrashRow> {
    Ok(CrashRow {
        id: row.get(0)?,
        ts: row.get(1)?,
        install_id: row.get(2)?,
        session_id: row.get(3)?,
        version: row.get(4)?,
        build: row.get(5)?,
        os_name: row.get(6)?,
        os_version: row.get(7)?,
        arch: row.get(8)?,
        kind: row.get(9)?,
        fatal: row.get::<_, i64>(10)? != 0,
        message: row.get(11)?,
        location: row.get(12)?,
        thread: row.get(13)?,
        backtrace: row.get(14)?,
        blocked_ms: row.get(15)?,
        exit_code: row.get(16)?,
        signal: row.get(17)?,
        context: row.get(18)?,
        breadcrumbs: row.get(19)?,
        perf: row.get(20)?,
        device: row.get(21)?,
    })
}

/// The crash shown as the issue's example: the newest one that carries a backtrace, else the newest.
pub fn representative_crash(conn: &Connection, app_id: i64, issue_id: i64) -> rusqlite::Result<Option<CrashRow>> {
    conn.prepare_cached(
        "SELECT id, ts, install_id, session_id, version, build, os_name, os_version, arch, kind, fatal, message, location, thread, backtrace,
                blocked_ms, exit_code, signal, context, breadcrumbs, perf, device
         FROM crashes WHERE app_id = ?1 AND issue_id = ?2
         ORDER BY (backtrace IS NOT NULL AND backtrace != '') DESC, ts DESC, id DESC LIMIT 1",
    )?
    .query_row(params![app_id, issue_id], crash_row)
    .optional()
}

pub fn recent_crashes(conn: &Connection, app_id: i64, issue_id: i64, limit: i64) -> rusqlite::Result<Vec<CrashBrief>> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, ts, version, os_name, os_version, install_id, fatal FROM crashes WHERE app_id = ?1 AND issue_id = ?2
         ORDER BY ts DESC, id DESC LIMIT ?3",
    )?;
    stmt.query_map(params![app_id, issue_id, limit.clamp(1, 200)], |row| {
        Ok(CrashBrief {
            id: row.get(0)?,
            ts: row.get(1)?,
            version: row.get(2)?,
            os_name: row.get(3)?,
            os_version: row.get(4)?,
            install_id: row.get(5)?,
            fatal: row.get::<_, i64>(6)? != 0,
        })
    })?
    .collect()
}

pub fn issue_breakdown(conn: &Connection, app_id: i64, issue_id: i64) -> rusqlite::Result<Breakdown> {
    let versions = conn
        .prepare_cached(
            "SELECT version, count(*) FROM crashes WHERE app_id = ?1 AND issue_id = ?2 GROUP BY version ORDER BY count(*) DESC, version LIMIT 30",
        )?
        .query_map(params![app_id, issue_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
    let os = conn
        .prepare_cached(
            "SELECT COALESCE(os_name, 'unknown') || COALESCE(' ' || os_version, ''), count(*) FROM crashes WHERE app_id = ?1 AND issue_id = ?2
             GROUP BY 1 ORDER BY count(*) DESC, 1 LIMIT 30",
        )?
        .query_map(params![app_id, issue_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
    Ok(Breakdown { versions, os })
}

pub fn set_issue_status(conn: &Connection, app_id: i64, id: i64, status: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute("UPDATE issues SET status = ?3 WHERE app_id = ?1 AND id = ?2", params![app_id, id, status])? > 0)
}

pub fn perf_rows(conn: &Connection, app_id: i64, from: i64, to: i64, version: Option<&str>) -> rusqlite::Result<Vec<PerfRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT day, version, os_name, frame_count, frame_sum_us, frame_max_us, slow, frozen, buckets,
                cpu_sum, cpu_n, rss_sum, rss_n, rss_peak_mb, startup_sum, startup_n
         FROM daily_perf WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3 AND (?4 IS NULL OR version = ?4)
         ORDER BY day, version, os_name LIMIT ?5",
    )?;
    stmt.query_map(params![app_id, from, to, version, MAX_PERF_ROWS], |row| {
        let buckets: String = row.get(8)?;
        let mut buckets: Vec<u64> = serde_json::from_str(&buckets).unwrap_or_default();
        buckets.resize(FRAME_BUCKETS, 0);
        let non_negative = |value: i64| u64::try_from(value).unwrap_or(0);
        Ok(PerfRow {
            day: row.get(0)?,
            version: row.get(1)?,
            os_name: row.get(2)?,
            frames: FrameStats {
                count: non_negative(row.get(3)?),
                sum_us: non_negative(row.get(4)?),
                max_us: u32::try_from(row.get::<_, i64>(5)?.max(0)).unwrap_or(u32::MAX),
                slow: non_negative(row.get(6)?),
                frozen: non_negative(row.get(7)?),
                buckets,
            },
            cpu_sum: row.get(9)?,
            cpu_n: row.get(10)?,
            rss_sum: row.get(11)?,
            rss_n: row.get(12)?,
            rss_peak_mb: row.get(13)?,
            startup_sum: row.get(14)?,
            startup_n: row.get(15)?,
        })
    })?
    .collect()
}

pub fn recent_custom_metrics(conn: &Connection, app_id: i64, since_day: i64, version: Option<&str>, limit: i64) -> rusqlite::Result<Vec<CustomRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT counters, gauges FROM perf
         WHERE app_id = ?1 AND day >= ?2 AND (?3 IS NULL OR version = ?3) AND (counters IS NOT NULL OR gauges IS NOT NULL)
         ORDER BY ts DESC LIMIT ?4",
    )?;
    stmt.query_map(params![app_id, since_day, version, limit.clamp(1, 5000)], |row| Ok(CustomRow { counters: row.get(0)?, gauges: row.get(1)? }))?
        .collect()
}

pub fn events_ranked(conn: &Connection, app_id: i64, from: i64, to: i64, version: Option<&str>, declared: bool) -> rusqlite::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT name, min(category), sum(count), sum(installs) FROM daily_events
         WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3 AND (?4 IS NULL OR version = ?4) AND declared = ?6
         GROUP BY name ORDER BY sum(count) DESC, name LIMIT ?5",
    )?;
    stmt.query_map(params![app_id, from, to, version, MAX_EVENTS, declared], |row| {
        Ok(EventRow { name: row.get(0)?, category: row.get(1)?, count: row.get(2)?, installs: row.get(3)? })
    })?
    .collect()
}

/// Declared events that have no rollup row on any day, from the catalog most installs of a version reported:
/// for each version, the catalog hash named by the most distinct installs (ties go to the smaller hash). A
/// catalog only one hostile client sent therefore cannot flood the list, which is also capped.
pub fn never_fired(conn: &Connection, app_id: i64, version: Option<&str>) -> rusqlite::Result<Vec<CatalogEntry>> {
    let mut stmt = conn.prepare_cached(
        "WITH dominant AS (
             SELECT version, hash FROM (
                 SELECT version, catalog_hash AS hash,
                        ROW_NUMBER() OVER (PARTITION BY version ORDER BY count(DISTINCT install_id) DESC, catalog_hash) AS rank
                 FROM sessions
                 WHERE app_id = ?1 AND catalog_hash IS NOT NULL AND (?2 IS NULL OR version = ?2)
                 GROUP BY version, catalog_hash)
             WHERE rank = 1)
         SELECT ce.name, min(ce.category) FROM catalog_events ce
         JOIN dominant d ON d.version = ce.version AND d.hash = ce.hash
         WHERE ce.app_id = ?1
           AND NOT EXISTS (SELECT 1 FROM daily_events de WHERE de.app_id = ce.app_id AND de.name = ce.name)
         GROUP BY ce.name ORDER BY ce.name LIMIT ?3",
    )?;
    stmt.query_map(params![app_id, version, MAX_EVENTS], |row| Ok(CatalogEntry { name: row.get(0)?, category: row.get(1)? }))?.collect()
}

pub fn drops(conn: &Connection, app_id: i64, from: i64, to: i64) -> rusqlite::Result<Vec<DropRow>> {
    let mut stmt = conn.prepare_cached(
        "SELECT reason, sum(min(count, 1000000000000)) FROM daily_drops WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3
         GROUP BY reason ORDER BY 2 DESC, reason LIMIT ?4",
    )?;
    stmt.query_map(params![app_id, from, to, MAX_DROPS], |row| Ok(DropRow { reason: row.get(0)?, count: row.get(1)? }))?.collect()
}

pub fn app_glance(conn: &Connection, app_id: i64, from: i64, to: i64) -> rusqlite::Result<AppGlance> {
    let opens: i64 = conn
        .prepare_cached("SELECT COALESCE(sum(opens), 0) FROM daily_sessions WHERE app_id = ?1 AND day BETWEEN ?2 AND ?3")?
        .query_row(params![app_id, from, to], |row| row.get(0))?;
    let open_issues: i64 =
        conn.prepare_cached("SELECT count(*) FROM issues WHERE app_id = ?1 AND status = 'open'")?.query_row(params![app_id], |row| row.get(0))?;
    Ok(AppGlance { opens, open_issues })
}
