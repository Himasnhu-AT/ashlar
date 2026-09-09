#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]
mod common;

use std::collections::BTreeMap;

use ashlar_trace_proto::{Catalog, CatalogEvent, Crash, CrashKind, Event, ItemBody, Perf, SessionEnd, SessionStart, SessionStatus, SessionTotals};
use ashlar_trace_server::store::Store;
use common::{TestServer, envelope, gzip, item};

const FULL: &str = include_str!("../../trace-proto/tests/fixtures/envelope_full.json");
const FUTURE: &str = include_str!("../../trace-proto/tests/fixtures/envelope_future.json");

fn event(id: &str, name: &str) -> ashlar_trace_proto::Item {
    item(id, ItemBody::Event(Event { name: name.into(), category: "file".into(), ..Event::default() }))
}

fn crash(id: &str, backtrace: &str) -> ashlar_trace_proto::Item {
    item(
        id,
        ItemBody::Crash(Crash {
            kind: CrashKind::Panic,
            fatal: true,
            message: Some("boom".into()),
            backtrace: Some(backtrace.into()),
            ..Crash::default()
        }),
    )
}

fn totals(duration_s: u64) -> SessionTotals {
    SessionTotals { seq: 1, duration_s, focused_s: duration_s, active_s: duration_s / 2, idle_s: 0 }
}

fn start_with_catalog(id: &str, names: &[&str]) -> ashlar_trace_proto::Item {
    let events = names.iter().map(|name| CatalogEvent { name: (*name).into(), category: "file".into() }).collect();
    item(
        id,
        ItemBody::SessionStart(SessionStart { catalog_hash: Some("hash1".into()), catalog: Some(Catalog { events }), ..SessionStart::default() }),
    )
}

#[test]
fn health_is_ok() {
    let server = TestServer::demo();
    let reply = server.get("/v1/health");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["status"], "ok");
}

#[test]
fn full_fixture_stores_every_item_type_and_a_resend_stores_nothing() {
    let server = TestServer::demo();
    let reply = server.ingest_json(FULL.as_bytes());
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.counts(), (8, 0, 0, false));

    assert_eq!(server.count("SELECT count(*) FROM installs"), 1);
    assert_eq!(server.count("SELECT count(*) FROM sessions"), 1);
    assert_eq!(server.text("SELECT status FROM sessions"), "crashed");
    assert_eq!(server.count("SELECT duration_s FROM sessions"), 62);
    assert_eq!(server.count("SELECT errors FROM sessions"), 1);
    assert_eq!(server.count("SELECT count(*) FROM events"), 2);
    assert_eq!(server.count("SELECT count(*) FROM events WHERE dynamic = 1"), 1);
    assert_eq!(server.text("SELECT props FROM events WHERE name = 'file.open'"), r#"{"count":3,"format":"native","ratio":0.5,"recent":true}"#);
    assert_eq!(server.count("SELECT count(*) FROM perf"), 1);
    assert_eq!(server.count("SELECT frame_count FROM perf"), 3);
    assert_eq!(server.count("SELECT count(*) FROM crashes"), 1);
    assert_eq!(server.count("SELECT count(*) FROM issues"), 1);
    assert_eq!(server.count("SELECT count(*) FROM catalog_events"), 2);
    assert_eq!(server.count("SELECT count FROM daily_drops WHERE reason = 'queue_full'"), 4);
    assert_eq!(server.count("SELECT count(*) FROM items_seen"), 8);

    let again = server.ingest_json(FULL.as_bytes());
    assert_eq!(again.counts(), (0, 8, 0, false));
    assert_eq!(server.count("SELECT count(*) FROM events"), 2);
    assert_eq!(server.count("SELECT count(*) FROM crashes"), 1);
    assert_eq!(server.count("SELECT count FROM issues"), 1);
    assert_eq!(server.count("SELECT count FROM daily_drops WHERE reason = 'queue_full'"), 4);
}

