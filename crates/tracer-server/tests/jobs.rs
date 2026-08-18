//! Rollups, retention and stale-session closing against a small seeded dataset with fixed times.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]

mod common;

use tracer_proto::{AppInfo, Device, Envelope, Event, FrameStats, Item, ItemBody, Perf, ProcessKind, SessionEnd, SessionStatus, SessionTotals};
use tracer_server::ingest::{DAY_MS, prepare};
use tracer_server::jobs;
use tracer_server::store::{SqliteStore, Store};

const DAY: i64 = 20_000;
/// Noon on day 20000.
const T: i64 = DAY * DAY_MS + DAY_MS / 2;

struct Fixture {
    store: SqliteStore,
    path: std::path::PathBuf,
    app_id: i64,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracer.db");
        let store = SqliteStore::open(&path).unwrap();
        let app_id = store.create_app("demo", "Demo", "tk_fixture", T).unwrap().id;
        Self { store, path, app_id, _dir: dir }
    }

    /// Ingests items at server time `received_at`, with the client clock agreeing.
    fn send(&self, install: &str, session: &str, os: &str, version: &str, process: ProcessKind, received_at: i64, items: Vec<(i64, ItemBody)>) {
        let items = items.into_iter().enumerate().map(|(index, (ts, body))| Item { id: format!("{session}-{index}"), ts: ts as u64, body }).collect();
        let envelope = Envelope {
            schema: 1,
            app: AppInfo { name: "demo".into(), version: version.into(), build: None, process },
            install_id: install.into(),
            session_id: session.into(),
            device: Some(Device { os_name: os.into(), arch: "x86_64".into(), ..Device::default() }),
            sent_at: received_at as u64,
            items,
        };
        self.store.ingest(self.app_id, &prepare(envelope, received_at).unwrap()).unwrap();
    }

    fn sql<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        rusqlite::Connection::open(&self.path).unwrap().query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

fn end(status: SessionStatus, duration_s: u64, errors: u32) -> ItemBody {
    ItemBody::SessionEnd(SessionEnd {
        status,
        totals: SessionTotals { seq: 1, duration_s, focused_s: duration_s / 2, active_s: duration_s / 4, idle_s: duration_s / 8 },
        errors,
    })
}

fn event(name: &str) -> ItemBody {
    ItemBody::Event(Event { name: name.into(), category: "file".into(), ..Event::default() })
}

fn perf(frames: FrameStats, cpu: f32, rss: u32, peak: u32, startup: Option<u32>) -> ItemBody {
    ItemBody::Perf(Perf {
        window_s: 60,
        frames: Some(frames),
        cpu_pct: Some(cpu),
        rss_mb: Some(rss),
        rss_peak_mb: Some(peak),
        startup_ms: startup,
        ..Perf::default()
    })
}

fn histogram(count: u64, sum_us: u64, max_us: u32, slow: u64, frozen: u64, filled: &[(usize, u64)]) -> FrameStats {
    let mut stats = FrameStats::empty();
    stats.count = count;
    stats.sum_us = sum_us;
    stats.max_us = max_us;
    stats.slow = slow;
    stats.frozen = frozen;
    for (index, value) in filled {
        stats.buckets[*index] = *value;
    }
    stats
}

fn seed(fx: &Fixture) {
    use ProcessKind::{Cli, Gui};
    let s = T - 3_600_000;
    // Install A on macOS, version 1.0: one clean session and one crash.
    fx.send(
        "A",
        "A1",
        "macos",
        "1.0",
        Gui,
        T,
        vec![
            (s, event("file.open")),
            (s + 1, event("file.open")),
            (s + 2, event("edit.undo")),
            (s + 3, perf(histogram(10, 100_000, 30_000, 2, 0, &[(0, 3), (6, 5), (8, 2)]), 10.0, 100, 120, Some(500))),
            (s + 4, perf(histogram(5, 90_000, 800_000, 3, 1, &[(6, 2), (12, 2), (13, 1)]), 30.0, 200, 250, None)),
            (s + 5, end(SessionStatus::Exited, 600, 0)),
        ],
    );
    fx.send("A", "A2", "macos", "1.0", Gui, T, vec![(s + 10, event("edit.undo")), (s + 11, end(SessionStatus::Crashed, 60, 2))]);
    // Install B on Windows, version 1.0: a cli session (not an open) and, the day before, an abnormal one.
    fx.send(
        "B",
        "B1",
        "windows",
        "1.0",
        Cli,
        T,
        vec![
            (s, event("file.open")),
            (s + 1, perf(histogram(1, 5_000, 5_000, 0, 0, &[(3, 1)]), 5.0, 50, 60, Some(100))),
            (s + 2, end(SessionStatus::Exited, 30, 1)),
        ],
    );
    fx.send("B", "B0", "windows", "1.0", Gui, T, vec![(s - DAY_MS, end(SessionStatus::Abnormal, 90, 0))]);
    // Install C on Linux, version 1.1: still running.
    fx.send(
        "C",
        "C1",
        "linux",
        "1.1",
        Gui,
        T,
        vec![(s, ItemBody::SessionHeartbeat(SessionTotals { seq: 1, duration_s: 20, focused_s: 20, active_s: 10, idle_s: 0 }))],
    );
}

