//! Pure ingest logic: validation, sanitising, clock-drift correction and crash fingerprints.
//!
//! Every field of an envelope is hostile input. [`prepare`] turns a parsed [`Envelope`] into a [`Prepared`]
//! batch in which strings are truncated, numbers are clamped, names are valid and times are corrected, so the
//! store can write it without further checks. It does no I/O.

use std::collections::BTreeMap;

use tracer_proto::{
    Breadcrumb, CatalogEvent, Crash, CrashKind, Device, Envelope, Event, FRAME_BUCKETS, FrameStats, Gpu, ItemBody, MAX_NAME_LEN, MAX_PROP_STR_LEN,
    MAX_PROPS, Perf, PropValue, SessionEnd, SessionStatus, SessionTotals, validate_name,
};

pub const DAY_MS: i64 = 86_400_000;
/// Item times are only corrected when the client clock is off by more than this.
pub const DRIFT_THRESHOLD_MS: i64 = 300_000;
/// Items beyond this many in one envelope are rejected.
pub const MAX_ITEMS: usize = 2_000;
pub const MAX_ID_LEN: usize = 128;
pub const MAX_MESSAGE_LEN: usize = 2 * 1024;
pub const MAX_BACKTRACE_LEN: usize = 32 * 1024;
pub const MAX_BREADCRUMBS: usize = 100;
pub const MAX_CATALOG_EVENTS: usize = 2_000;
const MAX_FIELD_LEN: usize = 128;
const MAX_VERSION_LEN: usize = 64;
const MAX_MAPPED: usize = 32;
/// Counters are clamped so that sums over millions of rows cannot overflow an `i64`.
pub const COUNTER_MAX: u64 = 1 << 40;
/// Ten years, in seconds.
const SECONDS_MAX: u64 = 315_360_000;
const FINGERPRINT_FRAMES: usize = 5;