#[test]
fn plain_and_gzip_bodies_are_both_accepted() {
    let server = TestServer::demo();
    let body = serde_json::to_vec(&envelope("i1", "s1", vec![event("e1", "a")])).unwrap();
    let plain = server.post_raw("/v1/ingest", Some(&server.key), false, &body);
    assert_eq!(plain.status, 200, "{}", plain.body);
    let body = serde_json::to_vec(&envelope("i1", "s1", vec![event("e2", "a")])).unwrap();
    let zipped = server.post_raw("/v1/ingest", Some(&server.key), true, &gzip(&body));
    assert_eq!(zipped.status, 200, "{}", zipped.body);
    assert_eq!(server.count("SELECT count(*) FROM events"), 2);
}

#[test]
fn unknown_fields_and_item_types_are_tolerated() {
    let server = TestServer::demo();
    let reply = server.ingest_json(FUTURE.as_bytes());
    assert_eq!(reply.status, 200, "{}", reply.body);
    // The unknown item type is refused; the crash with an unknown kind and the event are stored. No catalog
    // is held yet, so the server asks for one.
    assert_eq!(reply.counts(), (2, 0, 1, true));
    assert_eq!(server.text("SELECT kind FROM crashes"), "other");
    // No session item, so no session row.
    assert_eq!(server.count("SELECT count(*) FROM sessions"), 0);
}

#[test]
fn unknown_missing_and_revoked_keys_are_401() {
    let server = TestServer::demo();
    let body = serde_json::to_vec(&envelope("i1", "s1", vec![])).unwrap();
    assert_eq!(server.post_raw("/v1/ingest", Some("tk_nope"), false, &body).status, 401);
    assert_eq!(server.post_raw("/v1/ingest", None, false, &body).status, 401);
    assert_eq!(server.post_raw("/v1/forget", Some("tk_nope"), false, b"{}").status, 401);
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, &body).status, 200);
    assert!(server.store.revoke_key(&server.key, 1).unwrap());
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, &body).status, 401);
}

#[test]
fn oversized_and_gzip_bomb_bodies_are_413() {
    let server = TestServer::demo();
    let big = vec![b' '; ashlar_trace_proto::MAX_ENVELOPE_BYTES + 10_000];
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, &big).status, 413);

    // About 64 MiB of spaces compresses to tens of kilobytes; it must be refused without being inflated.
    let bomb = {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let chunk = vec![b' '; 1 << 20];
        for _ in 0..64 {
            encoder.write_all(&chunk).unwrap();
        }
        encoder.finish().unwrap()
    };
    assert!(bomb.len() < 200_000, "bomb is {} bytes", bomb.len());
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), true, &bomb).status, 413);
    // The server is still fine afterwards.
    assert_eq!(server.get("/v1/health").status, 200);
}

#[test]
fn malformed_bodies_are_400() {
    let server = TestServer::demo();
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, b"{not json").status, 400);
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, b"").status, 400);
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, br#"{"schema":1,"items":"x"}"#).status, 400);
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), true, b"this is not gzip").status, 400);
    let mut bad = envelope("not valid!", "s1", vec![]);
    assert_eq!(server.ingest(&bad).status, 400);
    bad.install_id = "ok".into();
    bad.session_id = String::new();
    assert_eq!(server.ingest(&bad).status, 400);
}

#[test]
fn a_future_schema_is_422() {
    let server = TestServer::demo();
    let reply = server.ingest_json(br#"{"schema":2,"anything":["the","server","cannot","parse"]}"#);
    assert_eq!(reply.status, 422);
    let mut future = envelope("i1", "s1", vec![]);
    future.schema = 99;
    assert_eq!(server.ingest(&future).status, 422);
}

#[test]
fn rate_limit_returns_429_with_retry_after() {
    let server = TestServer::demo_with(|config| config.rate_per_min = 3);
    let body = serde_json::to_vec(&envelope("i1", "s1", vec![])).unwrap();
    for _ in 0..3 {
        assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, &body).status, 200);
    }
    let limited = server.post_raw("/v1/ingest", Some(&server.key), false, &body);
    assert_eq!(limited.status, 429);
    let wait: u64 = limited.retry_after.expect("Retry-After").parse().unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    // Health shares the address bucket, so it is limited too.
    assert_eq!(server.get("/v1/health").status, 429);
}