#[test]
fn rollup_numbers_are_exact() {
    let fx = Fixture::new();
    seed(&fx);
    jobs::rollup_recent(&fx.store, T, 3, 90).unwrap();

    // Sessions on day D, per (version, os).
    let row = |version: &str, os: &str| -> Vec<i64> {
        rusqlite::Connection::open(&fx.path)
            .unwrap()
            .query_row(
                "SELECT sessions, opens, installs, exited, crashed, abnormal, errored, crashed_installs, duration_s, focused_s, active_s, idle_s
                 FROM daily_sessions WHERE day = ?1 AND version = ?2 AND os_name = ?3",
                rusqlite::params![DAY, version, os],
                |row| (0..12).map(|i| row.get(i)).collect(),
            )
            .unwrap()
    };
    // A1 exited (600s), A2 crashed (60s, 2 errors); both gui; one install, which had a crash.
    assert_eq!(row("1.0", "macos"), vec![2, 2, 1, 1, 1, 0, 1, 1, 660, 330, 165, 82]);
    // B1 is a cli session: counted as a session, not as an open; it ended well and reported an error.
    assert_eq!(row("1.0", "windows"), vec![1, 0, 1, 1, 0, 0, 1, 0, 30, 15, 7, 3]);
    // C1 never ended.
    assert_eq!(row("1.1", "linux"), vec![1, 1, 1, 0, 0, 0, 0, 0, 20, 20, 10, 0]);
    // The day before: B0, abnormal.
    let before: Vec<i64> = rusqlite::Connection::open(&fx.path)
        .unwrap()
        .query_row("SELECT sessions, abnormal, crashed_installs FROM daily_sessions WHERE day = ?1", [DAY - 1], |row| {
            Ok(vec![row.get(0)?, row.get(1)?, row.get(2)?])
        })
        .unwrap();
    assert_eq!(before, vec![1, 1, 1]);

    // Crash-free inputs for the day, across all rows: ended = exited + crashed + abnormal.
    let (ended, bad, active_installs): (i64, i64, i64) = (
        fx.sql("SELECT sum(exited + crashed + abnormal) FROM daily_sessions WHERE day = 20000"),
        fx.sql("SELECT sum(crashed + abnormal) FROM daily_sessions WHERE day = 20000"),
        fx.sql("SELECT count(*) FROM daily_installs WHERE day = 20000"),
    );
    assert_eq!((ended, bad, active_installs), (3, 1, 3));
    assert_eq!(fx.sql::<i64>("SELECT sum(crashed_installs) FROM daily_sessions WHERE day = 20000"), 1);
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM daily_installs WHERE day = 19999"), 1);

    // Events.
    let event_row = |name: &str| -> (i64, i64) {
        rusqlite::Connection::open(&fx.path)
            .unwrap()
            .query_row(
                "SELECT count, installs FROM daily_events WHERE day = ?1 AND version = '1.0' AND name = ?2",
                rusqlite::params![DAY, name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    };
    assert_eq!(event_row("file.open"), (3, 2));
    assert_eq!(event_row("edit.undo"), (2, 1));
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM daily_events"), 2);

    // Performance: histograms of A's two windows merge bucket by bucket.
    let (frames, sum, max, slow, frozen, buckets, cpu_sum, cpu_n, rss_sum, rss_n, peak, startup_sum, startup_n): (i64, i64, i64, i64, i64, String, f64, i64, i64, i64, i64, i64, i64) =
        rusqlite::Connection::open(&fx.path)
            .unwrap()
            .query_row(
                "SELECT frame_count, frame_sum_us, frame_max_us, slow, frozen, buckets, cpu_sum, cpu_n, rss_sum, rss_n, rss_peak_mb, startup_sum, startup_n
                 FROM daily_perf WHERE day = ?1 AND version = '1.0' AND os_name = 'macos'",
                [DAY],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?, row.get(12)?)),
            )
            .unwrap();
    assert_eq!((frames, sum, max, slow, frozen), (15, 190_000, 800_000, 5, 1));
    let buckets: Vec<u64> = serde_json::from_str(&buckets).unwrap();
    assert_eq!(buckets, vec![3, 0, 0, 0, 0, 0, 7, 0, 2, 0, 0, 0, 2, 1, 0, 0]);
    assert_eq!((cpu_sum, cpu_n, rss_sum, rss_n, peak, startup_sum, startup_n), (40.0, 2, 300, 2, 250, 500, 1));
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM daily_perf"), 2);

    // Recomputing changes nothing.
    jobs::rollup_recent(&fx.store, T, 3, 90).unwrap();
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM daily_sessions"), 4);
    assert_eq!(fx.sql::<i64>("SELECT sum(sessions) FROM daily_sessions"), 5);
}

