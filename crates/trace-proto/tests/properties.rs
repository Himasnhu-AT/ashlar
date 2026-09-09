//! Property tests of the wire types, driven by a small seeded generator so that a failure can be repeated
//! from the seed it prints.

use std::collections::BTreeMap;

use tracer_proto::*;

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }

    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }

    /// A number that is often at an edge.
    fn number(&mut self) -> u64 {
        match self.below(6) {
            0 => 0,
            1 => u64::MAX,
            2 => u64::from(u32::MAX),
            3 => self.below(100),
            _ => self.next(),
        }
    }

    /// Text with everything JSON has to escape, and characters of every width.
    fn text(&mut self) -> String {
        const PIECES: [&str; 14] = ["a", "Z", "0", " ", "\"", "\\", "/", "\n", "\u{0}", "é", "名", "🦀", "\u{202e}", "{\"type\":"];
        (0..self.below(12)).map(|_| PIECES[self.below(PIECES.len() as u64) as usize]).collect()
    }

    fn maybe<T>(&mut self, make: impl FnOnce(&mut Self) -> T) -> Option<T> {
        if self.chance(2) { Some(make(self)) } else { None }
    }

    /// A finite number with at most fifteen significant digits. Longer ones can come back one unit in
    /// the last place away: `serde_json` reads floats approximately unless its `float_roundtrip` feature
    /// is on, which this workspace leaves off. Gauges and float properties are measurements, so that is
    /// accepted, and the round-trip test stays within what is exact.
    fn float(&mut self) -> f64 {
        match self.below(5) {
            0 => 0.0,
            1 => -0.5,
            2 => 1e300,
            3 => -1e-300,
            _ => self.below(1_000_000_000) as f64 / 8.0,
        }
    }
}

fn frames(rng: &mut Rng) -> FrameStats {
    let mut stats = FrameStats::empty();
    for _ in 0..rng.below(40) {
        let us = match rng.below(5) {
            0 => 0,
            1 => u32::MAX,
            2 => FRAME_BUCKET_BOUNDS_US[rng.below(FRAME_BUCKET_BOUNDS_US.len() as u64) as usize] + rng.below(3) as u32 - 1,
            _ => rng.below(1_000_000) as u32,
        };
        stats.record(us);
    }
    stats
}

fn perf(rng: &mut Rng) -> Perf {
    Perf {
        window_s: rng.number() as u32,
        frames: rng.maybe(frames),
        cpu_pct: rng.maybe(|rng| rng.below(80_000) as f32 / 100.0),
        rss_mb: rng.maybe(|rng| rng.number() as u32),
        rss_peak_mb: rng.maybe(|rng| rng.number() as u32),
        startup_ms: rng.maybe(|rng| rng.number() as u32),
        counters: (0..rng.below(4)).map(|_| (rng.text(), rng.number())).collect(),
        gauges: (0..rng.below(4)).map(|_| (rng.text(), rng.float())).collect(),
    }
}

fn totals(rng: &mut Rng) -> SessionTotals {
    SessionTotals { seq: rng.number() as u32, duration_s: rng.number(), focused_s: rng.number(), active_s: rng.number(), idle_s: rng.number() }
}

