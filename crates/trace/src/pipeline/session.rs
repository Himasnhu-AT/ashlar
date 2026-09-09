//! Session time accounting, the running-session marker, and the record of what was selected and
//! acknowledged.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracer_proto::{Catalog, SessionTotals, UnixMs};

use super::spool::Header;

pub(super) const SESSION_FILE: &str = "session.json";
pub(super) const SELECTION_FILE: &str = "selection.json";

/// Splits the session's running time into focused, active and idle, on a monotonic clock.
///
/// The worker samples it on every tick. The time since the previous sample is credited to the state seen
/// at the sample, so the result does not depend on the ticks being evenly spaced.
#[derive(Debug)]
pub(super) struct SessionClock {
    last: Instant,
    total: Duration,
    focused: Duration,
    active: Duration,
    heartbeats: u32,
    ended: bool,
    /// Most that one sample may add. A gap longer than this is not time the session was running: the
    /// machine was asleep, and on some systems the monotonic clock goes on counting through that.
    max_step: Duration,
}

impl SessionClock {
    pub(super) fn new(now: Instant, max_step: Duration) -> Self {
        Self { last: now, total: Duration::ZERO, focused: Duration::ZERO, active: Duration::ZERO, heartbeats: 0, ended: false, max_step }
    }

    /// Credits the time since the last sample. `active` only counts while `focused`.
    pub(super) fn sample(&mut self, now: Instant, focused: bool, active: bool) {
        let elapsed = now.saturating_duration_since(self.last).min(self.max_step);
        self.last = self.last.max(now);
        self.total += elapsed;
        if focused {
            self.focused += elapsed;
            if active {
                self.active += elapsed;
            }
        }
    }

    /// Starts the next heartbeat and returns its totals. `None` once the session has ended, so that a
    /// heartbeat can never follow the session end.
    pub(super) fn next_heartbeat(&mut self) -> Option<SessionTotals> {
        if self.ended {
            return None;
        }
        self.heartbeats = self.heartbeats.saturating_add(1);
        Some(self.totals())
    }

    /// Marks the session as over and returns its final totals.
    pub(super) fn end(&mut self) -> SessionTotals {
        self.ended = true;
        self.totals()
    }

    /// Running totals in whole seconds. Idle is everything that was not active.
    pub(super) fn totals(&self) -> SessionTotals {
        SessionTotals {
            seq: self.heartbeats,
            duration_s: self.total.as_secs(),
            focused_s: self.focused.as_secs(),
            active_s: self.active.as_secs(),
            idle_s: self.total.saturating_sub(self.active).as_secs(),
        }
    }
}

/// Content of `session.json`. Present while a session runs; one found at startup means the previous run
/// did not shut down cleanly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Marker {
    pub header: Header,
    pub started: UnixMs,
    /// When the marker was last rewritten: the last moment the session is known to have been alive.
    #[serde(default)]
    pub updated: UnixMs,
    #[serde(default)]
    pub totals: SessionTotals,
    #[serde(default)]
    pub errors: u32,
    #[serde(default)]
    pub panicked: bool,
}

/// Content of `selection.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Selection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_hash: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub signals: Vec<String>,
    /// Catalog hashes the server has acknowledged, by app version.
    #[serde(default)]
    pub acknowledged: BTreeMap<String, Vec<String>>,
}

impl Selection {
    pub(super) fn is_acknowledged(&self, version: &str, hash: &str) -> bool {
        self.acknowledged.get(version).is_some_and(|hashes| hashes.iter().any(|known| known == hash))
    }

    /// Records that the server holds this catalog. Returns true when that is news.
    pub(super) fn acknowledge(&mut self, version: &str, hash: &str) -> bool {
        if self.is_acknowledged(version, hash) {
            return false;
        }
        self.acknowledged.entry(version.to_owned()).or_default().push(hash.to_owned());
        true
    }

    /// Forgets the acknowledgements for a version, so the next session resends the catalog. Returns true
    /// when there was something to forget.
    pub(super) fn clear_acknowledged(&mut self, version: &str) -> bool {
        self.acknowledged.remove(version).is_some()
    }
}

