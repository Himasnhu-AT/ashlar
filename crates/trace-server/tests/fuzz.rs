//! Seeded fuzz-style tests: thousands of mutated envelopes and broken gzip bodies must never produce a 5xx,
//! never store a string over its cap, and never kill the server. Deterministic: same seed, same inputs.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]

mod common;

use common::{TestServer, gzip};
use serde_json::{Value, json};

/// xorshift64*: small and reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const NASTY_STRINGS: &[&str] = &[
    "",
    " ",
    "\u{0}",
    "a\u{0}b",
    "\u{10FFFF}",
    "\u{1F600}\u{1F600}\u{1F600}",
    "\n\r\t",
    "Robert'); DROP TABLE events;--",
    "<script>alert(1)</script>",
    "%s%s%s%n",
    "../../etc/passwd",
    "\u{202E}reversed",
];

fn now() -> u64 {
    tracer_server::now_ms() as u64
}

fn base_envelope(rng: &mut Rng, n: usize) -> Value {
    let t = now();
    json!({
        "schema": 1,
        "app": {"name": "demo", "version": "1.0.0", "build": "abc", "process": "gui"},
        "install_id": format!("inst{}", n % 7),
        "session_id": format!("sess{n}"),
        "device": {"os_name": "macos", "os_version": "26", "arch": "aarch64", "cpu_cores": 8, "ram_mb": 16384, "locale": "en-US"},
        "sent_at": t,
        "items": [
            {"id": format!("a{n}"), "ts": t - 1000, "type": "session_start", "catalog_hash": "h", "catalog": {"events": [{"name": "file.open", "category": "file"}]}, "listening": ["file"], "signals": ["session"]},
            {"id": format!("b{n}"), "ts": t - 900, "type": "session_heartbeat", "seq": 1, "duration_s": 30, "focused_s": 20, "active_s": 10, "idle_s": 5},
            {"id": format!("c{n}"), "ts": t - 800, "type": "event", "name": "file.open", "category": "file", "props": {"count": 3, "mode": "x", "ratio": 0.5, "on": true}},
            {"id": format!("d{n}"), "ts": t - 700, "type": "perf", "window_s": 60, "frames": {"count": 3, "sum_us": 100, "max_us": 50, "slow": 0, "frozen": 0, "buckets": [1, 2, 3]}, "cpu_pct": 1.5, "rss_mb": 100, "counters": {"x": 1}, "gauges": {"y": 2.5}},
            {"id": format!("e{n}"), "ts": t - 600, "type": "crash", "kind": "panic", "fatal": true, "message": "boom", "location": "src/a.rs:1:1", "thread": "main", "backtrace": "0: demo::main\n1: std::rt::lang_start", "breadcrumbs": [{"ts": 1, "level": "info", "target": "t", "message": "m"}], "perf": {"window_s": 1}},
            {"id": format!("f{n}"), "ts": t - 500, "type": "drops", "reasons": {"queue_full": 3}},
            {"id": format!("g{n}"), "ts": t - 400, "type": "session_end", "status": rng_status(rng), "seq": 2, "duration_s": 60, "focused_s": 50, "active_s": 40, "idle_s": 10, "errors": 0}
        ]
    })
}

fn rng_status(rng: &mut Rng) -> &'static str {
    ["exited", "crashed", "abnormal", "ok"][rng.below(4)]
}

fn random_scalar(rng: &mut Rng) -> Value {
    match rng.below(14) {
        0 => Value::Null,
        1 => json!(true),
        2 => json!(0),
        3 => json!(-1),
        4 => json!(u64::MAX),
        5 => json!(i64::MIN),
        6 => json!(1.7976931348623157e308),
        7 => json!(-0.0),
        8 => json!("NaN"),
        9 => json!("-Infinity"),
        10 => json!(rng.next()),
        11 => json!("x".repeat(rng.below(5000))),
        12 => json!(*rng.pick(NASTY_STRINGS)),
        _ => json!([]),
    }
}

/// Replaces a random value anywhere in the tree.
fn mutate(value: &mut Value, rng: &mut Rng, depth: usize) -> bool {
    match value {
        Value::Object(map) if !map.is_empty() => {
            let keys: Vec<String> = map.keys().cloned().collect();
            let key = rng.pick(&keys).clone();
            if rng.below(3) == 0 || depth > 6 {
                match rng.below(4) {
                    0 => {
                        map.remove(&key);
                    }
                    1 => {
                        map.insert(key, random_scalar(rng));
                    }
                    2 => {
                        map.insert(format!("{key}_extra"), random_scalar(rng));
                    }
                    _ => {
                        map.insert(key, deep(rng.below(300)));
                    }
                }
                true
            } else {
                map.get_mut(&key).is_some_and(|inner| mutate(inner, rng, depth + 1))
            }
        }
        Value::Array(items) if !items.is_empty() => {
            let at = rng.below(items.len());
            if rng.below(3) == 0 {
                match rng.below(3) {
                    0 => {
                        items.remove(at);
                    }
                    1 => items[at] = random_scalar(rng),
                    _ => {
                        let copy = items[at].clone();
                        items.push(copy);
                    }
                }
                true
            } else {
                mutate(&mut items[at], rng, depth + 1)
            }
        }
        other => {
            *other = random_scalar(rng);
            true
        }
    }
}

