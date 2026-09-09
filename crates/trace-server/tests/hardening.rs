//! Tests for the security-review fixes: each one pins an attack or a bug that used to work.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]

mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use ashlar_trace_proto::{
    Catalog, CatalogEvent, Crash, CrashKind, Drops, Event, Item, ItemBody, Perf, SessionEnd, SessionStart, SessionStatus, SessionTotals,
};
use ashlar_trace_server::config::Config;
use ashlar_trace_server::http::ServerLimits;
use ashlar_trace_server::ingest::{DAY_MS, prepare};
use ashlar_trace_server::store::Store;
use common::{TestServer, envelope, item, now};

fn totals(duration_s: u64) -> SessionTotals {
    SessionTotals { seq: 1, duration_s, focused_s: duration_s, active_s: duration_s / 2, idle_s: 0 }
}

fn event(id: &str, name: &str) -> Item {
    item(id, ItemBody::Event(Event { name: name.into(), category: "file".into(), ..Event::default() }))
}

fn panic_crash(id: &str, ts_offset_ms: u64, stack: &str) -> Item {
    Item {
        id: id.into(),
        ts: now() - ts_offset_ms,
        body: ItemBody::Crash(Crash {
            kind: CrashKind::Panic,
            fatal: false,
            message: Some("boom".into()),
            backtrace: Some(stack.into()),
            ..Crash::default()
        }),
    }
}

fn start_with(id: &str, hash: &str, names: Option<&[&str]>) -> Item {
    let catalog =
        names.map(|names| Catalog { events: names.iter().map(|name| CatalogEvent { name: (*name).into(), category: "file".into() }).collect() });
    item(id, ItemBody::SessionStart(SessionStart { catalog_hash: Some(hash.into()), catalog, ..SessionStart::default() }))
}

fn end(id: &str, status: SessionStatus) -> Item {
    item(id, ItemBody::SessionEnd(SessionEnd { status, totals: totals(10), errors: 0 }))
}

// ---------------------------------------------------------------------------------------------------------
// E1, E2: sessions

#[test]
fn items_without_a_session_item_create_no_session_row() {
    let server = TestServer::demo();
    let perf = item("p", ItemBody::Perf(Perf { window_s: 60, ..Perf::default() }));
    let mut crash = panic_crash("c", 500, "0: demo::main");
    if let ItemBody::Crash(inner) = &mut crash.body {
        inner.fatal = true;
    }
    let reply = server.ingest(&envelope("opted-out", "s-phantom", vec![event("e", "file.open"), perf, crash]));
    assert_eq!(reply.counts().0, 3, "{}", reply.body);
    assert_eq!(server.count("SELECT count(*) FROM sessions"), 0, "no session row without a session item");
    // The ids are kept as plain text on the rows themselves.
    assert_eq!(server.text("SELECT session_id FROM crashes"), "s-phantom");
    assert_eq!(server.text("SELECT session_id FROM events"), "s-phantom");
    // The stale closer and the rollups therefore have nothing to count.
    let later = ashlar_trace_server::now_ms() + 3 * DAY_MS;
    assert_eq!(ashlar_trace_server::jobs::close_stale_sessions(server.store.as_ref(), later).unwrap(), 0);
    ashlar_trace_server::jobs::rollup_recent(server.store.as_ref(), ashlar_trace_server::now_ms(), 3, 90).unwrap();
    assert_eq!(server.count("SELECT count(*) FROM daily_sessions"), 0);
    // The install still counts as active for the day (it sent events), which is what the rollup promises.
    assert_eq!(server.count("SELECT count(*) FROM daily_installs"), 1);
    // A later session item for the same session creates the row then.
    server.ingest(&envelope("opted-out", "s-phantom", vec![item("h", ItemBody::SessionHeartbeat(totals(5)))]));
    assert_eq!(server.count("SELECT count(*) FROM sessions"), 1);
}

