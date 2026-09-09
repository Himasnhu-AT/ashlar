//! Exact boundaries: sizes, counts, caps, day edges, retention, the size ceiling, and the limiter.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]

mod common;

use std::time::{Duration, Instant};

use ashlar_trace_proto::MAX_ENVELOPE_BYTES;
use ashlar_trace_server::http::limiter::{Limiters, MAX_TRACKED, TokenBuckets};
use ashlar_trace_server::ingest::DAY_MS;
use ashlar_trace_server::store::Store;
use common::{TestServer, gzip, now};
use serde_json::{Value, json};

fn envelope_json(items: Vec<Value>) -> Value {
    json!({"schema": 1, "app": {"name": "demo", "version": "1.0.0"}, "install_id": "i", "session_id": "s", "sent_at": now(), "items": items})
}

fn event(id: &str, name: &str) -> Value {
    json!({"id": id, "ts": now(), "type": "event", "name": name, "category": "file"})
}

/// An envelope padded with trailing spaces to exactly `size` bytes.
fn padded(size: usize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&envelope_json(vec![event("e1", "file.open")])).unwrap();
    assert!(bytes.len() <= size);
    bytes.resize(size, b' ');
    bytes
}

#[test]
fn the_body_limit_is_exact_for_plain_and_gzip_bodies() {
    let server = TestServer::demo();
    let post = |bytes: &[u8], zipped: bool| {
        let body = if zipped { gzip(bytes) } else { bytes.to_vec() };
        server.post_raw("/v1/ingest", Some(&server.key), zipped, &body).status
    };
    for zipped in [false, true] {
        assert_eq!(post(&padded(MAX_ENVELOPE_BYTES - 1), zipped), 200, "one under, gzip {zipped}");
        assert_eq!(post(&padded(MAX_ENVELOPE_BYTES), zipped), 200, "exactly at the limit, gzip {zipped}");
        assert_eq!(post(&padded(MAX_ENVELOPE_BYTES + 1), zipped), 413, "one over, gzip {zipped}");
    }
}

#[test]
fn item_counts_and_names_are_exact() {
    let server = TestServer::demo();
    let items = |n: usize, tag: &str| (0..n).map(|i| event(&format!("{tag}{i}"), "file.open")).collect::<Vec<_>>();
    let at = server.ingest_json(&serde_json::to_vec(&envelope_json(items(2000, "a"))).unwrap());
    assert_eq!(at.counts().0, 2000);
    assert_eq!(at.counts().2, 0);
    let over = server.ingest_json(&serde_json::to_vec(&envelope_json(items(2001, "b"))).unwrap());
    assert_eq!((over.counts().0, over.counts().2), (2000, 1));

    let name = |n: usize| format!("a{}", "b".repeat(n - 1));
    let reply = server.ingest_json(&serde_json::to_vec(&envelope_json(vec![event("n64", &name(64)), event("n65", &name(65))])).unwrap());
    assert_eq!((reply.counts().0, reply.counts().2), (1, 1), "a 64-byte name is accepted, 65 is not");
    assert_eq!(server.count("SELECT count(*) FROM events WHERE length(name) = 64"), 1);
}

#[test]
fn every_text_cap_cuts_at_the_cap_and_keeps_text_below_it() {
    let server = TestServer::demo();
    // (field, cap) for crash fields; one string at the cap, one a byte over, one well under.
    let caps = [("message", 2048usize), ("location", 512), ("thread", 128), ("context", 256), ("backtrace", 32 * 1024)];
    for (n, size) in [("under", None), ("at", Some(0usize)), ("over", Some(1))] {
        let mut crash = json!({"id": format!("c-{n}"), "ts": now(), "type": "crash", "kind": "panic"});
        for (field, cap) in caps {
            let len = match size {
                None => 10,
                Some(extra) => cap + extra,
            };
            crash[field] = json!("x".repeat(len));
        }
        let mut env = envelope_json(vec![crash]);
        env["session_id"] = json!(format!("s-{n}"));
        assert_eq!(server.ingest_json(&serde_json::to_vec(&env).unwrap()).status, 200);
    }
    for (field, cap) in caps {
        let lengths: Vec<i64> = {
            let conn = rusqlite::Connection::open(&server.db_path).unwrap();
            let mut stmt = conn.prepare(&format!("SELECT length({field}) FROM crashes ORDER BY id")).unwrap();
            stmt.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
        };
        assert_eq!(lengths, [10, cap as i64, cap as i64], "{field}");
    }
}