/// A validated, sanitised envelope ready to be stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    pub install_id: String,
    pub session_id: String,
    pub version: String,
    pub build: Option<String>,
    pub process: &'static str,
    pub device: Option<Device>,
    /// When the session began, in corrected Unix milliseconds.
    pub started_at: i64,
    /// Time of the newest item (or of receipt when there are none).
    pub last_seen: i64,
    pub received_at: i64,
    pub items: Vec<PreparedItem>,
    /// Items refused during preparation: bad names, unknown types, over the item cap.
    pub rejected: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedItem {
    pub id: String,
    /// Corrected Unix milliseconds.
    pub ts: i64,
    pub body: PreparedBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PreparedBody {
    SessionStart { catalog_hash: Option<String>, catalog: Option<Vec<CatalogEvent>> },
    Heartbeat(SessionTotals),
    End { status: SessionStatus, totals: SessionTotals, errors: u32 },
    Event(Event),
    Perf(Perf),
    Crash(Box<PreparedCrash>),
    Drops(Vec<(String, u64)>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedCrash {
    pub crash: Crash,
    pub fingerprint: String,
    pub title: String,
}

/// Why an envelope as a whole is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrepareError {
    #[error("install_id is missing or malformed")]
    BadInstallId,
    #[error("session_id is missing or malformed")]
    BadSessionId,
    #[error("app.version is missing")]
    BadVersion,
}

/// Validates and sanitises `envelope`. `received_at` is the server clock in Unix milliseconds.
pub fn prepare(envelope: Envelope, received_at: i64) -> Result<Prepared, PrepareError> {
    if !valid_id(&envelope.install_id) {
        return Err(PrepareError::BadInstallId);
    }
    if !valid_id(&envelope.session_id) {
        return Err(PrepareError::BadSessionId);
    }
    let version = clean(&envelope.app.version, MAX_VERSION_LEN);
    if version.is_empty() {
        return Err(PrepareError::BadVersion);
    }
    let sent_at = envelope.sent_at;
    let mut rejected = 0u32;
    let mut items = Vec::new();
    let mut started_at: Option<i64> = None;
    let mut start_item_at: Option<i64> = None;
    let mut last_seen: Option<i64> = None;

    for (index, item) in envelope.items.into_iter().enumerate() {
        if index >= MAX_ITEMS || !valid_item_id(&item.id) {
            rejected = rejected.saturating_add(1);
            continue;
        }
        let ts = corrected_ts(item.ts, sent_at, received_at);
        let body = match item.body {
            ItemBody::SessionStart(start) => {
                start_item_at = Some(start_item_at.map_or(ts, |at| at.min(ts)));
                PreparedBody::SessionStart {
                    catalog_hash: start.catalog_hash.map(|hash| clean(&hash, MAX_VERSION_LEN)).filter(|h| !h.is_empty()),
                    catalog: start.catalog.map(|catalog| sanitize_catalog(catalog.events)),
                }
            }
            ItemBody::SessionHeartbeat(totals) => PreparedBody::Heartbeat(clamp_totals(totals)),
            ItemBody::SessionEnd(SessionEnd { status, totals, errors }) => {
                PreparedBody::End { status, totals: clamp_totals(totals), errors: errors.min(1_000_000) }
            }
            ItemBody::Event(event) => match sanitize_event(event) {
                Some(event) => PreparedBody::Event(event),
                None => {
                    rejected = rejected.saturating_add(1);
                    continue;
                }
            },
            ItemBody::Perf(perf) => PreparedBody::Perf(sanitize_perf(perf)),
            ItemBody::Crash(crash) => {
                let crash = sanitize_crash(crash);
                let print = fingerprint(crash.kind, crash.message.as_deref(), crash.location.as_deref(), crash.backtrace.as_deref());
                PreparedBody::Crash(Box::new(PreparedCrash { crash, fingerprint: print.hash, title: print.title }))
            }
            ItemBody::Drops(drops) => PreparedBody::Drops(
                drops
                    .reasons
                    .into_iter()
                    .filter_map(|(reason, count)| {
                        let reason = clean(&reason, MAX_NAME_LEN);
                        (!reason.is_empty()).then_some((reason, count.min(COUNTER_MAX)))
                    })
                    .take(MAX_MAPPED)
                    .collect(),
            ),
            ItemBody::Unknown => {
                rejected = rejected.saturating_add(1);
                continue;
            }
        };
        started_at = Some(started_at.map_or(ts, |at| at.min(ts)));
        last_seen = Some(last_seen.map_or(ts, |at| at.max(ts)));
        items.push(PreparedItem { id: item.id, ts, body });
    }

    Ok(Prepared {
        install_id: envelope.install_id,
        session_id: envelope.session_id,
        version,
        build: envelope.app.build.map(|build| clean(&build, MAX_FIELD_LEN)).filter(|build| !build.is_empty()),
        process: process_name(envelope.app.process),
        device: envelope.device.map(sanitize_device),
        started_at: start_item_at.or(started_at).unwrap_or(received_at),
        last_seen: last_seen.unwrap_or(received_at),
        received_at,
        items,
        rejected,
    })
}

fn process_name(kind: tracer_proto::ProcessKind) -> &'static str {
    use tracer_proto::ProcessKind::*;
    match kind {
        Gui => "gui",
        Cli => "cli",
        Service => "service",
        Child => "child",
        Other => "other",
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn valid_item_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && !id.chars().any(char::is_control)
}

/// Moves a client timestamp onto the server clock when the client clock is more than five minutes off, then
/// clamps it to at most one day in the future (and never before the epoch).
pub fn corrected_ts(ts: u64, sent_at: u64, received_at: i64) -> i64 {
    let ts = i64::try_from(ts).unwrap_or(i64::MAX);
    let sent_at = i64::try_from(sent_at).unwrap_or(i64::MAX);
    let drift = received_at.saturating_sub(sent_at);
    let ts = if drift.unsigned_abs() > DRIFT_THRESHOLD_MS as u64 { ts.saturating_add(drift) } else { ts };
    ts.min(received_at.saturating_add(DAY_MS)).max(0)
}

/// Truncates to at most `max` bytes on a character boundary.
pub fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// A single-line field: control characters removed, trimmed, truncated.
pub fn clean(text: &str, max: usize) -> String {
    let stripped: String = text.chars().filter(|c| !c.is_control()).collect();
    truncate(stripped.trim(), max).to_string()
}

/// Free text such as a message or backtrace: only NUL bytes removed, then truncated.
pub fn clean_text(text: &str, max: usize) -> String {
    let stripped: String = text.chars().filter(|c| *c != '\0').collect();
    truncate(&stripped, max).to_string()
}

fn clamp_secs(value: u64) -> u64 {
    value.min(SECONDS_MAX)
}

fn clamp_totals(totals: SessionTotals) -> SessionTotals {
    SessionTotals {
        seq: totals.seq,
        duration_s: clamp_secs(totals.duration_s),
        focused_s: clamp_secs(totals.focused_s),
        active_s: clamp_secs(totals.active_s),
        idle_s: clamp_secs(totals.idle_s),
    }
}

fn sanitize_catalog(events: Vec<CatalogEvent>) -> Vec<CatalogEvent> {
    let mut seen = std::collections::BTreeSet::new();
    events
        .into_iter()
        .filter(|event| validate_name(&event.name).is_ok() && validate_name(&event.category).is_ok())
        .filter(|event| seen.insert(event.name.clone()))
        .take(MAX_CATALOG_EVENTS)
        .collect()
}

/// `None` when the name or category is invalid.
fn sanitize_event(event: Event) -> Option<Event> {
    validate_name(&event.name).ok()?;
    validate_name(&event.category).ok()?;
    let props: BTreeMap<String, PropValue> = event
        .props
        .into_iter()
        .filter(|(key, _)| validate_name(key).is_ok())
        .filter_map(|(key, value)| {
            let value = match value {
                PropValue::Str(text) => PropValue::Str(clean(&text, MAX_PROP_STR_LEN)),
                PropValue::Float(float) if !float.is_finite() => return None,
                other => other,
            };
            Some((key, value))
        })
        .take(MAX_PROPS)
        .collect();
    Some(Event { name: event.name, category: event.category, dynamic: event.dynamic, props })
}

fn sanitize_perf(perf: Perf) -> Perf {
    let frames = perf.frames.map(|frames| {
        let mut buckets: Vec<u64> = frames.buckets.into_iter().map(|count| count.min(COUNTER_MAX)).take(FRAME_BUCKETS).collect();
        buckets.resize(FRAME_BUCKETS, 0);
        FrameStats {
            count: frames.count.min(COUNTER_MAX),
            sum_us: frames.sum_us.min(COUNTER_MAX * 1_000),
            max_us: frames.max_us,
            slow: frames.slow.min(COUNTER_MAX),
            frozen: frames.frozen.min(COUNTER_MAX),
            buckets,
        }
    });
    Perf {
        window_s: perf.window_s.min(86_400),
        frames,
        cpu_pct: perf.cpu_pct.filter(|cpu| cpu.is_finite() && (0.0..=100_000.0).contains(cpu)),
        rss_mb: perf.rss_mb,
        rss_peak_mb: perf.rss_peak_mb,
        startup_ms: perf.startup_ms,
        counters: perf
            .counters
            .into_iter()
            .map(|(key, value)| (clean(&key, MAX_NAME_LEN), value.min(COUNTER_MAX)))
            .filter(|(key, _)| !key.is_empty())
            .take(MAX_MAPPED)
            .collect(),
        gauges: perf
            .gauges
            .into_iter()
            .map(|(key, value)| (clean(&key, MAX_NAME_LEN), value))
            .filter(|(key, value)| !key.is_empty() && value.is_finite())
            .take(MAX_MAPPED)
            .collect(),
    }
}

fn sanitize_crash(crash: Crash) -> Crash {
    let skip = crash.breadcrumbs.len().saturating_sub(MAX_BREADCRUMBS);
    Crash {
        kind: crash.kind,
        fatal: crash.fatal,
        message: crash.message.map(|text| clean_text(&text, MAX_MESSAGE_LEN)),
        location: crash.location.map(|text| clean(&text, 512)),
        thread: crash.thread.map(|text| clean(&text, MAX_FIELD_LEN)),
        backtrace: crash.backtrace.map(|text| clean_text(&text, MAX_BACKTRACE_LEN)),
        blocked_ms: crash.blocked_ms,
        exit_code: crash.exit_code,
        signal: crash.signal,
        context: crash.context.map(|text| clean(&text, 256)),
        breadcrumbs: crash
            .breadcrumbs
            .into_iter()
            .skip(skip)
            .map(|crumb| Breadcrumb {
                ts: crumb.ts,
                level: clean(&crumb.level, 16),
                target: clean(&crumb.target, MAX_FIELD_LEN),
                message: clean(&crumb.message, 512),
            })
            .collect(),
        perf: crash.perf.map(sanitize_perf),
    }
}

fn sanitize_device(device: Device) -> Device {
    let opt = |value: Option<String>, max: usize| value.map(|text| clean(&text, max)).filter(|text| !text.is_empty());
    Device {
        os_name: clean(&device.os_name, 64),
        os_version: opt(device.os_version, 64),
        arch: clean(&device.arch, 32),
        cpu_brand: opt(device.cpu_brand, MAX_FIELD_LEN),
        cpu_cores: device.cpu_cores.map(|cores| cores.min(4096)),
        ram_mb: device.ram_mb.map(|ram| ram.min(1 << 32)),
        gpu: device.gpu.map(|gpu| Gpu {
            name: clean(&gpu.name, MAX_FIELD_LEN),
            vendor: opt(gpu.vendor, 64),
            backend: opt(gpu.backend, 64),
            driver: opt(gpu.driver, 64),
        }),
        locale: opt(device.locale, 32),
    }
}

/// A crash fingerprint and the issue title that goes with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub hash: String,
    pub title: String,
}

/// Groups crashes into issues. The input is the kind plus, in order of preference: the first frames of the
/// backtrace that belong to the app (runtime and panic machinery skipped, addresses and hash suffixes
/// removed), else the location, else the message with digits removed.
pub fn fingerprint(kind: CrashKind, message: Option<&str>, location: Option<&str>, backtrace: Option<&str>) -> Fingerprint {
    let frames = app_frames(backtrace.unwrap_or(""));
    let mut input = String::from(kind.as_str());
    if !frames.is_empty() {
        for frame in frames.iter().take(FINGERPRINT_FRAMES) {
            input.push('\n');
            input.push_str(frame);
        }
    } else if let Some(location) = location.map(str::trim).filter(|text| !text.is_empty()) {
        input.push_str("\nloc:");
        input.push_str(location);
    } else {
        input.push_str("\nmsg:");
        input.extend(message.unwrap_or("").chars().filter(|c| !c.is_ascii_digit()));
    }
    let title = message
        .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
        .map(|line| clean(line, 200))
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| kind.as_str().to_string());
    Fingerprint { hash: fnv128_hex(input.as_bytes()), title }
}

