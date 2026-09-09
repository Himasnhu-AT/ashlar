//! Pins the wire format. If one of these fails, an existing client or server would break:
//! add a new optional field instead of changing an old one.

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
    assert_eq!(stats.buckets.len(), FRAME_BUCKETS);
    assert_eq!(stats.quantile_us(0.5), Some(11_111));
    assert_eq!(stats.quantile_us(1.0), Some(710_000));
    assert_eq!(FrameStats::empty().quantile_us(0.5), None);

    let mut merged = FrameStats::default();
    merged.merge(&stats);
    merged.merge(&stats);
    assert_eq!(merged.count, 6);
    assert_eq!(merged.buckets.iter().sum::<u64>(), 6);
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
    huge.buckets = vec![u64::MAX; ashlar_trace_proto::FRAME_BUCKETS];
    let mut total = huge.clone();
    total.merge(&huge);
    assert_eq!((total.count, total.sum_us, total.slow, total.frozen), (u64::MAX, u64::MAX, u64::MAX, u64::MAX));
    assert!(total.buckets.iter().all(|bucket| *bucket == u64::MAX));
    total.record(800_000);
    assert_eq!(total.count, u64::MAX);
    assert_eq!(total.sum_us, u64::MAX);
}