fn body(rng: &mut Rng) -> ItemBody {
    match rng.below(8) {
        0 => ItemBody::SessionStart(SessionStart {
            catalog_hash: rng.maybe(Rng::text),
            catalog: rng
                .maybe(|rng| Catalog { events: (0..rng.below(4)).map(|_| CatalogEvent { name: rng.text(), category: rng.text() }).collect() }),
            listening: (0..rng.below(3)).map(|_| rng.text()).collect(),
            signals: (0..rng.below(3)).map(|_| rng.text()).collect(),
        }),
        1 => ItemBody::SessionHeartbeat(totals(rng)),
        2 => ItemBody::SessionEnd(SessionEnd {
            status: [SessionStatus::Ok, SessionStatus::Exited, SessionStatus::Crashed, SessionStatus::Abnormal][rng.below(4) as usize],
            totals: totals(rng),
            errors: rng.number() as u32,
        }),
        3 => {
            let mut props = BTreeMap::new();
            for _ in 0..rng.below(5) {
                let value = match rng.below(4) {
                    0 => PropValue::Bool(rng.chance(2)),
                    1 => PropValue::Int(rng.number() as i64),
                    2 => PropValue::Float(rng.float() + 0.25),
                    _ => PropValue::Str(rng.text()),
                };
                props.insert(rng.text(), value);
            }
            ItemBody::Event(Event { name: rng.text(), category: rng.text(), dynamic: rng.chance(2), props })
        }
        4 => ItemBody::Perf(perf(rng)),
        5 => ItemBody::Crash(Crash {
            kind: [CrashKind::Panic, CrashKind::Handled, CrashKind::Hang, CrashKind::UncleanExit, CrashKind::ChildExit, CrashKind::Other]
                [rng.below(6) as usize],
            fatal: rng.chance(2),
            message: rng.maybe(Rng::text),
            location: rng.maybe(Rng::text),
            thread: rng.maybe(Rng::text),
            backtrace: rng.maybe(Rng::text),
            blocked_ms: rng.maybe(|rng| rng.number() as u32),
            exit_code: rng.maybe(|rng| rng.number() as i32),
            signal: rng.maybe(|rng| rng.number() as i32),
            context: rng.maybe(Rng::text),
            breadcrumbs: (0..rng.below(3))
                .map(|_| Breadcrumb { ts: rng.number(), level: rng.text(), target: rng.text(), message: rng.text() })
                .collect(),
            perf: rng.maybe(perf),
        }),
        6 => ItemBody::Drops(Drops { reasons: (0..rng.below(4)).map(|_| (rng.text(), rng.number())).collect() }),
        _ => ItemBody::Unknown,
    }
}

fn envelope(rng: &mut Rng) -> Envelope {
    Envelope {
        schema: rng.number() as u32,
        app: AppInfo {
            name: rng.text(),
            version: rng.text(),
            build: rng.maybe(Rng::text),
            process: [ProcessKind::Gui, ProcessKind::Cli, ProcessKind::Service, ProcessKind::Child, ProcessKind::Other][rng.below(5) as usize],
        },
        install_id: rng.text(),
        session_id: rng.text(),
        device: rng.maybe(|rng| Device {
            os_name: rng.text(),
            os_version: rng.maybe(Rng::text),
            arch: rng.text(),
            cpu_brand: rng.maybe(Rng::text),
            cpu_cores: rng.maybe(|rng| rng.number() as u32),
            ram_mb: rng.maybe(Rng::number),
            gpu: rng.maybe(|rng| Gpu { name: rng.text(), vendor: rng.maybe(Rng::text), backend: rng.maybe(Rng::text), driver: rng.maybe(Rng::text) }),
            locale: rng.maybe(Rng::text),
        }),
        sent_at: rng.number(),
        items: (0..rng.below(6)).map(|_| Item { id: rng.text(), ts: rng.number(), body: body(rng) }).collect(),
    }
}

#[test]
fn any_envelope_round_trips_through_json() {
    for seed in 0..3_000 {
        let original = envelope(&mut Rng(seed));
        let json = serde_json::to_string(&original).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
        let parsed: Envelope = serde_json::from_str(&json).unwrap_or_else(|error| panic!("seed {seed}: {error}\n{json}"));
        assert_eq!(parsed, original, "seed {seed}\n{json}");
        // And once more: what was written again is the same text.
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json, "seed {seed}");
        for item in &original.items {
            assert!(json.contains(&format!("\"type\":\"{}\"", item.body.type_name())), "seed {seed}: {}", item.body.type_name());
        }
    }
}