#[test]
fn drops_survive_a_rollup() {
    let fx = Fixture::new();
    let drops = ItemBody::Drops(tracer_proto::Drops { reasons: [("queue_full".to_string(), 7)].into() });
    fx.send("A", "A1", "macos", "1.0", ProcessKind::Gui, T, vec![(T - 10, drops)]);
    jobs::rollup_recent(&fx.store, T, 3, 90).unwrap();
    assert_eq!(fx.sql::<i64>("SELECT count FROM daily_drops WHERE reason = 'queue_full'"), 7);
}

#[test]
fn prune_removes_old_raw_rows_and_keeps_rollups() {
    let fx = Fixture::new();
    let old_received = T - 100 * DAY_MS;
    fx.send(
        "old",
        "O1",
        "macos",
        "0.9",
        ProcessKind::Gui,
        old_received,
        vec![(old_received - 10, event("file.open")), (old_received - 5, end(SessionStatus::Exited, 10, 0))],
    );
    seed(&fx);
    // Roll up the old day while its raw rows still exist, then everything recent.
    fx.store.rollup(DAY - 100, DAY - 100).unwrap();
    jobs::rollup_recent(&fx.store, T, 3, 90).unwrap();
    assert_eq!(fx.sql::<i64>("SELECT sessions FROM daily_sessions WHERE version = '0.9'"), 1);

    let stats = jobs::run_retention(&fx.store, T, 90).unwrap();
    assert_eq!((stats.events, stats.sessions), (1, 1));
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM events WHERE name = 'file.open' AND version = '0.9'"), 0);
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM sessions WHERE version = '0.9'"), 0);
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM installs WHERE install_id = 'old'"), 0);
    // Recent raw rows and every rollup row are still there.
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM events"), 5);
    assert_eq!(fx.sql::<i64>("SELECT sessions FROM daily_sessions WHERE version = '0.9'"), 1);
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM daily_events WHERE version = '0.9'"), 1);
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM daily_installs WHERE install_id = 'old'"), 1);

    // A long rollup window is clamped to retention, so the kept rollup is not erased.
    jobs::rollup_recent(&fx.store, T, 400, 90).unwrap();
    assert_eq!(fx.sql::<i64>("SELECT sessions FROM daily_sessions WHERE version = '0.9'"), 1);
}

#[test]
fn old_dedup_ids_are_pruned() {
    let fx = Fixture::new();
    let old = T - 40 * DAY_MS;
    fx.send("A", "A1", "macos", "1.0", ProcessKind::Gui, old, vec![(old, event("a"))]);
    fx.send("A", "A2", "macos", "1.0", ProcessKind::Gui, T, vec![(T, event("a"))]);
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM items_seen"), 2);
    jobs::run_retention(&fx.store, T, 90).unwrap();
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM items_seen"), 1);
}