#[test]
fn counts_of_breadcrumbs_props_and_catalog_events_are_capped() {
    let server = TestServer::demo();
    for (tag, crumbs, props, names) in [("at", 100usize, 16usize, 2000usize), ("over", 101, 17, 2001)] {
        let breadcrumbs: Vec<Value> = (0..crumbs).map(|n| json!({"ts": n, "level": "info", "target": "t", "message": format!("m{n}")})).collect();
        let props: serde_json::Map<String, Value> = (0..props).map(|n| (format!("key{n:02}"), json!(n))).collect();
        let events: Vec<Value> = (0..names).map(|n| json!({"name": format!("cat.event{n}"), "category": "cat"})).collect();
        let mut env = envelope_json(vec![
            json!({"id": format!("crash-{tag}"), "ts": now(), "type": "crash", "kind": "panic", "breadcrumbs": breadcrumbs}),
            json!({"id": format!("event-{tag}"), "ts": now(), "type": "event", "name": "file.open", "category": "file", "props": props}),
            json!({"id": format!("start-{tag}"), "ts": now(), "type": "session_start", "catalog_hash": tag, "catalog": {"events": events}}),
        ]);
        env["app"]["version"] = json!(tag);
        env["session_id"] = json!(format!("s-{tag}"));
        assert_eq!(server.ingest_json(&serde_json::to_vec(&env).unwrap()).status, 200, "{tag}");
    }
    for tag in ["at", "over"] {
        assert_eq!(server.count(&format!("SELECT json_array_length(breadcrumbs) FROM crashes WHERE version = '{tag}'")), 100, "{tag} breadcrumbs");
        assert_eq!(server.count(&format!("SELECT count(*) FROM json_each((SELECT props FROM events WHERE version = '{tag}'))")), 16, "{tag} props");
        assert_eq!(server.count(&format!("SELECT count(*) FROM catalog_events WHERE version = '{tag}'")), 2000, "{tag} catalog");
    }
}

#[test]
fn identifier_lengths_are_exact() {
    let server = TestServer::demo();
    let send = |install: String, session: String, item_id: String| {
        let mut env = envelope_json(vec![event(&item_id, "file.open")]);
        env["install_id"] = json!(install);
        env["session_id"] = json!(session);
        server.ingest_json(&serde_json::to_vec(&env).unwrap())
    };
    assert_eq!(send("i".repeat(64), "s".repeat(64), "e".repeat(64)).counts().0, 1);
    assert_eq!(send("i".repeat(65), "s".repeat(64), "f".repeat(64)).status, 400);
    assert_eq!(send("i".repeat(64), "s".repeat(65), "g".repeat(64)).status, 400);
    assert_eq!(send("i".repeat(64), "s".repeat(64), "h".repeat(65)).counts(), (0, 0, 1, false));
}

#[test]
fn items_on_either_side_of_midnight_land_on_the_right_day_and_a_session_may_span_it() {
    let server = TestServer::demo();
    let day = ashlar_trace_server::now_ms() / DAY_MS - 3;
    let midnight = ((day + 1) * DAY_MS) as u64;
    let env = json!({"schema": 1, "app": {"name": "demo", "version": "1.0.0"}, "install_id": "i", "session_id": "s", "sent_at": now(),
    "items": [
        {"id": "a", "ts": midnight - 1000 * 60, "type": "session_start"},
        {"id": "b", "ts": midnight - 1, "type": "event", "name": "file.open", "category": "file"},
        {"id": "c", "ts": midnight, "type": "event", "name": "file.open", "category": "file"},
        {"id": "d", "ts": midnight + 60_000, "type": "session_heartbeat", "seq": 1, "duration_s": 120, "focused_s": 100, "active_s": 50, "idle_s": 20}
    ]});
    assert_eq!(server.ingest_json(&serde_json::to_vec(&env).unwrap()).counts().0, 4);
    assert_eq!(server.count("SELECT day FROM events WHERE id = 1"), day);
    assert_eq!(server.count("SELECT day FROM events WHERE id = 2"), day + 1);
    // The session belongs to the day it started; its later activity does not move it.
    assert_eq!(server.count("SELECT day FROM sessions"), day);
    ashlar_trace_server::jobs::rollup_pending(server.store.as_ref(), ashlar_trace_server::now_ms(), 90).unwrap();
    assert_eq!(server.count(&format!("SELECT sessions FROM daily_sessions WHERE day = {day}")), 1);
    assert_eq!(server.count(&format!("SELECT count(*) FROM daily_sessions WHERE day = {}", day + 1)), 0);
    assert_eq!(server.count(&format!("SELECT sum(count) FROM daily_events WHERE day = {day}")), 1);
    assert_eq!(server.count(&format!("SELECT sum(count) FROM daily_events WHERE day = {}", day + 1)), 1);
}