#[test]
fn damaged_json_parses_or_errors_but_never_panics() {
    let mut parsed = 0;
    for seed in 0..4_000 {
        let mut rng = Rng(seed);
        let mut bytes = serde_json::to_vec(&envelope(&mut rng)).unwrap();
        for _ in 0..=rng.below(3) {
            let at = rng.below(bytes.len() as u64) as usize;
            match rng.below(6) {
                0 => bytes[at] = rng.next() as u8,
                1 => bytes.truncate(at),
                2 => {
                    let end = (at + rng.below(20) as usize).min(bytes.len());
                    bytes.drain(at..end);
                }
                3 => {
                    let noise: Vec<u8> = (0..rng.below(12)).map(|_| rng.next() as u8).collect();
                    bytes.splice(at..at, noise);
                }
                4 => {
                    let end = (at + rng.below(40) as usize).min(bytes.len());
                    let copy = bytes[at..end].to_vec();
                    bytes.splice(at..at, copy);
                }
                _ => {
                    let token: &[u8] =
                        [&b"null"[..], b"[]", b"{}", b"1e999", b"-0", b"\"\\ud800\"", b"99999999999999999999999999"][rng.below(7) as usize];
                    let end = (at + rng.below(6) as usize).min(bytes.len());
                    bytes.splice(at..end, token.iter().copied());
                }
            }
            if bytes.is_empty() {
                break;
            }
        }
        parsed += usize::from(serde_json::from_slice::<Envelope>(&bytes).is_ok());
        let _ = serde_json::from_slice::<IngestResponse>(&bytes);
        let _ = serde_json::from_slice::<ForgetRequest>(&bytes);
        let _ = serde_json::from_slice::<Item>(&bytes);
    }
    // Some damage is harmless (inside a string, say); most is not. Both paths were taken.
    assert!(parsed > 50 && parsed < 3_900, "{parsed} of 4000 damaged bodies still parsed");
}

#[test]
fn unknown_fields_anywhere_are_ignored() {
    for seed in 0..300 {
        let original = envelope(&mut Rng(seed));
        let mut value = serde_json::to_value(&original).unwrap();
        fn sprinkle(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(map) => {
                    for inner in map.values_mut() {
                        sprinkle(inner);
                    }
                    // Not into maps whose keys are data: properties, counters, gauges, drop reasons.
                    if map.contains_key("schema") || map.contains_key("ts") || map.contains_key("os_name") || map.contains_key("version") {
                        map.insert("field_from_the_future".to_owned(), serde_json::json!({"nested": [1, 2, 3]}));
                    }
                }
                serde_json::Value::Array(list) => list.iter_mut().for_each(sprinkle),
                _ => {}
            }
        }
        sprinkle(&mut value);
        let parsed: Envelope = serde_json::from_value(value).unwrap_or_else(|error| panic!("seed {seed}: {error}"));
        assert_eq!(parsed, original, "seed {seed}");
    }
}