#[test]
fn stale_ok_sessions_are_closed_as_abnormal() {
    let fx = Fixture::new();
    let beat = || ItemBody::SessionHeartbeat(SessionTotals { seq: 1, duration_s: 5, ..SessionTotals::default() });
    fx.send("A", "stale", "macos", "1.0", ProcessKind::Gui, T, vec![(T - 2 * DAY_MS, beat())]);
    fx.send("A", "fresh", "macos", "1.0", ProcessKind::Gui, T, vec![(T - 3_600_000, beat())]);
    fx.send("A", "done", "macos", "1.0", ProcessKind::Gui, T, vec![(T - 3 * DAY_MS, end(SessionStatus::Exited, 5, 0))]);
    assert_eq!(jobs::close_stale_sessions(&fx.store, T).unwrap(), 1);
    let status = |id: &str| fx.sql::<String>(&format!("SELECT status FROM sessions WHERE session_id = '{id}'"));
    assert_eq!(status("stale"), "abnormal");
    assert_eq!(status("fresh"), "ok");
    assert_eq!(status("done"), "exited");
    assert_eq!(jobs::close_stale_sessions(&fx.store, T).unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_timers_roll_up_and_close_stale_sessions_on_their_own() {
    let fx = Fixture::new();
    seed(&fx);
    // A clock three days ahead makes the session that never ended stale; the fast timers fire at once.
    let later = T + 3 * DAY_MS;
    let store: std::sync::Arc<dyn Store> = std::sync::Arc::new(SqliteStore::open(&fx.path).unwrap());
    let timers = jobs::Timers {
        rollup_every: std::time::Duration::from_millis(20),
        retention_every: std::time::Duration::from_millis(20),
        clock: std::sync::Arc::new(move || later),
    };
    let handles = jobs::spawn_timers_with(store, 90, timers);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let rolled: i64 = fx.sql("SELECT count(*) FROM daily_sessions");
        let closed: i64 = fx.sql("SELECT count(*) FROM sessions WHERE session_id = 'C1' AND status = 'abnormal'");
        let dirty: i64 = fx.sql("SELECT count(*) FROM dirty_days");
        if rolled > 0 && closed == 1 && dirty == 0 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "the timers did nothing: rolled {rolled}, closed {closed}, dirty {dirty}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    for handle in handles {
        handle.abort();
    }
    // The stale close changed the totals, so the rollup of that day shows the abnormal session.
    assert_eq!(fx.sql::<i64>("SELECT sum(abnormal) FROM daily_sessions WHERE day = 20000"), 1);
}

#[test]
fn a_rollup_tick_recomputes_only_the_days_that_received_data() {
    let fx = Fixture::new();
    seed(&fx);
    assert_eq!(jobs::rollup_tick(&fx.store, T, 90).unwrap(), 2, "days 19999 and 20000");
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM dirty_days"), 0);
    assert_eq!(jobs::rollup_tick(&fx.store, T, 90).unwrap(), 0, "nothing new, nothing to do");
    // Late data for an old day marks just that day.
    let old = T - 30 * DAY_MS;
    fx.send("Z", "Z1", "macos", "1.0", ProcessKind::Gui, T, vec![(old, event("file.open")), (old + 1, end(SessionStatus::Exited, 5, 0))]);
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM dirty_days"), 1);
    assert_eq!(jobs::rollup_pending(&fx.store, T, 90).unwrap(), 1);
    assert_eq!(fx.sql::<i64>("SELECT sessions FROM daily_sessions WHERE day = 19970"), 1);
    // A forced recompute of everything gives the same numbers.
    let before = fx.sql::<i64>("SELECT sum(sessions) FROM daily_sessions");
    jobs::rollup_recent(&fx.store, T, 90, 90).unwrap();
    assert_eq!(fx.sql::<i64>("SELECT sum(sessions) FROM daily_sessions"), before);
    // Marks for days beyond retention are dropped rather than recomputed.
    fx.send("Y", "Y1", "macos", "1.0", ProcessKind::Gui, T, vec![(T - 85 * DAY_MS, event("file.open"))]);
    assert_eq!(jobs::rollup_pending(&fx.store, T + 20 * DAY_MS, 90).unwrap(), 0);
    assert_eq!(fx.sql::<i64>("SELECT count(*) FROM dirty_days"), 0);
}

#[test]
fn weekly_and_monthly_actives_are_stored_per_day() {
    let fx = Fixture::new();
    // One new install per day for ten days; each also sends an event so it counts as active.
    for n in 0..10 {
        let at = T - (9 - n) * DAY_MS;
        fx.send(&format!("inst{n}"), &format!("s{n}"), "macos", "1.0", ProcessKind::Gui, at, vec![(at, event("file.open"))]);
    }
    jobs::rollup_pending(&fx.store, T, 90).unwrap();
    let row = |day: i64| -> (i64, i64, i64) {
        rusqlite::Connection::open(&fx.path)
            .unwrap()
            .query_row("SELECT dau, wau, mau FROM daily_active WHERE day = ?1", [day], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
    };
    assert_eq!(row(DAY), (1, 7, 10), "today: 1 today, 7 in the last 7 days, all 10 in the last 30");
    // Older days keep only their daily figure; the weekly and monthly ones are left "not computed" (-1).
    assert_eq!(row(DAY - 1).0, 1, "yesterday is still among the newest two");
    assert_eq!(row(DAY - 3), (1, -1, -1));
    // The dashboard still gets the right answer for them by counting.
    assert_eq!(fx.store.active_installs_between(fx.app_id, DAY - 9, DAY - 3, None).unwrap(), 7);
}