#[test]
fn heartbeats_never_lower_totals_and_a_terminal_status_sticks() {
    let server = TestServer::demo();
    let send = |items| assert_eq!(server.ingest(&envelope("i1", "s1", items)).status, 200);
    send(vec![item("start", ItemBody::SessionStart(SessionStart::default())), item("h1", ItemBody::SessionHeartbeat(totals(100)))]);
    send(vec![item("h2", ItemBody::SessionHeartbeat(totals(40)))]);
    assert_eq!(server.count("SELECT duration_s FROM sessions"), 100);
    assert_eq!(server.count("SELECT focused_s FROM sessions"), 100);
    assert_eq!(server.text("SELECT status FROM sessions"), "ok");

    send(vec![item("end1", ItemBody::SessionEnd(SessionEnd { status: SessionStatus::Exited, totals: totals(120), errors: 2 }))]);
    assert_eq!(server.text("SELECT status FROM sessions"), "exited");
    assert_eq!(server.count("SELECT duration_s FROM sessions"), 120);

    send(vec![item("end2", ItemBody::SessionEnd(SessionEnd { status: SessionStatus::Crashed, totals: totals(10), errors: 0 }))]);
    assert_eq!(server.text("SELECT status FROM sessions"), "exited");
    assert_eq!(server.count("SELECT duration_s FROM sessions"), 120);
    assert_eq!(server.count("SELECT errors FROM sessions"), 2);
}

#[test]
fn a_session_cannot_be_hijacked_by_another_install() {
    let server = TestServer::demo();
    server.ingest(&envelope("owner", "s1", vec![item("h1", ItemBody::SessionHeartbeat(totals(10)))]));
    server.ingest(&envelope("intruder", "s1", vec![item("h2", ItemBody::SessionHeartbeat(totals(9999)))]));
    assert_eq!(server.count("SELECT duration_s FROM sessions WHERE session_id = 's1'"), 10);
}

#[test]
fn events_are_checked_against_a_held_catalog_and_accepted_with_need_catalog_otherwise() {
    let server = TestServer::demo();

    // No catalog held: accepted, and the server asks for it.
    let reply = server.ingest(&envelope("i1", "s1", vec![event("e1", "anything")]));
    assert_eq!(reply.counts(), (1, 0, 0, true));

    // A session_start naming a hash the server does not hold also asks.
    let hash_only = item("s", ItemBody::SessionStart(SessionStart { catalog_hash: Some("hash1".into()), ..SessionStart::default() }));
    assert!(server.ingest(&envelope("i1", "s2", vec![hash_only])).counts().3);

    // Carrying the catalog stores it. An undeclared name is still stored, flagged `declared = 0`; nothing is
    // ever refused because of the catalog.
    let mut dynamic = event("e4", "undeclared.but.dynamic");
    if let ItemBody::Event(inner) = &mut dynamic.body {
        inner.dynamic = true;
    }
    let reply = server.ingest(&envelope(
        "i1",
        "s3",
        vec![start_with_catalog("s3start", &["file.open"]), event("e2", "file.open"), event("e3", "undeclared"), dynamic],
    ));
    assert_eq!(reply.counts(), (4, 0, 0, false));
    assert_eq!(server.count("SELECT declared FROM events WHERE name = 'undeclared'"), 0);
    assert_eq!(server.count("SELECT declared FROM events WHERE name = 'file.open'"), 1);
    assert_eq!(server.count("SELECT declared FROM events WHERE name = 'undeclared.but.dynamic'"), 1);

    // The acknowledged hash no longer triggers need_catalog.
    let hash_only = item("s5", ItemBody::SessionStart(SessionStart { catalog_hash: Some("hash1".into()), ..SessionStart::default() }));
    assert!(!server.ingest(&envelope("i1", "s4", vec![hash_only])).counts().3);

    // A different app version has no catalog of its own.
    let mut other = envelope("i1", "s6", vec![event("e5", "whatever")]);
    other.app.version = "2.0.0".into();
    assert!(server.ingest(&other).counts().3);
}

