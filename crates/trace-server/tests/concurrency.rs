//! Concurrency: duplicates, orderings, forget, key revocation and rollups racing ingest. Each test fixes its
//! own inputs; the interleaving varies but the asserted outcome must not.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]

mod common;

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use ashlar_trace_proto::{Crash, CrashKind, Event, Item, ItemBody, SessionEnd, SessionStart, SessionStatus, SessionTotals};
use ashlar_trace_server::store::Store;
use common::{TestServer, envelope, item};

fn totals(duration_s: u64) -> SessionTotals {
    SessionTotals { seq: 1, duration_s, focused_s: duration_s, active_s: duration_s / 2, idle_s: 0 }
}

fn event(id: &str, name: &str) -> Item {
    item(id, ItemBody::Event(Event { name: name.into(), category: "file".into(), ..Event::default() }))
}

#[test]
fn sixty_four_threads_posting_the_same_envelope_store_it_once() {
    let server = TestServer::demo();
    let items: Vec<Item> = (0..20).map(|n| event(&format!("same-{n}"), "file.open")).collect();
    let body = serde_json::to_vec(&envelope("i1", "s1", items)).unwrap();
    let accepted = AtomicU32::new(0);
    let duplicates = AtomicU32::new(0);
    std::thread::scope(|scope| {
        for _ in 0..64 {
            scope.spawn(|| {
                let reply = server.post_raw("/v1/ingest", Some(&server.key), false, &body);
                assert_eq!(reply.status, 200, "{}", reply.body);
                let (a, d, _, _) = reply.counts();
                accepted.fetch_add(a as u32, Ordering::Relaxed);
                duplicates.fetch_add(d as u32, Ordering::Relaxed);
            });
        }
    });
    assert_eq!(accepted.load(Ordering::Relaxed), 20, "each item accepted exactly once across all replies");
    assert_eq!(duplicates.load(Ordering::Relaxed), 63 * 20);
    assert_eq!(server.count("SELECT count(*) FROM events"), 20);
    assert_eq!(server.count("SELECT count(*) FROM items_seen"), 20);
}

fn permutations(n: usize) -> Vec<Vec<usize>> {
    if n == 1 {
        return vec![vec![0]];
    }
    let mut out = Vec::new();
    for rest in permutations(n - 1) {
        for at in 0..n {
            let mut order = rest.clone();
            order.insert(at, n - 1);
            out.push(order);
        }
    }
    out
}

#[test]
fn heartbeats_and_end_in_every_order_keep_totals_monotonic_and_the_terminal_status() {
    let server = TestServer::demo();
    // start, three heartbeats (10, 20, 30 s), an end that says crashed at 40 s, and a late end that says exited.
    let make = |tag: usize, which: usize| -> Item {
        let id = format!("p{tag}-{which}");
        match which {
            0 => item(&id, ItemBody::SessionStart(SessionStart::default())),
            1 => item(&id, ItemBody::SessionHeartbeat(totals(10))),
            2 => item(&id, ItemBody::SessionHeartbeat(totals(20))),
            3 => item(&id, ItemBody::SessionHeartbeat(totals(30))),
            4 => item(&id, ItemBody::SessionEnd(SessionEnd { status: SessionStatus::Crashed, totals: totals(40), errors: 1 })),
            _ => item(&id, ItemBody::SessionEnd(SessionEnd { status: SessionStatus::Exited, totals: totals(35), errors: 0 })),
        }
    };
    let orders = permutations(6);
    assert_eq!(orders.len(), 720);
    for (tag, order) in orders.iter().enumerate().step_by(7) {
        let session = format!("s{tag}");
        let mut last = 0;
        let mut ended: Option<String> = None;
        for which in order {
            server.ingest(&envelope("i1", &session, vec![make(tag, *which)]));
            let (duration, status): (i64, String) = rusqlite::Connection::open(&server.db_path)
                .unwrap()
                .query_row("SELECT duration_s, status FROM sessions WHERE session_id = ?1", [&session], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap();
            assert!(duration >= last, "order {order:?}: duration went {last} -> {duration}");
            last = duration;
            if let Some(first) = &ended {
                assert_eq!(&status, first, "order {order:?}: a terminal status changed");
            } else if status != "ok" {
                ended = Some(status);
            }
        }
        // The first end to arrive decides; the totals are the largest seen.
        let first_end = order.iter().find(|w| **w >= 4).unwrap();
        let (duration, status): (i64, String) = rusqlite::Connection::open(&server.db_path)
            .unwrap()
            .query_row("SELECT duration_s, status FROM sessions WHERE session_id = ?1", [&session], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        assert_eq!(duration, 40, "order {order:?}");
        assert_eq!(status, if *first_end == 4 { "crashed" } else { "exited" }, "order {order:?}");
    }
}

#[test]
fn concurrent_forget_and_ingest_leave_no_rows_and_no_errors() {
    let server = TestServer::demo();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for writer in 0..4 {
            let (server, stop) = (&server, &stop);
            scope.spawn(move || {
                let mut n = 0;
                while !stop.load(Ordering::Relaxed) {
                    n += 1;
                    let session = format!("w{writer}-{}", n % 5);
                    let items = vec![
                        item(&format!("hb-{writer}-{n}"), ItemBody::SessionHeartbeat(totals(n))),
                        event(&format!("ev-{writer}-{n}"), "file.open"),
                        item(
                            &format!("cr-{writer}-{n}"),
                            ItemBody::Crash(Crash { kind: CrashKind::Panic, backtrace: Some(format!("0: demo::f{}", n % 3)), ..Crash::default() }),
                        ),
                    ];
                    let reply = server.ingest(&envelope("victim", &session, items));
                    assert_eq!(reply.status, 200, "ingest during forget: {}", reply.body);
                }
            });
        }
        let (server, stop) = (&server, &stop);
        scope.spawn(move || {
            for _ in 0..40 {
                let reply = server.forget("victim");
                assert_eq!(reply.status, 200, "{}", reply.body);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            stop.store(true, Ordering::Relaxed);
        });
    });
    // Ingest has stopped; one more forget must leave nothing behind anywhere.
    assert_eq!(server.forget("victim").status, 200);
    for table in ["installs", "sessions", "events", "perf", "crashes", "daily_installs"] {
        assert_eq!(server.count(&format!("SELECT count(*) FROM {table} WHERE install_id = 'victim'")), 0, "{table}");
    }
    assert_eq!(
        server.count("SELECT count(*) FROM issues WHERE NOT EXISTS (SELECT 1 FROM crashes WHERE crashes.issue_id = issues.id)"),
        0,
        "orphaned issues"
    );
}

#[test]
fn revoking_a_key_while_ingesting_gives_200s_then_401s_and_nothing_else() {
    let server = TestServer::demo();
    let statuses = std::sync::Mutex::new(Vec::new());
    let revoked = std::sync::atomic::AtomicBool::new(false);
    // Driven by what happened, not by the clock: revoke once some requests got through, and keep
    // sending until well after, however fast the machine is.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut after = 0;
            for n in 0..100_000 {
                let done = revoked.load(Ordering::SeqCst);
                let reply = server.ingest(&envelope("i1", "s1", vec![event(&format!("e{n}"), "file.open")]));
                statuses.lock().unwrap().push(reply.status);
                after += u32::from(done);
                if after >= 20 {
                    break;
                }
            }
        });
        scope.spawn(|| {
            while statuses.lock().unwrap().len() < 20 {
                std::thread::yield_now();
            }
            assert!(server.store.revoke_key(&server.key, 1).unwrap());
            revoked.store(true, Ordering::SeqCst);
        });
    });
    let statuses = statuses.into_inner().unwrap();
    let first_401 = statuses.iter().position(|s| *s == 401).expect("the revocation took effect");
    assert!(statuses[..first_401].iter().all(|s| *s == 200), "{statuses:?}");
    assert!(statuses[first_401..].iter().all(|s| *s == 401), "a revoked key worked again: {statuses:?}");
}

