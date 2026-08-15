//! Hang detection. The host calls `heartbeat()` from its main loop; a watchdog thread reports when the
//! last beat is older than the threshold. Watching starts at the first beat, so hosts that never call
//! `heartbeat()` get no reports.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracer_proto::{CrashKind, ItemBody, Signal};

/// Longest and shortest time the watchdog sleeps between checks.
const MAX_POLL: Duration = Duration::from_millis(500);
const MIN_POLL: Duration = Duration::from_millis(1);

#[derive(Default)]
pub(crate) struct State {
    /// Milliseconds since the epoch of the last beat, plus one. Zero means "never beaten".
    last_beat: AtomicU64,
    /// Nesting depth of `pause_hang_detection`.
    paused: AtomicU32,
    /// The `last_beat` value a report was already made for.
    reported_for: AtomicU64,
    stop: AtomicBool,
}

impl State {
    fn stamp(epoch: Instant) -> u64 {
        u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX - 1).saturating_add(1)
    }

    /// One atomic store.
    pub(super) fn beat(&self, epoch: Instant) {
        self.last_beat.store(Self::stamp(epoch), Ordering::Relaxed);
    }

    pub(super) fn pause(&self) {
        self.paused.fetch_add(1, Ordering::AcqRel);
    }

    /// Ends one pause. When the last one ends, counts as a beat so the paused time is not a hang.
    pub(super) fn resume(&self, epoch: Instant) {
        let result = self.paused.fetch_update(Ordering::AcqRel, Ordering::Acquire, |depth| depth.checked_sub(1));
        if result == Ok(1) && self.last_beat.load(Ordering::Relaxed) != 0 {
            self.beat(epoch);
        }
    }
}

/// Starts the `tracer-watchdog` thread. It holds the shared state weakly and ends when that is gone.
pub(super) fn spawn(shared: &Arc<super::Shared>) -> Option<JoinHandle<()>> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = shared;
        None
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let weak = Arc::downgrade(shared);
        let poll = (shared.settings.hang_threshold / 4).clamp(MIN_POLL, MAX_POLL);
        std::thread::Builder::new().name("tracer-watchdog".to_owned()).spawn(move || run(&weak, poll)).ok()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run(weak: &std::sync::Weak<super::Shared>, poll: Duration) {
    loop {
        {
            let Some(shared) = weak.upgrade() else { return };
            if shared.hang.stop.load(Ordering::Acquire) {
                return;
            }
            check(&shared, State::stamp(shared.epoch));
        }
        std::thread::park_timeout(poll);
    }
}

/// Tells the watchdog to stop and waits for it. Quick, because the thread is woken.
pub(super) fn stop(shared: &super::Shared, handle: JoinHandle<()>) {
    shared.hang.stop.store(true, Ordering::Release);
    handle.thread().unpark();
    if handle.thread().id() != std::thread::current().id() {
        // A panic inside the watchdog would already have been reported; there is nothing to do with it here.
        let _ = handle.join();
    }
}

