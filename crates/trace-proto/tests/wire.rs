//! Pins the wire format. If one of these fails, an existing client or server would break:
//! add a new optional field instead of changing an old one.

use std::collections::BTreeMap;

use ashlar_trace_proto::*;

const FULL: &str = include_str!("fixtures/envelope_full.json");
const FUTURE: &str = include_str!("fixtures/envelope_future.json");

#[test]
fn full_fixture_round_trips() {
    let envelope: Envelope = serde_json::from_str(FULL).unwrap();
    let rewritten = serde_json::to_value(&envelope).unwrap();
    let original: serde_json::Value = serde_json::from_str(FULL).unwrap();
    assert_eq!(rewritten, original);
}

#[test]
fn full_fixture_has_every_item_type() {
    let envelope: Envelope = serde_json::from_str(FULL).unwrap();
    let types: Vec<&str> = envelope.items.iter().map(|item| item.body.type_name()).collect();
    assert_eq!(types, ["session_start", "event", "event", "session_heartbeat", "perf", "crash", "drops", "session_end"]);
    assert_eq!(envelope.schema, SCHEMA_VERSION);
}

#[test]
fn unknown_fields_types_and_variants_are_tolerated() {
    let envelope: Envelope = serde_json::from_str(FUTURE).unwrap();
    assert_eq!(envelope.app.process, ProcessKind::Other);
    assert_eq!(envelope.device, None);
    assert_eq!(envelope.items[0].body, ItemBody::Unknown);
    let ItemBody::Crash(crash) = &envelope.items[1].body else { panic!("expected a crash") };
    assert_eq!(crash.kind, CrashKind::Other);
    let ItemBody::Event(event) = &envelope.items[2].body else { panic!("expected an event") };
    assert_eq!(event.name, "file.open");
    assert!(!event.dynamic);
}

#[test]
fn signal_names_match_their_serialized_form() {
    for signal in Signal::ALL {
        assert_eq!(serde_json::to_value(signal).unwrap(), signal.as_str());
    }
}

#[test]
fn frame_stats_record_merge_and_quantiles() {
    let mut stats = FrameStats::empty();
    for us in [4_000, 11_000, 710_000] {
        stats.record(us);
    }
    assert_eq!((stats.count, stats.slow, stats.frozen, stats.max_us), (3, 1, 1, 710_000));
    assert_eq!((stats.over_budget, stats.over_budget_frames()), (Some(2), 2));
    assert_eq!(stats.buckets.len(), FRAME_BUCKETS);
    assert_eq!(stats.quantile_us(0.5), Some(11_111));
    assert_eq!(stats.quantile_us(1.0), Some(710_000));
    assert_eq!(FrameStats::empty().quantile_us(0.5), None);

    let mut merged = FrameStats::default();
    merged.merge(&stats);
    merged.merge(&stats);
    assert_eq!(merged.count, 6);
    assert_eq!(merged.buckets.iter().sum::<u64>(), 6);
    assert_eq!(merged.over_budget, Some(4));
}

#[test]
fn display_budget_count_is_additive() {
    // the 120 Hz budget is a bucket bound, so histograms without the count still give it exactly
    assert!(FRAME_BUCKET_BOUNDS_US.contains(&DISPLAY_BUDGET_US));
    const { assert!(DISPLAY_BUDGET_US < SLOW_FRAME_US) };
    let mut stats = FrameStats::empty();
    for us in [DISPLAY_BUDGET_US, DISPLAY_BUDGET_US + 1, 16_000, 20_000] {
        stats.record(us);
    }
    assert_eq!((stats.over_budget, stats.frames_over(DISPLAY_BUDGET_US), stats.slow), (Some(3), 3, 1));

    // an older sender leaves the field out: it reads as None and is counted from the buckets
    let old = r#"{"count":3,"sum_us":724000,"max_us":710000,"slow":1,"frozen":1,"buckets":[0,0,1,0,0,1,0,0,0,0,0,0,0,1,0,0]}"#;
    let mut from_old: FrameStats = serde_json::from_str(old).unwrap();
    assert_eq!((from_old.over_budget, from_old.over_budget_frames()), (None, 2));
    // ... and writing it back adds nothing an older reader has not seen
    assert_eq!(serde_json::to_value(&from_old).unwrap(), serde_json::from_str::<serde_json::Value>(old).unwrap());
    from_old.record(9_000);
    assert_eq!(from_old.over_budget, Some(3));
    let mut merged = FrameStats::default();
    merged.merge(&serde_json::from_str(old).unwrap());
    merged.merge(&stats);
    assert_eq!(merged.over_budget, Some(5));

    // an older reader (the fields before this one) still parses what a new sender writes
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct OldFrameStats {
        count: u64,
        sum_us: u64,
        max_us: u32,
        slow: u64,
        frozen: u64,
        buckets: Vec<u64>,
    }
    let new = serde_json::to_string(&stats).unwrap();
    assert!(new.contains("\"over_budget\":3"), "{new}");
    let parsed: OldFrameStats = serde_json::from_str(&new).unwrap();
    assert_eq!(parsed.count, 4);
}