/// Symbol names from a backtrace, runtime frames removed and noise normalised.
pub fn app_frames(backtrace: &str) -> Vec<String> {
    backtrace.lines().filter_map(normalize_frame).filter(|frame| !is_runtime_frame(frame)).collect()
}

fn normalize_frame(line: &str) -> Option<String> {
    let mut line = line.trim();
    if line.is_empty() || line.starts_with("at ") {
        return None;
    }
    // Strip a leading frame index such as `12:`.
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && line[digits..].starts_with(':') {
        line = line[digits + 1..].trim_start();
    }
    let symbol: Vec<&str> = line
        .split_whitespace()
        .filter(|token| !is_address(token) && *token != "-" && *token != "+" && !token.bytes().all(|b| b.is_ascii_digit()))
        .collect();
    if symbol.is_empty() {
        return None;
    }
    let joined = symbol.join(" ");
    Some(strip_hash_suffix(&joined).to_string())
}

fn is_address(token: &str) -> bool {
    token.strip_prefix("0x").is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Removes a trailing `::h` followed by 16 hex digits, the legacy symbol hash.
fn strip_hash_suffix(symbol: &str) -> &str {
    let bytes = symbol.as_bytes();
    if bytes.len() >= 19 {
        let start = bytes.len() - 19;
        if symbol.is_char_boundary(start) && &symbol[start..start + 3] == "::h" && bytes[start + 3..].iter().all(u8::is_ascii_hexdigit) {
            return &symbol[..start];
        }
    }
    symbol
}

fn is_runtime_frame(frame: &str) -> bool {
    const PREFIXES: [&str; 14] = [
        "std::",
        "core::",
        "alloc::",
        "<std::",
        "<core::",
        "<alloc::",
        "backtrace::",
        "<backtrace::",
        "rust_begin_unwind",
        "rust_panic",
        "rust_eh_personality",
        "__rust",
        "tracer_sdk::",
        "<tracer_sdk::",
    ];
    PREFIXES.iter().any(|prefix| frame.starts_with(prefix))
}

/// FNV-1a over 128 bits, as 32 hex digits. Stable across releases and platforms; not a security hash.
fn fnv128_hex(bytes: &[u8]) -> String {
    const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
    const PRIME: u128 = 0x0000000001000000000000000000013b;
    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:032x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracer_proto::{AppInfo, Item, ProcessKind};

    const NOW: i64 = 1_791_900_100_000;

    fn envelope(items: Vec<Item>) -> Envelope {
        Envelope {
            schema: 1,
            app: AppInfo { name: "demo".into(), version: "1.0.0".into(), build: None, process: ProcessKind::Gui },
            install_id: "install1".into(),
            session_id: "session1".into(),
            device: None,
            sent_at: NOW as u64,
            items,
        }
    }

    fn event_item(id: &str, name: &str) -> Item {
        Item { id: id.into(), ts: NOW as u64 - 1000, body: ItemBody::Event(Event { name: name.into(), category: "file".into(), ..Event::default() }) }
    }

    #[test]
    fn same_stack_with_different_addresses_and_hashes_fingerprints_alike() {
        let a = "0: 0x1000 - demo::render::draw::h0123456789abcdef\n1: 0x2000 - demo::main::h1111111111111111\n2: std::rt::lang_start";
        let b = "   0: 0x9fff - demo::render::draw::hfedcba9876543210\n   1: 0x8abc - demo::main::h2222222222222222\n             at src/main.rs:10\n   2: std::rt::lang_start";
        let one = fingerprint(CrashKind::Panic, Some("index 3 out of bounds"), Some("src/a.rs:1:1"), Some(a));
        let two = fingerprint(CrashKind::Panic, Some("index 9 out of bounds"), Some("src/a.rs:2:2"), Some(b));
        assert_eq!(one.hash, two.hash);
    }

    #[test]
    fn different_stacks_or_kinds_fingerprint_differently() {
        let one = fingerprint(CrashKind::Panic, None, None, Some("0: demo::a\n1: demo::main"));
        let two = fingerprint(CrashKind::Panic, None, None, Some("0: demo::b\n1: demo::main"));
        let three = fingerprint(CrashKind::Hang, None, None, Some("0: demo::a\n1: demo::main"));
        assert_ne!(one.hash, two.hash);
        assert_ne!(one.hash, three.hash);
    }

    #[test]
    fn runtime_and_panic_frames_are_skipped() {
        let trace =
            "0: std::panicking::begin_panic\n1: core::panicking::panic_fmt\n2: rust_begin_unwind\n3: tracer_sdk::signals::crash::hook\n4: demo::load";
        assert_eq!(app_frames(trace), vec!["demo::load".to_string()]);
    }

    #[test]
    fn fingerprint_falls_back_to_location_then_message() {
        let by_location = |loc| fingerprint(CrashKind::Panic, Some("x"), Some(loc), None).hash;
        assert_eq!(by_location("src/a.rs:1:1"), by_location("src/a.rs:1:1"));
        assert_ne!(by_location("src/a.rs:1:1"), by_location("src/b.rs:1:1"));
        let by_message = |msg| fingerprint(CrashKind::Handled, Some(msg), None, None).hash;
        assert_eq!(by_message("failed after 12 tries"), by_message("failed after 97 tries"));
        assert_ne!(by_message("failed after 12 tries"), by_message("other"));
    }

    #[test]
    fn title_is_first_message_line_or_kind() {
        assert_eq!(fingerprint(CrashKind::Panic, Some("boom\nmore"), None, None).title, "boom");
        assert_eq!(fingerprint(CrashKind::Hang, None, None, None).title, "hang");
        assert_eq!(fingerprint(CrashKind::Hang, Some("  \n"), None, None).title, "hang");
    }

    #[test]
    fn drift_is_corrected_only_beyond_five_minutes() {
        // Client clock 1 hour behind: shifted forward by an hour.
        assert_eq!(corrected_ts(1_000_000, 1_000_000, 1_000_000 + 3_600_000), 1_000_000 + 3_600_000);
        // Two minutes off: left alone.
        assert_eq!(corrected_ts(1_000_000, 1_000_000, 1_000_000 + 120_000), 1_000_000);
        // Client clock ahead by an hour: shifted back.
        assert_eq!(corrected_ts(10_000_000, 10_000_000, 10_000_000 - 3_600_000), 10_000_000 - 3_600_000);
    }

    #[test]
    fn future_times_are_clamped_to_one_day_and_never_negative() {
        assert_eq!(corrected_ts(u64::MAX, NOW as u64, NOW), NOW + DAY_MS);
        assert_eq!(corrected_ts(0, 10_000_000_000_000, NOW), 0);
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        assert_eq!(truncate("héllo", 2), "h");
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(clean("a\u{0}b\nc  ", 10), "abc");
    }

    #[test]
    fn invalid_names_and_unknown_items_are_rejected_not_fatal() {
        let items = vec![
            event_item("e1", "file.open"),
            event_item("e2", "Bad Name"),
            Item { id: "u1".into(), ts: 1, body: ItemBody::Unknown },
            Item { id: String::new(), ts: 1, body: ItemBody::Unknown },
        ];
        let prepared = prepare(envelope(items), NOW).unwrap();
        assert_eq!(prepared.items.len(), 1);
        assert_eq!(prepared.rejected, 3);
    }

    #[test]
    fn hostile_headers_are_refused() {
        let mut bad = envelope(vec![]);
        bad.install_id = "a b; drop".into();
        assert_eq!(prepare(bad, NOW).unwrap_err(), PrepareError::BadInstallId);
        let mut bad = envelope(vec![]);
        bad.session_id = "x".repeat(65);
        assert_eq!(prepare(bad, NOW).unwrap_err(), PrepareError::BadSessionId);
        let mut bad = envelope(vec![]);
        bad.app.version = "\n".into();
        assert_eq!(prepare(bad, NOW).unwrap_err(), PrepareError::BadVersion);
    }

    #[test]
    fn oversized_strings_are_truncated_and_props_limited() {
        let mut props = BTreeMap::new();
        for index in 0..40 {
            props.insert(format!("key{index:02}"), PropValue::Str("v".repeat(500)));
        }
        props.insert("Bad Key".into(), PropValue::Bool(true));
        let item =
            Item { id: "e".into(), ts: NOW as u64, body: ItemBody::Event(Event { name: "a".into(), category: "b".into(), dynamic: false, props }) };
        let crash = Item {
            id: "c".into(),
            ts: NOW as u64,
            body: ItemBody::Crash(Crash { message: Some("m".repeat(10_000)), backtrace: Some("b".repeat(100_000)), ..Crash::default() }),
        };
        let prepared = prepare(envelope(vec![item, crash]), NOW).unwrap();
        let PreparedBody::Event(event) = &prepared.items[0].body else { panic!("event") };
        assert_eq!(event.props.len(), MAX_PROPS);
        assert!(event.props.values().all(|value| matches!(value, PropValue::Str(text) if text.len() == MAX_PROP_STR_LEN)));
        let PreparedBody::Crash(crash) = &prepared.items[1].body else { panic!("crash") };
        assert_eq!(crash.crash.message.as_ref().map(String::len), Some(MAX_MESSAGE_LEN));
        assert_eq!(crash.crash.backtrace.as_ref().map(String::len), Some(MAX_BACKTRACE_LEN));
    }

    #[test]
    fn perf_buckets_are_normalised_and_numbers_clamped() {
        let perf = Perf {
            frames: Some(FrameStats { count: u64::MAX, buckets: vec![u64::MAX; 40], ..FrameStats::default() }),
            cpu_pct: Some(f32::NAN),
            ..Perf::default()
        };
        let cleaned = sanitize_perf(perf);
        let frames = cleaned.frames.unwrap();
        assert_eq!(frames.buckets.len(), FRAME_BUCKETS);
        assert_eq!(frames.count, COUNTER_MAX);
        assert_eq!(cleaned.cpu_pct, None);
    }

    #[test]
    fn session_start_time_wins_over_other_items() {
        let start = Item { id: "s".into(), ts: NOW as u64 - 50_000, body: ItemBody::SessionStart(Default::default()) };
        let prepared = prepare(envelope(vec![event_item("e", "a"), start]), NOW).unwrap();
        assert_eq!(prepared.started_at, NOW - 50_000);
        assert_eq!(prepared.last_seen, NOW - 1000);
    }
}