#[test]
fn frame_stats_keep_their_invariants() {
    for seed in 0..2_000 {
        let mut rng = Rng(seed);
        let (a, b, c) = (frames(&mut rng), frames(&mut rng), frames(&mut rng));
        for stats in [&a, &b, &c] {
            assert_eq!(stats.buckets.len(), FRAME_BUCKETS, "seed {seed}");
            assert_eq!(stats.count, stats.buckets.iter().sum::<u64>(), "seed {seed}: count is the sum of the buckets");
            assert!(stats.frozen <= stats.slow && stats.slow <= stats.count, "seed {seed}");
            // Quantiles never decrease as q grows, never exceed the largest frame, and exist iff there are frames.
            let quantiles: Vec<Option<u32>> =
                [-1.0, 0.0, 0.01, 0.25, 0.5, 0.5, 0.9, 0.99, 1.0, 2.0, f64::NAN].iter().map(|q| stats.quantile_us(*q)).collect();
            assert!(quantiles.iter().all(|q| q.is_some() == (stats.count > 0)), "seed {seed}");
            let known: Vec<u32> = quantiles.iter().take(10).flatten().copied().collect();
            assert!(known.windows(2).all(|pair| pair[0] <= pair[1]), "seed {seed}: {known:?}");
            assert!(known.iter().all(|q| *q <= stats.max_us), "seed {seed}");
        }
        let merged = |first: &FrameStats, second: &FrameStats| {
            let mut sum = first.clone();
            sum.merge(second);
            sum
        };
        assert_eq!(merged(&a, &b), merged(&b, &a), "seed {seed}: merge is commutative");
        assert_eq!(merged(&merged(&a, &b), &c), merged(&a, &merged(&b, &c)), "seed {seed}: merge is associative");
        assert_eq!(merged(&a, &FrameStats::empty()), a, "seed {seed}: merging nothing changes nothing");
        // Merging is the same as having recorded everything into one.
        let all = merged(&merged(&a, &b), &c);
        assert_eq!(all.count, a.count + b.count + c.count);
        assert_eq!(all.max_us, a.max_us.max(b.max_us).max(c.max_us));
        // A histogram from a peer with too few or too many buckets does not break the merge.
        let mut short = b.clone();
        short.buckets.truncate(rng.below(FRAME_BUCKETS as u64) as usize);
        let mut long = b.clone();
        long.buckets.extend([7, 7, 7]);
        for odd in [&short, &long, &FrameStats::default()] {
            let mut into = a.clone();
            into.merge(odd);
            assert!(into.buckets.len() >= FRAME_BUCKETS, "seed {seed}");
            let mut from = odd.clone();
            from.merge(&a);
            from.record(5);
            assert!(from.quantile_us(0.5).is_some());
        }
    }
}

#[test]
fn frame_buckets_are_ordered_and_cover_every_duration() {
    let mut previous = 0;
    for us in (0..6_000_000u32)
        .step_by(997)
        .chain([0, 1, u32::MAX - 1, u32::MAX])
        .chain(FRAME_BUCKET_BOUNDS_US.iter().flat_map(|bound| [bound - 1, *bound, bound + 1]))
    {
        let index = FrameStats::bucket_index(us);
        assert!(index < FRAME_BUCKETS, "{us}");
        if let Some(bound) = FRAME_BUCKET_BOUNDS_US.get(index) {
            assert!(us <= *bound, "{us} is in a bucket whose bound is {bound}");
        }
        if index > 0 {
            assert!(us > FRAME_BUCKET_BOUNDS_US[index - 1], "{us} belongs in an earlier bucket");
        }
        let _ = previous;
        previous = index;
    }
    assert!(FRAME_BUCKET_BOUNDS_US.windows(2).all(|pair| pair[0] < pair[1]));
    // Both thresholds are bucket bounds, which is what lets a histogram alone say how many frames were slow.
    assert!(FRAME_BUCKET_BOUNDS_US.contains(&SLOW_FRAME_US) && FRAME_BUCKET_BOUNDS_US.contains(&FROZEN_FRAME_US));
}

#[test]
fn frame_stats_saturate_at_the_extremes() {
    let mut full = FrameStats::empty();
    full.count = u64::MAX;
    full.sum_us = u64::MAX;
    full.slow = u64::MAX;
    full.frozen = u64::MAX;
    full.max_us = u32::MAX;
    full.buckets = vec![u64::MAX; FRAME_BUCKETS];
    let mut twice = full.clone();
    twice.merge(&full);
    twice.record(u32::MAX);
    twice.record(0);
    assert_eq!(twice.count, u64::MAX);
    assert_eq!(twice.sum_us, u64::MAX);
    assert!(twice.buckets.iter().all(|bucket| *bucket == u64::MAX));
    // With every bucket full the running total is full from the first bucket on, so no quantile can be
    // told apart from another; what matters is that asking is safe and stays within the largest frame.
    for q in [0.0, 0.5, 1.0] {
        assert!(twice.quantile_us(q).is_some_and(|us| us <= twice.max_us));
    }
}