#[test]
fn a_session_that_ends_crashed_makes_its_last_panic_fatal_in_either_order() {
    let server = TestServer::demo();
    let fatal = |session: &str| -> Vec<i64> {
        let conn = rusqlite::Connection::open(&server.db_path).unwrap();
        let mut stmt = conn.prepare("SELECT fatal FROM crashes WHERE session_id = ?1 ORDER BY ts, id").unwrap();
        stmt.query_map([session], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
    };

    // Crash first, then the end, in separate envelopes. Only the most recent panic is marked.
    server.ingest(&envelope(
        "i",
        "a",
        vec![
            item("a-h", ItemBody::SessionHeartbeat(totals(1))),
            panic_crash("a-c1", 5000, "0: demo::one"),
            panic_crash("a-c2", 4000, "0: demo::two"),
        ],
    ));
    assert_eq!(fatal("a"), [0, 0]);
    server.ingest(&envelope("i", "a", vec![end("a-e", SessionStatus::Crashed)]));
    assert_eq!(fatal("a"), [0, 1]);

    // The end first, the crash after.
    server.ingest(&envelope("i", "b", vec![item("b-h", ItemBody::SessionHeartbeat(totals(1))), end("b-e", SessionStatus::Crashed)]));
    server.ingest(&envelope("i", "b", vec![panic_crash("b-c", 3000, "0: demo::three")]));
    assert_eq!(fatal("b"), [1]);

    // A crash with no session row at all is handled.
    server.ingest(&envelope("i", "ghost", vec![panic_crash("g-c", 3000, "0: demo::four")]));
    assert_eq!(fatal("ghost"), [0]);

    // A session that exited cleanly keeps its panic non-fatal (a handled panic, say), and other kinds are untouched.
    let hang = Item { id: "c-hang".into(), ts: now() - 2000, body: ItemBody::Crash(Crash { kind: CrashKind::Hang, ..Crash::default() }) };
    server.ingest(&envelope(
        "i",
        "c",
        vec![item("c-h", ItemBody::SessionHeartbeat(totals(1))), panic_crash("c-c", 3000, "0: demo::five"), hang, end("c-e", SessionStatus::Exited)],
    ));
    assert_eq!(fatal("c"), [0, 0]);
    let crashed_hang = envelope(
        "i",
        "d",
        vec![
            item("d-h", ItemBody::SessionHeartbeat(totals(1))),
            Item { id: "d-hang".into(), ts: now() - 2000, body: ItemBody::Crash(Crash { kind: CrashKind::Hang, ..Crash::default() }) },
            end("d-e", SessionStatus::Crashed),
        ],
    );
    server.ingest(&crashed_hang);
    assert_eq!(fatal("d"), [0], "only panics are marked");
}

// ---------------------------------------------------------------------------------------------------------
// H1: catalogs

#[test]
fn a_forged_catalog_cannot_replace_the_real_one_or_cause_data_loss() {
    let server = TestServer::demo();
    // The real client declares its events.
    let real = server.ingest(&envelope("honest", "s1", vec![start_with("s1-start", "real-hash", Some(&["file.open", "file.save"]))]));
    assert_eq!(real.counts(), (1, 0, 0, false));
    // The write key is public: an attacker posts a different catalog for the same version.
    let forged = server.ingest(&envelope("attacker", "x1", vec![start_with("x1-start", "forged-hash", Some(&["only.the.attacker"]))]));
    assert_eq!(forged.status, 200);
    assert_eq!(server.count("SELECT count(*) FROM catalogs"), 2, "catalogs are added, not replaced");
    assert_eq!(server.count("SELECT count(*) FROM catalog_events WHERE hash = 'real-hash'"), 2, "the real catalog is intact");

    // The honest client's events are all stored and still declared, and so is anything else it sends.
    let reply = server.ingest(&envelope("honest", "s1", vec![event("e1", "file.open"), event("e2", "file.save"), event("e3", "not.in.any.catalog")]));
    assert_eq!(reply.counts(), (3, 0, 0, false), "nothing is rejected for being undeclared");
    assert_eq!(server.count("SELECT count(*) FROM events WHERE declared = 1"), 2);
    assert_eq!(server.text("SELECT name FROM events WHERE declared = 0"), "not.in.any.catalog");

    // A catalog sent twice, or a hash that is already held, changes nothing.
    server.ingest(&envelope("honest", "s2", vec![start_with("s2-start", "real-hash", Some(&["file.open"]))]));
    assert_eq!(server.count("SELECT count(*) FROM catalog_events WHERE hash = 'real-hash'"), 2);
    // need_catalog is still asked for when a session names a hash the server does not hold.
    let unknown = server.ingest(&envelope("honest", "s3", vec![start_with("s3-start", "never-seen", None)]));
    assert!(unknown.counts().3);
}

#[test]
fn catalogs_are_capped_per_version() {
    let server = TestServer::demo();
    for index in 0..20 {
        server.ingest(&envelope("attacker", &format!("s{index}"), vec![start_with(&format!("st{index}"), &format!("hash-{index}"), Some(&["a.b"]))]));
    }
    assert!(server.count("SELECT count(*) FROM catalogs") <= 8);
}

#[test]
fn never_fired_uses_the_catalog_most_installs_reported() {
    let server = TestServer::demo();
    // Three installs report the real catalog; one hostile install reports a huge one.
    for (index, install) in ["a", "b", "c"].iter().enumerate() {
        let names: Option<&[&str]> = if index == 0 { Some(&["real.one", "real.two"]) } else { None };
        server.ingest(&envelope(install, &format!("s-{install}"), vec![start_with(&format!("st-{install}"), "real", names)]));
    }
    let flood: Vec<String> = (0..500).map(|n| format!("junk.name{n}")).collect();
    let flood_refs: Vec<&str> = flood.iter().map(String::as_str).collect();
    server.ingest(&envelope("evil", "s-evil", vec![start_with("st-evil", "evil", Some(&flood_refs))]));
    server.ingest(&envelope("a", "s-a", vec![event("fired", "real.one")]));
    ashlar_trace_server::jobs::rollup_recent(server.store.as_ref(), ashlar_trace_server::now_ms(), 3, 90).unwrap();

    let never: Vec<String> = server.store.never_fired(server.app_id, Some("1.0.0")).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(never, ["real.two"], "the forged catalog adds nothing");
    let never_any: Vec<String> = server.store.never_fired(server.app_id, None).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(never_any, ["real.two"]);
}

#[test]
fn declared_travels_through_the_rollup() {
    let server = TestServer::demo();
    server.ingest(&envelope(
        "a",
        "s",
        vec![start_with("st", "h", Some(&["file.open"])), event("e1", "file.open"), event("e2", "file.open"), event("e3", "made.up")],
    ));
    ashlar_trace_server::jobs::rollup_recent(server.store.as_ref(), ashlar_trace_server::now_ms(), 3, 90).unwrap();
    let today = ashlar_trace_server::now_ms() / DAY_MS;
    let declared = server.store.events_ranked(server.app_id, today - 1, today, None, true).unwrap();
    let undeclared = server.store.events_ranked(server.app_id, today - 1, today, None, false).unwrap();
    assert_eq!(declared.iter().map(|e| (e.name.as_str(), e.count)).collect::<Vec<_>>(), [("file.open", 2)]);
    assert_eq!(undeclared.iter().map(|e| (e.name.as_str(), e.count)).collect::<Vec<_>>(), [("made.up", 1)]);
}

// ---------------------------------------------------------------------------------------------------------
// H2, L2: rate limits

#[test]
fn one_address_cannot_drain_the_key_bucket_for_everyone_else() {
    // Behind a trusted proxy the address comes from X-Forwarded-For. Each address gets 3 requests a minute;
    // the key as a whole gets 10.
    let server = TestServer::demo_with(|config| {
        config.rate_per_min = 3;
        config.key_rate_per_min = 10;
        config.trust_proxy = true;
    });
    let body = serde_json::to_vec(&envelope("i", "s", vec![])).unwrap();
    let from = |address: &str| server.post_with("/v1/ingest", Some(&server.key), false, &body, &[("x-forwarded-for", address)]).status;

    let noisy: Vec<u16> = (0..100).map(|_| from("203.0.113.1")).collect();
    assert_eq!(noisy.iter().filter(|status| **status == 200).count(), 3);
    assert!(noisy.iter().skip(3).all(|status| *status == 429));
    // Other addresses are still served: the noisy one used 3 key tokens, not 100.
    for address in ["203.0.113.2", "203.0.113.3", "203.0.113.4"] {
        assert_eq!(from(address), 200, "{address} was starved");
    }
}

#[test]
fn the_key_limit_is_a_separate_capacity_guard() {
    let server = TestServer::demo_with(|config| {
        config.rate_per_min = 100;
        config.key_rate_per_min = 2;
        config.trust_proxy = true;
    });
    let body = serde_json::to_vec(&envelope("i", "s", vec![])).unwrap();
    let from = |address: &str| server.post_with("/v1/ingest", Some(&server.key), false, &body, &[("x-forwarded-for", address)]);
    assert_eq!(from("198.51.100.1").status, 200);
    assert_eq!(from("198.51.100.2").status, 200);
    let limited = from("198.51.100.3");
    assert_eq!(limited.status, 429);
    assert!(limited.retry_after.is_some());
}

#[test]
fn unknown_keys_and_health_are_limited_by_address_too() {
    let server = TestServer::demo_with(|config| config.rate_per_min = 5);
    let statuses: Vec<u16> = (0..10).map(|_| server.post_raw("/v1/ingest", Some("tk_guess"), false, b"{}").status).collect();
    assert_eq!(statuses.iter().filter(|s| **s == 401).count(), 5);
    assert_eq!(statuses.iter().filter(|s| **s == 429).count(), 5);
    // The same address is now refused on the health route as well.
    assert_eq!(server.get("/v1/health").status, 429);
}

#[test]
fn only_the_last_forwarded_for_line_counts() {
    let server = TestServer::demo_with(|config| {
        config.rate_per_min = 2;
        config.trust_proxy = true;
    });
    let body = serde_json::to_vec(&envelope("i", "s", vec![])).unwrap();
    // The client forges different leading lines each time; the proxy's own entry (last) is always the same.
    let statuses: Vec<u16> = (0..4)
        .map(|n| {
            server
                .post_with(
                    "/v1/ingest",
                    Some(&server.key),
                    false,
                    &body,
                    &[("x-forwarded-for", &format!("1.1.1.{n}")), ("x-forwarded-for", "192.0.2.50")],
                )
                .status
        })
        .collect();
    assert_eq!(statuses, [200, 200, 429, 429]);
}

// ---------------------------------------------------------------------------------------------------------
// M1, M2: growth

#[test]
fn drop_counts_are_clamped_named_and_capped() {
    let server = TestServer::demo();
    let huge = |id: &str, reason: &str| item(id, ItemBody::Drops(Drops { reasons: [(reason.to_string(), u64::MAX)].into() }));
    for round in 0..3 {
        let items = vec![huge(&format!("d{round}"), "queue_full"), huge(&format!("bad{round}"), "Not A Valid Name!")];
        assert_eq!(server.ingest(&envelope("i", "s", items)).status, 200);
    }
    // Per-item counts are clamped to a million, and a name that is not a valid name is never stored.
    assert_eq!(server.count("SELECT count FROM daily_drops WHERE reason = 'queue_full'"), 3_000_000);
    assert_eq!(server.count("SELECT count(*) FROM daily_drops"), 1);

    // Past the cap of distinct reasons per day, new ones are folded into `other`.
    let many: Vec<(String, u64)> = (0..100).map(|n| (format!("reason_{n:03}"), 1)).collect();
    server.ingest(&envelope("i", "s", vec![item("many", ItemBody::Drops(Drops { reasons: many.into_iter().take(32).collect() }))]));
    let more: Vec<(String, u64)> = (32..100).map(|n| (format!("reason_{n:03}"), 1)).collect();
    for (index, chunk) in more.chunks(30).enumerate() {
        server.ingest(&envelope("i", "s", vec![item(&format!("more{index}"), ItemBody::Drops(Drops { reasons: chunk.iter().cloned().collect() }))]));
    }
    assert!(server.count("SELECT count(*) FROM daily_drops") <= 65, "{}", server.count("SELECT count(*) FROM daily_drops"));
    assert!(server.count("SELECT count FROM daily_drops WHERE reason = 'other'") > 0);

    // A stored total can never exceed the cap, and a total that is already huge (from before) is read safely.
    let conn = rusqlite::Connection::open(&server.db_path).unwrap();
    conn.execute("UPDATE daily_drops SET count = 9223372036854775800 WHERE reason = 'queue_full'", []).unwrap();
    conn.execute("INSERT INTO daily_drops (app_id, day, reason, count) VALUES (?1, 1, 'queue_full', 9223372036854775800)", [server.app_id]).unwrap();
    let read = server.store.drops(server.app_id, 0, i64::MAX / 2);
    assert!(read.is_ok(), "{read:?}");
    server.ingest(&envelope("i", "s", vec![huge("again", "queue_full")]));
    assert_eq!(server.count("SELECT count FROM daily_drops WHERE day > 1000 AND reason = 'queue_full'"), 1_000_000_000_000);
}

#[test]
fn a_crash_flood_is_capped_per_envelope_and_new_issues_per_day() {
    let server = TestServer::demo();
    let crashes: Vec<Item> = (0..30).map(|n| panic_crash(&format!("c{n}"), 1000, &format!("0: demo::f{n}"))).collect();
    let reply = server.ingest(&envelope("i", "s", crashes));
    assert_eq!(reply.counts().0, 20);
    assert_eq!(reply.counts().2, 10, "the rest are counted as rejected");

    // 400 more distinct fingerprints (through the store, to be quick): issues stop at the cap plus one
    // overflow issue, which keeps counting what would have been new issues.
    for batch in 0..20 {
        let crashes: Vec<Item> = (0..20).map(|n| panic_crash(&format!("f{batch}-{n}"), 1000, &format!("0: demo::flood{batch}_{n}"))).collect();
        server.store.ingest(server.app_id, &prepare(envelope("i", "s", crashes), ashlar_trace_server::now_ms()).unwrap()).unwrap();
    }
    let issues = server.count("SELECT count(*) FROM issues");
    assert_eq!(issues, 201, "200 real issues and one overflow issue");
    assert_eq!(server.text("SELECT title FROM issues WHERE fingerprint LIKE 'overflow:%'"), "Too many distinct crashes");
    assert_eq!(server.count("SELECT count FROM issues WHERE fingerprint LIKE 'overflow:%'"), 420 - 200);
    assert_eq!(server.count("SELECT count(*) FROM crashes"), 420, "no crash is dropped");
    // A known fingerprint still lands in its own issue.
    server.ingest(&envelope("i", "s", vec![panic_crash("again", 10, "0: demo::f3")]));
    assert_eq!(server.count("SELECT i.count FROM issues i JOIN crashes c ON c.issue_id = i.id WHERE c.backtrace = '0: demo::f3' LIMIT 1"), 2);
    assert_eq!(server.count("SELECT count(*) FROM issues"), 201);
}

#[test]
fn item_ids_and_times_are_bounded() {
    let server = TestServer::demo();
    let long_id = event(&"x".repeat(65), "file.open");
    let ok_id = event(&"y".repeat(64), "file.open");
    let mut ancient = event("ancient", "file.open");
    ancient.ts = 1_000;
    let mut future = event("future", "file.open");
    future.ts = now() + 30 * DAY_MS as u64;
    let reply = server.ingest(&envelope("i", "s", vec![long_id, ok_id, ancient, future]));
    assert_eq!(reply.counts().0, 3);
    assert_eq!(reply.counts().2, 1);
    let nowms = ashlar_trace_server::now_ms();
    assert!(server.count("SELECT min(ts) FROM events") >= nowms - 90 * DAY_MS - 5_000, "an ancient time is clamped into the retention window");
    assert!(server.count("SELECT max(ts) FROM events") <= nowms + 3_600_000 + 5_000, "a far future time is clamped to an hour ahead");
}

#[test]
fn rollups_fold_the_long_tail_of_versions_and_event_names() {
    let server = TestServer::demo();
    for n in 0..80 {
        let mut env = envelope(
            &format!("i{n}"),
            &format!("s{n}"),
            vec![item(&format!("h{n}"), ItemBody::SessionHeartbeat(totals(1))), event(&format!("e{n}"), &format!("junk.event{n}"))],
        );
        env.app.version = format!("9.{n}.0");
        server.store.ingest(server.app_id, &prepare(env, ashlar_trace_server::now_ms()).unwrap()).unwrap();
    }
    ashlar_trace_server::jobs::rollup_recent(server.store.as_ref(), ashlar_trace_server::now_ms(), 3, 90).unwrap();
    assert_eq!(server.count("SELECT sum(sessions) FROM daily_sessions"), 80, "nothing is lost, only grouped");
    assert!(server.count("SELECT count(DISTINCT version) FROM daily_sessions") <= 51);
    assert!(server.count("SELECT count(*) FROM daily_sessions WHERE version = 'other'") >= 1);
    assert_eq!(server.count("SELECT sum(count) FROM daily_events"), 80);
}

#[test]
fn rollup_runs_per_app_and_leaves_other_apps_alone() {
    let server = TestServer::demo();
    let second = server.store.create_app("second", "Second", "tk_second_key", ashlar_trace_server::now_ms()).unwrap();
    server.ingest(&envelope("a", "s", vec![item("h", ItemBody::SessionHeartbeat(totals(1)))]));
    server
        .store
        .ingest(
            second.id,
            &prepare(envelope("b", "t", vec![item("h2", ItemBody::SessionHeartbeat(totals(1)))]), ashlar_trace_server::now_ms()).unwrap(),
        )
        .unwrap();
    ashlar_trace_server::jobs::rollup_recent(server.store.as_ref(), ashlar_trace_server::now_ms(), 3, 90).unwrap();
    assert_eq!(server.count("SELECT count(DISTINCT app_id) FROM daily_sessions"), 2);
    assert_eq!(server.count("SELECT count(*) FROM daily_installs"), 2);
}

#[test]
fn retention_also_prunes_drops_and_catalogs() {
    let server = TestServer::demo();
    let conn = rusqlite::Connection::open(&server.db_path).unwrap();
    let now = ashlar_trace_server::now_ms();
    let old_day = now / DAY_MS - 200;
    conn.execute("INSERT INTO daily_drops (app_id, day, reason, count) VALUES (?1, ?2, 'queue_full', 5)", rusqlite::params![server.app_id, old_day])
        .unwrap();
    conn.execute(
        "INSERT INTO catalogs (app_id, version, hash, received_at) VALUES (?1, '0.1', 'old', ?2)",
        rusqlite::params![server.app_id, now - 200 * DAY_MS],
    )
    .unwrap();
    conn.execute("INSERT INTO catalog_events (app_id, version, hash, name, category) VALUES (?1, '0.1', 'old', 'a.b', 'c')", [server.app_id])
        .unwrap();
    server.ingest(&envelope(
        "a",
        "s",
        vec![start_with("st", "fresh", Some(&["file.open"])), item("d", ItemBody::Drops(Drops { reasons: [("queue_full".to_string(), 2)].into() }))],
    ));
    ashlar_trace_server::jobs::run_retention(server.store.as_ref(), now, 90).unwrap();
    assert_eq!(server.count(&format!("SELECT count(*) FROM daily_drops WHERE day = {old_day}")), 0);
    assert_eq!(server.count("SELECT count(*) FROM daily_drops"), 1, "recent drops stay");
    assert_eq!(server.count("SELECT count(*) FROM catalogs"), 1);
    assert_eq!(server.count("SELECT count(*) FROM catalog_events WHERE hash = 'old'"), 0);
    assert_eq!(server.count("SELECT count(*) FROM catalog_events WHERE hash = 'fresh'"), 1);
}

#[test]
fn ingest_answers_507_over_the_size_ceiling_while_everything_else_keeps_working() {
    let server = TestServer::demo_with(|config| config.max_db_mb = 1);
    // Fill the database past 1 MiB with large crash records.
    let blob = "b".repeat(30_000);
    for batch in 0..4 {
        let crashes: Vec<Item> = (0..12)
            .map(|n| {
                let mut crash = panic_crash(&format!("big{batch}-{n}"), 100, &format!("0: demo::fill{batch}_{n}"));
                if let ItemBody::Crash(inner) = &mut crash.body {
                    inner.message = Some(blob.clone());
                    inner.backtrace = Some(format!("0: demo::fill{batch}_{n}\n{blob}"));
                }
                crash
            })
            .collect();
        server.store.ingest(server.app_id, &prepare(envelope("i", "s", crashes), ashlar_trace_server::now_ms()).unwrap()).unwrap();
    }
    assert!(server.store.size_bytes().unwrap() > 1024 * 1024);
    let body = serde_json::to_vec(&envelope("i", "s", vec![event("e", "file.open")])).unwrap();
    let reply = server.post_raw("/v1/ingest", Some(&server.key), false, &body);
    assert_eq!(reply.status, 507, "{}", reply.body);
    assert_eq!(server.count("SELECT count(*) FROM events"), 0);
    // Health, the dashboard login page and forget are not blocked.
    assert_eq!(server.get("/v1/health").status, 200);
    assert_eq!(server.get("/login").status, 200);
    assert_eq!(server.forget("i").status, 200);
    // Login sessions are still written when the ceiling is reached.
    let hash = ashlar_trace_server::cli::hash_password("a long enough password").unwrap();
    server.store.create_admin("root", &hash, ashlar_trace_server::now_ms()).unwrap();
    let id = server.count("SELECT id FROM admins");
    server.store.admin_session_create("aa", id, 1, ashlar_trace_server::now_ms() + DAY_MS).unwrap();
}

#[test]
fn the_size_ceiling_can_be_switched_off() {
    let server = TestServer::demo_with(|config| config.max_db_mb = 0);
    let body = serde_json::to_vec(&envelope("i", "s", vec![event("e", "file.open")])).unwrap();
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, &body).status, 200);
}