/// Reports a hang if the last beat is older than the threshold as of `now` (a stamp from [`State::stamp`]).
/// Returns true when a report was made.
pub(super) fn check(shared: &super::Shared, now: u64) -> bool {
    let state = &shared.hang;
    let beat = state.last_beat.load(Ordering::Relaxed);
    if beat == 0 || state.paused.load(Ordering::Acquire) > 0 || state.reported_for.load(Ordering::Relaxed) == beat {
        return false;
    }
    let blocked = now.saturating_sub(beat);
    if u128::from(blocked) < shared.settings.hang_threshold.as_millis() {
        return false;
    }
    if !shared.allowed(Signal::Hang) {
        return false;
    }
    state.reported_for.store(beat, Ordering::Relaxed);
    let mut crash = shared.crash_base(CrashKind::Hang, true);
    crash.blocked_ms = Some(u32::try_from(blocked).unwrap_or(u32::MAX));
    shared.remember_crash(&crash, true);
    shared.sink.submit(Signal::Hang, ItemBody::Crash(crash));
    true
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tracer_proto::{CrashKind, ItemBody, Signal};

    use super::*;
    use crate::signals::testutil::{Fixture, wait_until};

    fn hang_items(fixture: &Fixture) -> Vec<tracer_proto::Crash> {
        fixture
            .sink
            .bodies()
            .into_iter()
            .filter_map(|body| match body {
                ItemBody::Crash(crash) if crash.kind == CrashKind::Hang => Some(crash),
                _ => None,
            })
            .collect()
    }

    /// A fixture whose watchdog thread has been stopped, so `check` is driven by hand.
    fn manual() -> Fixture {
        let fixture = Fixture::new(|config| {
            config.signals = vec![Signal::Hang];
            config.hang_threshold = Duration::from_millis(100);
        });
        fixture.signals.stop_watchdog();
        fixture
    }

    #[test]
    fn nothing_before_the_first_heartbeat() {
        let fixture = manual();
        assert!(!check(&fixture.signals.shared, 10_000));
        assert!(hang_items(&fixture).is_empty());
    }

    #[test]
    fn reports_once_then_again_only_after_recovery() {
        let fixture = manual();
        let shared = &fixture.signals.shared;
        shared.hang.last_beat.store(1_000, Ordering::Relaxed);
        assert!(!check(shared, 1_050), "below threshold");
        assert!(check(shared, 1_100), "at threshold");
        assert!(!check(shared, 1_500), "same hang is not reported again");
        assert!(!check(shared, 9_000));
        // The loop recovers, beats, and then stalls again.
        shared.hang.last_beat.store(10_000, Ordering::Relaxed);
        assert!(!check(shared, 10_050));
        assert!(check(shared, 10_300));
        let items = hang_items(&fixture);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].blocked_ms, Some(100));
        assert_eq!(items[1].blocked_ms, Some(300));
        assert!(!items[0].fatal);
    }

    #[test]
    fn paused_means_no_report_and_pauses_nest() {
        let fixture = manual();
        let shared = &fixture.signals.shared;
        fixture.signals.heartbeat();
        fixture.signals.pause_hang_detection();
        fixture.signals.pause_hang_detection();
        assert!(!check(shared, u64::MAX));
        fixture.signals.resume_hang_detection();
        assert!(!check(shared, u64::MAX), "one pause is still open");
        fixture.signals.resume_hang_detection();
        // Resuming counts as a beat, so the time spent paused is not a hang.
        let beat = shared.hang.last_beat.load(Ordering::Relaxed);
        assert!(!check(shared, beat + 50));
        assert!(check(shared, beat + 100));
    }

    #[test]
    fn extra_resume_does_not_underflow_or_start_watching() {
        let fixture = manual();
        let shared = &fixture.signals.shared;
        fixture.signals.resume_hang_detection();
        assert_eq!(shared.hang.paused.load(Ordering::Relaxed), 0);
        assert_eq!(shared.hang.last_beat.load(Ordering::Relaxed), 0, "resume alone must not start watching");
        fixture.signals.pause_hang_detection();
        assert!(!check(shared, u64::MAX));
    }

    #[test]
    fn denied_hang_signal_reports_nothing() {
        let fixture = manual();
        fixture.deny(Signal::Hang);
        fixture.signals.shared.hang.last_beat.store(1, Ordering::Relaxed);
        assert!(!check(&fixture.signals.shared, 100_000));
        assert!(hang_items(&fixture).is_empty());
    }

    #[test]
    fn watchdog_thread_detects_a_stalled_loop_and_stops_on_shutdown() {
        let fixture = Fixture::new(|config| {
            config.signals = vec![Signal::Hang];
            config.hang_threshold = Duration::from_millis(200);
        });
        // Not before the first heartbeat.
        std::thread::sleep(Duration::from_millis(500));
        assert!(hang_items(&fixture).is_empty());

        fixture.signals.heartbeat();
        assert!(wait_until(|| hang_items(&fixture).len() == 1), "stall was not reported");
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(hang_items(&fixture).len(), 1, "one report per hang");

        fixture.signals.heartbeat();
        assert!(wait_until(|| hang_items(&fixture).len() == 2), "second hang after recovery was not reported");

        fixture.signals.shutdown();
        assert!(fixture.signals.watchdog.lock().is_ok_and(|slot| slot.is_none()));
        fixture.signals.heartbeat();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(hang_items(&fixture).len(), 2, "no reports after shutdown");
    }

    #[test]
    fn no_thread_without_the_hang_signal() {
        let fixture = Fixture::new(|config| config.signals = vec![Signal::Perf]);
        assert!(fixture.signals.watchdog.lock().is_ok_and(|slot| slot.is_none()));
    }

    #[test]
    fn thread_is_named_and_ends_when_signals_are_dropped() {
        let fixture = Fixture::new(|config| {
            config.signals = vec![Signal::Hang];
            config.hang_threshold = Duration::from_millis(200);
        });
        let handle_name = fixture.signals.watchdog.lock().ok().and_then(|slot| slot.as_ref().and_then(|h| h.thread().name().map(str::to_owned)));
        assert_eq!(handle_name.as_deref(), Some("tracer-watchdog"));
        let weak = Arc::downgrade(&fixture.signals.shared);
        let Fixture { signals, .. } = fixture;
        drop(signals);
        assert!(wait_until(|| weak.upgrade().is_none()), "shared state leaked");
    }
}