#[test]
fn crashes_with_the_same_stack_share_an_issue_and_different_stacks_do_not() {
    let server = TestServer::demo();
    let one = "0: 0x1000 - demo::render::draw::h0123456789abcdef\n1: 0x2000 - demo::main::h1111111111111111\n2: std::rt::lang_start";
    let same = "   0: 0x7777 - demo::render::draw::hfedcba9876543210\n   1: 0x8888 - demo::main::h2222222222222222\n             at src/main.rs:9\n   2: std::rt::lang_start";
    let other = "0: demo::audio::mix::h0123456789abcdef\n1: demo::main::h1111111111111111";
    let beat = item("h1", ItemBody::SessionHeartbeat(totals(1)));
    assert_eq!(server.ingest(&envelope("i1", "s1", vec![beat, crash("c1", one)])).status, 200);
    assert_eq!(server.ingest(&envelope("i2", "s2", vec![crash("c2", same)])).status, 200);
    assert_eq!(server.count("SELECT count(*) FROM issues"), 1);
    assert_eq!(server.count("SELECT count FROM issues"), 2);
    assert_eq!(server.count("SELECT count(*) FROM crashes"), 2);
    assert_eq!(server.ingest(&envelope("i1", "s3", vec![crash("c3", other)])).status, 200);
    assert_eq!(server.count("SELECT count(*) FROM issues"), 2);
    // A fatal panic marks an ok session crashed.
    assert_eq!(server.text("SELECT status FROM sessions WHERE session_id = 's1'"), "crashed");
}

#[test]
fn forget_removes_the_install_everywhere_and_leaves_others() {
    let server = TestServer::demo();
    let items = |tag: &str| {
        vec![
            item(&format!("{tag}start"), ItemBody::SessionStart(SessionStart::default())),
            event(&format!("{tag}e"), "file.open"),
            item(&format!("{tag}p"), ItemBody::Perf(Perf { window_s: 60, ..Perf::default() })),
            crash(&format!("{tag}c"), &format!("0: demo::only_{tag}")),
        ]
    };
    server.ingest(&envelope("gone", "sg", items("g")));
    server.ingest(&envelope("stay", "ss", items("s")));
    ashlar_trace_server::jobs::rollup_recent(server.store.as_ref(), ashlar_trace_server::now_ms(), 3, 90).unwrap();
    assert_eq!(server.count("SELECT count(*) FROM daily_installs"), 2);

    let reply = server.forget("gone");
    assert_eq!(reply.status, 200, "{}", reply.body);
    for table in ["installs", "sessions", "events", "perf", "crashes", "daily_installs"] {
        assert_eq!(server.count(&format!("SELECT count(*) FROM {table} WHERE install_id = 'gone'")), 0, "{table}");
        assert_eq!(server.count(&format!("SELECT count(*) FROM {table} WHERE install_id = 'stay'")), 1, "{table}");
    }
    // The issue only the forgotten install had is gone.
    assert_eq!(server.count("SELECT count(*) FROM issues"), 1);
    assert_eq!(server.forget("").status, 400);
    assert_eq!(server.post_raw("/v1/forget", Some(&server.key), false, b"nope").status, 400);
}

#[test]
fn hostile_values_are_truncated_not_fatal() {
    let server = TestServer::demo();
    let mut props = BTreeMap::new();
    props.insert("note".to_string(), ashlar_trace_proto::PropValue::Str("x".repeat(10_000)));
    let long_event = item("e1", ItemBody::Event(Event { name: "a".into(), category: "b".into(), dynamic: false, props }));
    let big_crash =
        item("c1", ItemBody::Crash(Crash { message: Some("m".repeat(100_000)), backtrace: Some("b".repeat(300_000)), ..Crash::default() }));
    let mut env = envelope("i1", "s1", vec![long_event, big_crash]);
    env.app.version = "v".repeat(1000);
    let reply = server.ingest(&env);
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.counts().0, 2);
    assert_eq!(server.count("SELECT length(message) FROM crashes"), 2048);
    assert_eq!(server.count("SELECT length(backtrace) FROM crashes"), 32 * 1024);
    assert_eq!(server.count("SELECT length(version) FROM events"), 64);
    assert_eq!(server.count("SELECT length(json_extract(props, '$.note')) FROM events"), 64);
}
