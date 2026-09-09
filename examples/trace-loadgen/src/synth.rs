//! Realistic envelope generation from a seeded PRNG.

use std::collections::BTreeMap;
use std::io::Write;

use tracer_proto::{
    AppInfo, Breadcrumb, Catalog, CatalogEvent, Crash, CrashKind, Device, Envelope, Event, FrameStats, Item, ItemBody, Perf, PropValue, SessionEnd,
    SessionStart, SessionStatus, SessionTotals,
};

/// xorshift64*: small, fast, reproducible.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }

    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
}

pub const EVENT_NAMES: usize = 30;

pub fn event_name(index: usize) -> String {
    format!("ui.action{index:02}")
}

#[derive(Clone)]
pub struct Profile {
    pub seed: u64,
    pub installs: u64,
    pub versions: u64,
    /// Crash probability per envelope, in 1/1000.
    pub crash_permille: u64,
}

fn catalog() -> Catalog {
    Catalog { events: (0..EVENT_NAMES).map(|i| CatalogEvent { name: event_name(i), category: "ui".into() }).collect() }
}

fn frames(rng: &mut Rng) -> FrameStats {
    let mut stats = FrameStats::empty();
    for _ in 0..rng.range(200, 900) {
        let us = if rng.below(100) < 2 { rng.range(40_000, 900_000) } else { rng.range(3_000, 20_000) };
        stats.record(us as u32);
    }
    stats
}

fn crash(rng: &mut Rng, ts: u64) -> Crash {
    let stack = rng.below(40);
    let backtrace = (0..30)
        .map(|n| format!("{n}: 0x{:x} - demo::module{}::function{}::h{:016x}", rng.next() >> 20, (stack + n) % 9, n, rng.next()))
        .collect::<Vec<_>>()
        .join("\n");
    let breadcrumbs = (0..20)
        .map(|n| Breadcrumb {
            ts: ts.saturating_sub(2000 - n * 100),
            level: "info".into(),
            target: "demo::net".into(),
            message: format!("step {n} finished"),
        })
        .collect();
    Crash {
        kind: CrashKind::Panic,
        fatal: false,
        message: Some(format!("index out of bounds: the len is {} but the index is {}", rng.below(10), rng.below(100))),
        location: Some(format!("src/module{}.rs:{}:9", stack % 9, 10 + stack)),
        thread: Some("main".into()),
        backtrace: Some(backtrace),
        breadcrumbs,
        ..Crash::default()
    }
}

/// One envelope for one fresh session. Returns it with the number of items it carries.
pub fn envelope(profile: &Profile, rng: &mut Rng, seq: u64, now_ms: u64) -> Envelope {
    let install = rng.below(profile.installs.max(1));
    let version = install % profile.versions.max(1);
    let id = |n: u64| format!("{:08x}{:012x}{:04x}", profile.seed as u32, seq, n);
    let mut items = Vec::with_capacity(48);
    let base = now_ms.saturating_sub(60_000);
    let mut clock = base;
    let mut tick = move |rng: &mut Rng| {
        clock += rng.range(5, 400);
        clock
    };
    let with_catalog = rng.below(50) == 0;
    items.push(Item {
        id: id(0),
        ts: base,
        body: ItemBody::SessionStart(SessionStart {
            catalog_hash: Some(format!("cat-{version}")),
            catalog: with_catalog.then(catalog),
            listening: vec!["ui".into()],
            signals: vec!["session".into(), "usage".into(), "crash".into(), "perf".into()],
        }),
    });
    let mut n = 1;
    for beat in 1..=rng.range(1, 3) {
        items.push(Item {
            id: id(n),
            ts: tick(rng),
            body: ItemBody::SessionHeartbeat(SessionTotals {
                seq: beat as u32,
                duration_s: beat * 30,
                focused_s: beat * 25,
                active_s: beat * 20,
                idle_s: beat * 10,
            }),
        });
        n += 1;
    }
    for _ in 0..rng.range(20, 50) {
        let mut props = BTreeMap::new();
        if rng.below(3) == 0 {
            props.insert("count".to_string(), PropValue::Int(rng.below(100) as i64));
            props.insert("mode".to_string(), PropValue::Str("fast".into()));
        }
        items.push(Item {
            id: id(n),
            ts: tick(rng),
            body: ItemBody::Event(Event { name: event_name(rng.below(EVENT_NAMES as u64) as usize), category: "ui".into(), dynamic: false, props }),
        });
        n += 1;
    }
    items.push(Item {
        id: id(n),
        ts: tick(rng),
        body: ItemBody::Perf(Perf {
            window_s: 60,
            frames: Some(frames(rng)),
            cpu_pct: Some(rng.below(100) as f32),
            rss_mb: Some(300 + rng.below(200) as u32),
            rss_peak_mb: Some(500),
            startup_ms: Some(900),
            ..Perf::default()
        }),
    });
    n += 1;
    if rng.below(1000) < profile.crash_permille {
        let at = tick(rng);
        items.push(Item { id: id(n), ts: at, body: ItemBody::Crash(crash(rng, at)) });
        n += 1;
    }
    let (status, errors) = if rng.below(20) == 0 { (SessionStatus::Crashed, 1) } else { (SessionStatus::Exited, 0) };
    items.push(Item {
        id: id(n),
        ts: tick(rng),
        body: ItemBody::SessionEnd(SessionEnd {
            status,
            totals: SessionTotals { seq: 4, duration_s: 100, focused_s: 90, active_s: 70, idle_s: 30 },
            errors,
        }),
    });

    Envelope {
        schema: 1,
        app: AppInfo { name: "demo".into(), version: format!("1.{version}.0"), build: None, process: tracer_proto::ProcessKind::Gui },
        install_id: format!("inst{install:08x}"),
        session_id: format!("sess{:08x}{seq:012x}", profile.seed as u32),
        device: Some(Device { os_name: ["macos", "windows", "linux"][(install % 3) as usize].into(), arch: "x86_64".into(), ..Device::default() }),
        sent_at: now_ms,
        items,
    }
}

/// The gzip body the SDK would send, with the item count.
pub fn body(profile: &Profile, rng: &mut Rng, seq: u64, now_ms: u64) -> (Vec<u8>, u64) {
    let envelope = envelope(profile, rng, seq, now_ms);
    let items = envelope.items.len() as u64;
    let json = serde_json::to_vec(&envelope).unwrap_or_default();
    let mut encoder = flate2::write::GzEncoder::new(Vec::with_capacity(json.len() / 4), flate2::Compression::fast());
    let _ = encoder.write_all(&json);
    (encoder.finish().unwrap_or_default(), items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_reproducible_and_unique() {
        let profile = Profile { seed: 7, installs: 100, versions: 3, crash_permille: 1000 };
        let a = envelope(&profile, &mut Rng::new(1), 5, 1_700_000_000_000);
        let b = envelope(&profile, &mut Rng::new(1), 5, 1_700_000_000_000);
        assert_eq!(a, b);
        let ids: std::collections::HashSet<_> = a.items.iter().map(|i| i.id.clone()).collect();
        assert_eq!(ids.len(), a.items.len());
        assert!(a.items.len() >= 24);
        assert!(a.items.iter().any(|i| matches!(i.body, ItemBody::Crash(_))));
    }
}