#[test]
fn retention_keeps_the_day_exactly_n_days_old_and_drops_the_one_before() {
    let server = TestServer::demo();
    let nowms = ashlar_trace_server::now_ms();
    let today = nowms / DAY_MS;
    for (n, age) in [89, 90, 91].into_iter().enumerate() {
        let env = json!({"schema": 1, "app": {"name": "demo", "version": "1.0.0"}, "install_id": "i", "session_id": format!("s{n}"), "sent_at": now(),
            "items": [{"id": format!("e{n}"), "ts": ((today - age) * DAY_MS + 1000) as u64, "type": "event", "name": "file.open", "category": "file"}]});
        server.ingest_json(&serde_json::to_vec(&env).unwrap());
    }
    // Days 89 and 90 are inside the clamp window (90 days); 91 was clamped to the window's edge on the way in.
    let stats = ashlar_trace_server::jobs::run_retention(server.store.as_ref(), nowms, 90).unwrap();
    let days: Vec<i64> = {
        let conn = rusqlite::Connection::open(&server.db_path).unwrap();
        let mut stmt = conn.prepare("SELECT day FROM events ORDER BY day").unwrap();
        stmt.query_map([], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
    };
    assert!(days.contains(&(today - 89)) && days.contains(&(today - 90)), "{days:?}");
    assert!(days.iter().all(|d| *d >= today - 90), "{days:?}");
    let _ = stats;
    // One day later the oldest of those is past retention.
    let stats = ashlar_trace_server::jobs::run_retention(server.store.as_ref(), nowms + DAY_MS, 90).unwrap();
    assert!(stats.events >= 1);
    assert!(server.count("SELECT min(day) FROM events") >= today - 89);
}

#[test]
fn the_size_ceiling_turns_off_again_once_the_database_shrinks() {
    let server = TestServer::demo_with(|config| config.max_db_mb = 1);
    let blob = "b".repeat(30_000);
    // At most 20 crashes are taken per envelope, so fill the file with several.
    for batch in 0..4 {
        let crashes: Vec<ashlar_trace_proto::Item> = (0..20)
            .map(|n| ashlar_trace_proto::Item {
                id: format!("big{batch}-{n}"),
                ts: now(),
                body: ashlar_trace_proto::ItemBody::Crash(ashlar_trace_proto::Crash {
                    backtrace: Some(format!("0: demo::fill{batch}_{n}\n{blob}")),
                    ..ashlar_trace_proto::Crash::default()
                }),
            })
            .collect();
        let prepared = ashlar_trace_server::ingest::prepare(common::envelope("i", "s", crashes), ashlar_trace_server::now_ms()).unwrap();
        server.store.ingest(server.app_id, &prepared).unwrap();
    }
    let body = serde_json::to_vec(&envelope_json(vec![event("probe1", "file.open")])).unwrap();
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, &body).status, 507);
    // Shrink the file. The measurement is cached for two seconds, so wait that out.
    {
        let conn = rusqlite::Connection::open(&server.db_path).unwrap();
        conn.execute_batch("DELETE FROM crashes; DELETE FROM issues; PRAGMA wal_checkpoint(TRUNCATE); VACUUM;").unwrap();
    }
    std::thread::sleep(Duration::from_millis(2300));
    let body = serde_json::to_vec(&envelope_json(vec![event("probe2", "file.open")])).unwrap();
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, &body).status, 200);
}

// ---------------------------------------------------------------------------------------------------------
// the limiter

#[test]
fn the_bucket_refills_at_exactly_the_documented_rate() {
    let limiter = TokenBuckets::new(60);
    let start = Instant::now();
    for _ in 0..60 {
        assert!(limiter.check(&"k", start).is_ok());
    }
    assert!(limiter.check(&"k", start).is_err());
    assert!(limiter.check(&"k", start + Duration::from_millis(999)).is_err(), "not yet a full token");
    assert!(limiter.check(&"k", start + Duration::from_secs(1)).is_ok(), "one token a second at 60 a minute");
    // Idle for far longer than a minute: the bucket holds its capacity, not more.
    let later = start + Duration::from_secs(3600);
    for _ in 0..60 {
        assert!(limiter.check(&"k", later).is_ok());
    }
    assert!(limiter.check(&"k", later).is_err());
}

#[test]
fn the_limiter_never_tracks_more_than_its_cap_and_evicts_the_least_recently_used() {
    let limiter = TokenBuckets::new(10);
    let start = Instant::now();
    for key in 0..(MAX_TRACKED as u32 + 500) {
        let _ = limiter.check(&key, start + Duration::from_micros(u64::from(key)));
    }
    assert!(limiter.len() <= MAX_TRACKED);

    let small = TokenBuckets::with_capacity(2, 3);
    let t = start;
    // Drain "a" and "b", keep "c" fresh; touch "a" last so "b" is the least recently used.
    for key in ["a", "b", "c"] {
        let _ = small.check(&key, t);
    }
    let _ = small.check(&"a", t + Duration::from_millis(10));
    let _ = small.check(&"a", t + Duration::from_millis(11));
    assert!(small.check(&"a", t + Duration::from_millis(12)).is_err(), "a is drained");
    let _ = small.check(&"d", t + Duration::from_millis(13));
    assert_eq!(small.len(), 3);
    assert!(small.check(&"a", t + Duration::from_millis(14)).is_err(), "a survived and is still drained");
    // b was evicted, so it starts over with a full bucket.
    assert!(small.check(&"b", t + Duration::from_millis(15)).is_ok());
}

#[test]
fn an_ipv4_address_and_its_ipv6_mapped_form_share_one_bucket() {
    let limiters = Limiters::new(1, 100);
    let now = Instant::now();
    let plain: std::net::IpAddr = "203.0.113.7".parse().unwrap();
    let mapped: std::net::IpAddr = "::ffff:203.0.113.7".parse().unwrap();
    assert!(limiters.check_addr(plain, now).is_ok());
    assert!(limiters.check_addr(mapped, now).is_err(), "the mapped form must not get a second allowance");
}