// ---------------------------------------------------------------------------------------------------------
// L5

#[test]
fn forget_answers_the_same_whether_or_not_the_install_exists() {
    let server = TestServer::demo();
    server.ingest(&envelope("known", "s", vec![event("e", "file.open")]));
    let existing = server.forget("known");
    let missing = server.forget("never-heard-of-it");
    assert_eq!((existing.status, missing.status), (200, 200));
    assert_eq!(existing.body, missing.body);
    assert_eq!(server.count("SELECT count(*) FROM events"), 0);
}

// ---------------------------------------------------------------------------------------------------------
// M6: connection limits

fn raw(server: &TestServer) -> TcpStream {
    let stream = TcpStream::connect(server.base.trim_start_matches("http://")).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    stream
}

fn closed_within(stream: &mut TcpStream, limit: Duration) -> bool {
    let started = Instant::now();
    let mut buf = [0u8; 256];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return true,
        }
        if started.elapsed() > limit {
            return false;
        }
    }
}

#[test]
fn slow_and_idle_connections_are_closed() {
    let limits = ServerLimits { header_read_timeout: Duration::from_millis(400), max_connections: 100 };
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with_limits(Config { rate_per_min: 0, ..common::config_in(dir.path()) }, limits);
    // Headers that never finish.
    let mut slow = raw(&server);
    slow.write_all(b"POST /v1/ingest HTTP/1.1\r\nHost: x\r\nx-tracer-key: ").unwrap();
    assert!(closed_within(&mut slow, Duration::from_secs(4)), "a client that never finishes its headers must be cut off");
    // A connection that says nothing at all.
    let mut silent = raw(&server);
    assert!(closed_within(&mut silent, Duration::from_secs(4)), "an idle connection must be cut off");
    // A normal request on a fresh connection still works.
    assert_eq!(server.get("/v1/health").status, 200);
}

#[test]
fn the_number_of_open_connections_is_capped() {
    let limits = ServerLimits { header_read_timeout: Duration::from_millis(800), max_connections: 1 };
    let dir = tempfile::tempdir().unwrap();
    let server = TestServer::start_with_limits(Config { rate_per_min: 0, ..common::config_in(dir.path()) }, limits);
    // One idle connection holds the only slot...
    let _hog = raw(&server);
    std::thread::sleep(Duration::from_millis(100));
    // ...so the next request waits until the hog is cut off by the header timeout, then is served.
    let started = Instant::now();
    assert_eq!(server.get("/v1/health").status, 200);
    assert!(started.elapsed() >= Duration::from_millis(400), "served after only {:?}: the cap did not hold", started.elapsed());
}