#[test]
fn incidents_are_additive() {
    // an older sender leaves them out
    let old: Perf = serde_json::from_str(r#"{"window_s":60}"#).unwrap();
    assert!(old.incidents.is_empty());
    assert!(!serde_json::to_string(&old).unwrap().contains("incidents"), "an empty list is not written");

    let perf =
        Perf { window_s: 60, incidents: vec![Incident { rule: IncidentRule::Stall, at_ms: 1_500, count: 2, value: 400_000 }], ..Perf::default() };
    let new = serde_json::to_string(&perf).unwrap();
    assert!(new.contains(r#""incidents":[{"rule":"stall","at_ms":1500,"count":2,"value":400000}]"#), "{new}");
    assert_eq!(serde_json::from_str::<Perf>(&new).unwrap(), perf);

    // an older reader (the fields before this one) still parses what a new sender writes
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct OldPerf {
        window_s: u32,
        #[serde(default)]
        counters: BTreeMap<String, u64>,
    }
    let parsed: OldPerf = serde_json::from_str(&new).unwrap();
    assert_eq!(parsed.window_s, 60);

    // a rule this version does not know reads as Other
    let future: Perf = serde_json::from_str(r#"{"window_s":1,"incidents":[{"rule":"gpu_reset","at_ms":0,"count":1,"value":0}]}"#).unwrap();
    assert_eq!(future.incidents[0].rule, IncidentRule::Other);
    for rule in [IncidentRule::LongFrames, IncidentRule::Stall, IncidentRule::MemoryGrowth] {
        assert_eq!(serde_json::to_string(&rule).unwrap(), format!("\"{}\"", rule.name()));
    }
}

#[test]
fn name_validation() {
    for good in ["open", "file.open", "edit_undo", "a1.b2_c3"] {
        assert_eq!(validate_name(good), Ok(()), "{good}");
    }
    assert_eq!(validate_name(""), Err(NameError::Empty));
    assert_eq!(validate_name(&"a".repeat(MAX_NAME_LEN + 1)), Err(NameError::TooLong));
    assert_eq!(validate_name("1abc"), Err(NameError::BadStart));
    assert_eq!(validate_name("File.open"), Err(NameError::BadStart));
    assert_eq!(validate_name("file open"), Err(NameError::BadChar));
    assert_eq!(validate_name("file/open"), Err(NameError::BadChar));
    assert_eq!(validate_name("file..open"), Err(NameError::BadSeparator));
    assert_eq!(validate_name("file."), Err(NameError::BadSeparator));
    assert!(const_str_eq("abc", "abc") && !const_str_eq("abc", "abd") && !const_str_eq("abc", "ab"));
}

#[test]
fn frame_stats_arithmetic_saturates_instead_of_overflowing() {
    let mut huge = ashlar_trace_proto::FrameStats::empty();
    huge.count = u64::MAX;
    huge.sum_us = u64::MAX;
    huge.slow = u64::MAX;
    huge.frozen = u64::MAX;
    huge.over_budget = Some(u64::MAX);
    huge.buckets = vec![u64::MAX; ashlar_trace_proto::FRAME_BUCKETS];
    let mut total = huge.clone();
    total.merge(&huge);
    assert_eq!((total.count, total.sum_us, total.slow, total.frozen), (u64::MAX, u64::MAX, u64::MAX, u64::MAX));
    assert_eq!(total.over_budget, Some(u64::MAX));
    huge.over_budget = None;
    assert_eq!(huge.over_budget_frames(), u64::MAX, "counting the buckets saturates too");
    assert!(total.buckets.iter().all(|bucket| *bucket == u64::MAX));
    total.record(800_000);
    assert_eq!(total.count, u64::MAX);
    assert_eq!(total.sum_us, u64::MAX);
}