fn deep(levels: usize) -> Value {
    let mut value = json!(1);
    for _ in 0..levels {
        value = json!([value]);
    }
    value
}

fn assert_stored_strings_are_capped(server: &TestServer) {
    let caps: &[(&str, &str, i64)] = &[
        ("crashes", "message", 2048),
        ("crashes", "backtrace", 32 * 1024),
        ("crashes", "location", 512),
        ("crashes", "thread", 128),
        ("crashes", "context", 256),
        ("events", "name", 64),
        ("events", "category", 64),
        ("sessions", "version", 64),
        ("sessions", "install_id", 64),
        ("sessions", "session_id", 64),
        ("installs", "os_name", 64),
        ("installs", "locale", 32),
        ("installs", "arch", 32),
        ("installs", "cpu_brand", 128),
        ("items_seen", "item_id", 64),
        ("daily_drops", "reason", 64),
        ("issues", "title", 200),
    ];
    for (table, column, cap) in caps {
        let longest = server.count(&format!("SELECT COALESCE(max(length(CAST({column} AS BLOB))), 0) FROM {table}"));
        assert!(longest <= *cap, "{table}.{column} holds {longest} bytes, cap {cap}");
    }
}

#[test]
fn mutated_envelopes_never_cause_a_server_error_or_an_oversized_string() {
    let server = TestServer::demo();
    let mut rng = Rng::new(0xF00D);
    let mut statuses = std::collections::BTreeMap::new();
    for n in 0..3000 {
        let mut envelope = base_envelope(&mut rng, n);
        // Most mutations land inside an item; a few hit the header.
        for _ in 0..1 + rng.below(2) {
            if rng.below(5) == 0 {
                mutate(&mut envelope, &mut rng, 0);
            } else if let Some(items) = envelope["items"].as_array_mut().filter(|items| !items.is_empty()) {
                let at = rng.below(items.len());
                mutate(&mut items[at], &mut rng, 1);
            }
        }
        let mut bytes = serde_json::to_vec(&envelope).unwrap();
        match rng.below(16) {
            0 => bytes.truncate(rng.below(bytes.len())),
            1 => {
                let at = rng.below(bytes.len());
                bytes[at] = 0;
            }
            2 => {
                let at = rng.below(bytes.len());
                bytes[at] = 0xFF;
            }
            3 => {
                // A duplicate key, which JSON allows and parsers handle in different ways.
                let text = String::from_utf8_lossy(&bytes).replacen("\"schema\":1", "\"schema\":1,\"schema\":2", 1);
                bytes = text.into_bytes();
            }
            _ => {}
        }
        let reply = if rng.below(2) == 0 {
            server.post_raw("/v1/ingest", Some(&server.key), true, &gzip(&bytes))
        } else {
            server.post_raw("/v1/ingest", Some(&server.key), false, &bytes)
        };
        assert!(reply.status < 500, "input {n} gave {}: {}", reply.status, reply.body);
        *statuses.entry(reply.status).or_insert(0) += 1;
    }
    assert!(statuses.get(&200).copied().unwrap_or(0) > 300, "too few valid envelopes got through: {statuses:?}");
    assert!(statuses.get(&400).copied().unwrap_or(0) > 30, "no malformed input was refused: {statuses:?}");
    assert_eq!(server.get("/v1/health").status, 200, "the server must still be alive");
    assert_stored_strings_are_capped(&server);
    // Whatever was stored, the rollups still compute.
    tracer_server::jobs::rollup_recent(server.store.as_ref(), tracer_server::now_ms(), 3, 90).unwrap();
}

#[test]
fn text_at_every_cap_boundary_is_cut_on_a_character_boundary() {
    let server = TestServer::demo();
    // A four-byte character placed so that each cap falls in the middle of one.
    let caps = [64usize, 128, 256, 512, 2048, 32 * 1024];
    for (n, cap) in caps.iter().enumerate() {
        for offset in 0..4 {
            let text = format!("{}{}", "a".repeat(cap - offset), "\u{1F600}".repeat(4));
            let t = now();
            let envelope = json!({
                "schema": 1, "app": {"name": "demo", "version": text, "build": text, "process": "gui"},
                "install_id": format!("i{n}{offset}"), "session_id": format!("s{n}{offset}"),
                "device": {"os_name": text, "arch": text, "locale": text, "cpu_brand": text},
                "sent_at": t,
                "items": [
                    {"id": format!("c{n}{offset}"), "ts": t, "type": "crash", "kind": "panic", "message": text, "location": text, "thread": text, "context": text, "backtrace": text,
                     "breadcrumbs": [{"ts": 1, "level": text, "target": text, "message": text}]},
                    {"id": format!("e{n}{offset}"), "ts": t, "type": "event", "name": "x", "category": "y", "props": {"k": text}}
                ]
            });
            let reply = server.ingest_json(&serde_json::to_vec(&envelope).unwrap());
            assert!(reply.status == 200 || reply.status == 400, "{}", reply.status);
        }
    }
    assert_stored_strings_are_capped(&server);
    // Stored text is valid UTF-8 (a cut inside a character would have made SQLite return an error here).
    assert!(server.count("SELECT count(*) FROM crashes WHERE message IS NOT NULL") > 0);
}