fn seed_for_rollup(server: &TestServer) {
    for n in 0..60 {
        let items = vec![
            item(&format!("st{n}"), ItemBody::SessionStart(SessionStart::default())),
            event(&format!("ev{n}"), "file.open"),
            item(&format!("en{n}"), ItemBody::SessionEnd(SessionEnd { status: SessionStatus::Exited, totals: totals(10 + n), errors: 0 })),
        ];
        assert_eq!(server.ingest(&envelope(&format!("inst{}", n % 9), &format!("sess{n}"), items)).status, 200);
    }
}

fn rollup_fingerprint(server: &TestServer) -> Vec<i64> {
    vec![
        server.count("SELECT COALESCE(sum(sessions), 0) FROM daily_sessions"),
        server.count("SELECT COALESCE(sum(exited), 0) FROM daily_sessions"),
        server.count("SELECT COALESCE(sum(duration_s), 0) FROM daily_sessions"),
        server.count("SELECT COALESCE(sum(count), 0) FROM daily_events"),
        server.count("SELECT count(*) FROM daily_installs"),
        server.count("SELECT COALESCE(sum(dau), 0) FROM daily_active"),
    ]
}

#[test]
fn two_rollups_at_once_agree_with_one() {
    let server = TestServer::demo();
    seed_for_rollup(&server);
    ashlar_trace_server::jobs::rollup_recent(server.store.as_ref(), ashlar_trace_server::now_ms(), 3, 90).unwrap();
    let alone = rollup_fingerprint(&server);
    assert_eq!(alone[0], 60);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| ashlar_trace_server::jobs::rollup_recent(server.store.as_ref(), ashlar_trace_server::now_ms(), 3, 90).unwrap());
        }
        scope.spawn(|| {
            server.store.rollup_dirty(0).unwrap();
        });
    });
    assert_eq!(rollup_fingerprint(&server), alone);
}

#[test]
fn a_rollup_racing_ingest_ends_with_the_same_numbers_as_a_rollup_after_it() {
    let after = TestServer::demo();
    seed_for_rollup(&after);
    ashlar_trace_server::jobs::rollup_pending(after.store.as_ref(), ashlar_trace_server::now_ms(), 90).unwrap();
    let expected = rollup_fingerprint(&after);

    let racing = TestServer::demo();
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            seed_for_rollup(&racing);
            done.store(true, Ordering::Relaxed);
        });
        scope.spawn(|| {
            while !done.load(Ordering::Relaxed) {
                ashlar_trace_server::jobs::rollup_pending(racing.store.as_ref(), ashlar_trace_server::now_ms(), 90).unwrap();
            }
        });
    });
    // Data that arrived during the last rollup left its day marked dirty; the next pass settles it.
    ashlar_trace_server::jobs::rollup_pending(racing.store.as_ref(), ashlar_trace_server::now_ms(), 90).unwrap();
    assert_eq!(rollup_fingerprint(&racing), expected);
    assert_eq!(racing.count("SELECT count(*) FROM dirty_days"), 0, "everything was rolled up");
}