/// A hash of the catalog that does not depend on declaration order: FNV-1a over the sorted events, as 16
/// hex characters. `None` for an empty catalog.
pub(super) fn catalog_hash(catalog: &Catalog) -> Option<String> {
    if catalog.events.is_empty() {
        return None;
    }
    let mut events: Vec<_> = catalog.events.iter().collect();
    events.sort();
    events.dedup();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    for event in events {
        feed(event.name.as_bytes());
        feed(&[0]);
        feed(event.category.as_bytes());
        feed(b"\n");
    }
    Some(format!("{hash:016x}"))
}

#[cfg(test)]
mod tests {
    use tracer_proto::CatalogEvent;

    use super::*;

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn time_is_split_by_the_state_at_each_sample() {
        let start = Instant::now();
        let mut clock = SessionClock::new(start, Duration::from_secs(60));
        clock.sample(start + secs(10), true, true);
        clock.sample(start + secs(25), true, false);
        clock.sample(start + secs(30), false, true);
        let totals = clock.totals();
        assert_eq!((totals.duration_s, totals.focused_s, totals.active_s, totals.idle_s), (30, 25, 10, 20));
        assert_eq!(totals.seq, 0);
        assert_eq!(clock.next_heartbeat().map(|totals| totals.seq), Some(1));
        assert_eq!(clock.next_heartbeat().map(|totals| totals.seq), Some(2));
        assert_eq!(clock.end().seq, 2);
        assert_eq!(clock.next_heartbeat(), None);
    }

    #[test]
    fn sub_second_samples_add_up_and_time_never_runs_backwards() {
        let start = Instant::now();
        let mut clock = SessionClock::new(start, Duration::from_secs(60));
        for step in 1..=2500u64 {
            clock.sample(start + Duration::from_millis(step), true, step % 2 == 0);
        }
        clock.sample(start, true, true);
        let totals = clock.totals();
        assert_eq!((totals.duration_s, totals.focused_s, totals.active_s, totals.idle_s), (2, 2, 1, 1));
    }

    #[test]
    fn a_gap_longer_than_one_step_is_not_credited_in_full() {
        let start = Instant::now();
        let mut clock = SessionClock::new(start, Duration::from_secs(60));
        clock.sample(start + secs(30), true, true);
        // The machine slept for three hours between two ticks.
        clock.sample(start + secs(30 + 3 * 3600), true, true);
        clock.sample(start + secs(40 + 3 * 3600), true, false);
        let totals = clock.totals();
        assert_eq!((totals.duration_s, totals.focused_s, totals.active_s, totals.idle_s), (100, 100, 90, 10));
    }

    #[test]
    fn catalog_hash_ignores_order_and_sees_changes() {
        let event = |name: &str, category: &str| CatalogEvent { name: name.to_owned(), category: category.to_owned() };
        let forward = Catalog { events: vec![event("file.open", "file"), event("edit.undo", "edit")] };
        let backward = Catalog { events: vec![event("edit.undo", "edit"), event("file.open", "file")] };
        let moved = Catalog { events: vec![event("file.open", "edit"), event("edit.undo", "edit")] };
        let hash = catalog_hash(&forward).unwrap();
        assert_eq!(hash.len(), 16);
        assert_eq!(Some(&hash), catalog_hash(&backward).as_ref());
        assert_ne!(Some(&hash), catalog_hash(&moved).as_ref());
        assert_eq!(catalog_hash(&Catalog::default()), None);
        // Pinned: a later SDK version that hashed differently would make every install resend its catalog.
        assert_eq!(hash, "004a91c3e7d3accb");
    }

    #[test]
    fn acknowledgements_are_per_version() {
        let mut selection = Selection::default();
        assert!(!selection.is_acknowledged("1.0.0", "abc"));
        assert!(selection.acknowledge("1.0.0", "abc"));
        assert!(!selection.acknowledge("1.0.0", "abc"));
        assert!(selection.is_acknowledged("1.0.0", "abc"));
        assert!(!selection.is_acknowledged("1.1.0", "abc"));
        assert!(selection.clear_acknowledged("1.0.0"));
        assert!(!selection.clear_acknowledged("1.0.0"));
        assert!(!selection.is_acknowledged("1.0.0", "abc"));
    }
}