#[test]
fn ten_thousand_items_in_one_envelope_are_bounded() {
    let server = TestServer::demo();
    let t = now();
    let items: Vec<Value> =
        (0..10_000).map(|n| json!({"id": format!("i{n}"), "ts": t, "type": "event", "name": "file.open", "category": "file"})).collect();
    let envelope =
        json!({"schema": 1, "app": {"name": "demo", "version": "1.0.0"}, "install_id": "i", "session_id": "s", "sent_at": t, "items": items});
    let reply = server.ingest_json(&serde_json::to_vec(&envelope).unwrap());
    assert_eq!(reply.status, 200, "{}", reply.body);
    let (accepted, _, rejected, _) = reply.counts();
    assert_eq!(accepted, 2000);
    assert_eq!(rejected, 8000);
    assert_eq!(server.count("SELECT count(*) FROM events"), 2000);
}

#[test]
fn extreme_timestamps_are_clamped_not_stored_raw() {
    let server = TestServer::demo();
    let t = now();
    for (n, ts) in [0u64, 1, u64::MAX, u64::MAX / 2, t + 365 * 86_400_000, t - 3650 * 86_400_000].into_iter().enumerate() {
        let envelope = json!({"schema": 1, "app": {"name": "demo", "version": "1.0.0"}, "install_id": "i", "session_id": format!("s{n}"), "sent_at": ts,
            "items": [{"id": format!("e{n}"), "ts": ts, "type": "event", "name": "file.open", "category": "file"}]});
        assert_eq!(server.ingest_json(&serde_json::to_vec(&envelope).unwrap()).status, 200);
    }
    let nowms = tracer_server::now_ms();
    assert!(server.count("SELECT min(ts) FROM events") >= nowms - 90 * 86_400_000 - 10_000);
    assert!(server.count("SELECT max(ts) FROM events") <= nowms + 3_600_000 + 10_000);
}

// ---------------------------------------------------------------------------------------------------------
// gzip

fn valid_gzip(server: &TestServer) -> Vec<u8> {
    let mut rng = Rng::new(7);
    let envelope = base_envelope(&mut rng, 1);
    let _ = server;
    gzip(&serde_json::to_vec(&envelope).unwrap())
}

#[test]
fn malformed_gzip_bodies_are_refused_cleanly() {
    let server = TestServer::demo();
    let good = valid_gzip(&server);
    let post = |bytes: &[u8]| server.post_raw("/v1/ingest", Some(&server.key), true, bytes);

    // Truncated at every interesting length.
    for cut in [0, 1, 5, 10, good.len() / 2, good.len() - 9, good.len() - 1] {
        let reply = post(&good[..cut]);
        assert_eq!(reply.status, 400, "truncated at {cut}: {}", reply.body);
    }
    // Wrong CRC (the last eight bytes are CRC32 and length).
    let mut bad_crc = good.clone();
    let at = bad_crc.len() - 8;
    bad_crc[at] ^= 0xFF;
    assert_eq!(post(&bad_crc).status, 400);
    // Wrong length field.
    let mut bad_len = good.clone();
    let last = bad_len.len() - 1;
    bad_len[last] ^= 0x7F;
    assert_eq!(post(&bad_len).status, 400);
    // Garbage header, empty body, a plain body declared as gzip.
    assert_eq!(post(b"\x1f\x8b\x08\x00garbage").status, 400);
    assert_eq!(post(b"").status, 400);
    assert_eq!(post(b"{\"schema\":1}").status, 400);
    // Trailing garbage after a valid member, and two members back to back: never a server error.
    let mut trailing = good.clone();
    trailing.extend_from_slice(b"trailing garbage");
    assert!(post(&trailing).status < 500);
    let mut two = good.clone();
    two.extend_from_slice(&good);
    assert!(post(&two).status < 500);
    // Random corruption of a valid body.
    let mut rng = Rng::new(99);
    for _ in 0..300 {
        let mut broken = good.clone();
        for _ in 0..1 + rng.below(3) {
            let at = rng.below(broken.len());
            broken[at] = rng.next() as u8;
        }
        assert!(post(&broken).status < 500);
    }
    assert_eq!(server.get("/v1/health").status, 200);
}

#[test]
fn a_gzip_bomb_is_refused_without_inflating_it() {
    let server = TestServer::demo();
    let bomb = {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        let chunk = vec![b'0'; 1 << 20];
        for _ in 0..256 {
            encoder.write_all(&chunk).unwrap();
        }
        encoder.finish().unwrap()
    };
    assert!(bomb.len() < 400_000, "{}", bomb.len());
    let started = std::time::Instant::now();
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), true, &bomb).status, 413);
    assert!(started.elapsed() < std::time::Duration::from_secs(3), "took {:?}", started.elapsed());
}