/// The rule for names, written the plain way.
fn reference_name_is_valid(name: &str) -> bool {
    let separator = |c: char| c == '_' || c == '.';
    let chars: Vec<char> = name.chars().collect();
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && chars[0].is_ascii_lowercase()
        && chars.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || separator(*c))
        && !chars.windows(2).any(|pair| separator(pair[0]) && separator(pair[1]))
        && !separator(chars[chars.len() - 1])
}

#[test]
fn name_validation_agrees_with_the_plain_rule_on_random_strings() {
    const ALPHABET: [&str; 14] = ["a", "b", "z", "m", "0", "9", "_", ".", "A", "-", " ", "é", "名", "\u{0}"];
    let (mut valid, mut invalid) = (0, 0);
    for seed in 0..60_000 {
        let mut rng = Rng(seed);
        // Mostly the characters a valid name is made of, so that many names are valid or nearly so.
        let length = if rng.chance(8) { 60 + rng.below(10) } else { rng.below(14) };
        let name: String = (0..length).map(|_| ALPHABET[if rng.chance(12) { rng.below(14) } else { rng.below(8) } as usize]).collect();
        let expected = reference_name_is_valid(&name);
        assert_eq!(validate_name(&name).is_ok(), expected, "seed {seed}: {name:?}");
        valid += u32::from(expected);
        invalid += u32::from(!expected);
        let other: String = if rng.chance(2) { name.clone() } else { name.chars().rev().collect() };
        assert_eq!(const_str_eq(&name, &other), name == other, "seed {seed}");
    }
    assert!(valid > 3_000 && invalid > 3_000, "{valid} valid and {invalid} invalid names were tried");
    // At the length limit and one past it.
    let at_limit = "a".repeat(MAX_NAME_LEN);
    assert_eq!(validate_name(&at_limit), Ok(()));
    assert_eq!(validate_name(&format!("{at_limit}a")), Err(NameError::TooLong));
    assert_eq!(validate_name(""), Err(NameError::Empty));
    assert_eq!(validate_name("1a"), Err(NameError::BadStart));
    assert_eq!(validate_name("a-b"), Err(NameError::BadChar));
    assert_eq!(validate_name("a..b"), Err(NameError::BadSeparator));
    assert_eq!(validate_name("a."), Err(NameError::BadSeparator));
}

#[test]
fn every_enum_has_a_stable_name_and_unknown_values_fall_back() {
    for signal in Signal::ALL {
        assert_eq!(serde_json::to_string(&signal).unwrap(), format!("\"{}\"", signal.as_str()));
        let _ = signal.consent();
    }
    for status in [SessionStatus::Ok, SessionStatus::Exited, SessionStatus::Crashed, SessionStatus::Abnormal] {
        assert_eq!(serde_json::to_string(&status).unwrap(), format!("\"{}\"", status.as_str()));
        assert_eq!(status.is_terminal(), status != SessionStatus::Ok);
    }
    for kind in [CrashKind::Panic, CrashKind::Handled, CrashKind::Hang, CrashKind::UncleanExit, CrashKind::ChildExit, CrashKind::Other] {
        assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{}\"", kind.as_str()));
    }
    assert_eq!(serde_json::from_str::<CrashKind>("\"from_the_future\"").unwrap(), CrashKind::Other);
    assert_eq!(serde_json::from_str::<ProcessKind>("\"from_the_future\"").unwrap(), ProcessKind::Other);
    assert!(serde_json::from_str::<SessionStatus>("\"from_the_future\"").is_err());
    let unknown: Item = serde_json::from_str(r#"{"id":"x","ts":1,"type":"from_the_future","anything":[1,2]}"#).unwrap();
    assert_eq!(unknown.body, ItemBody::Unknown);
    assert_eq!(unknown.body.type_name(), "unknown");
}
