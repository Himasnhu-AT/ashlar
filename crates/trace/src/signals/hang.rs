//! Hang detection. A watchdog thread reports when the host's main loop has been stuck for longer than
//! the threshold. The host tells it about the loop in one of two ways:
//!
//! - **Turns** (preferred): `begin_turn()` when the loop starts handling something, `end_turn()` when it
//!   is done. A turn open for longer than the threshold is a hang. Between turns the loop is waiting for
//!   work, which is never a hang, so this suits loops that only run on input.
//! - **Heartbeats**: `heartbeat()` once per iteration of a loop that runs continuously. A last beat older
//!   than the threshold is a hang. Once a host has begun a turn, heartbeats no longer count.
//!
//! Watching starts with the first turn or beat, so hosts that call neither get no reports.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ashlar_trace_proto::{CrashKind, ItemBody, Signal};

/// Longest and shortest time the watchdog sleeps between checks.
const MAX_POLL: Duration = Duration::from_millis(500);
const MIN_POLL: Duration = Duration::from_millis(1);

#[derive(Default)]
pub(crate) struct State {
    /// Milliseconds since the epoch of the last beat, plus one. Zero means "never beaten".
    last_beat: AtomicU64,
    /// Zero until the host first begins a turn; then [`NO_TURN`] between turns, and while a turn is open
    /// the stamp of its start, plus one.
    turn: AtomicU64,
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

    /// Opens a turn. One atomic update. A turn that is already open keeps its start, so a nested or
    /// repeated call cannot hide how long the loop has really been busy.
    pub(super) fn begin_turn(&self, epoch: Instant) {
        let start = Self::stamp(epoch).saturating_add(1);
        let _ = self.turn.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |turn| (turn <= NO_TURN).then_some(start));
    }

    /// Closes the open turn. One atomic update. Without an open turn it does nothing; in particular it
    /// does not switch a host that only sends heartbeats over to turns.
    pub(super) fn end_turn(&self) {
        let _ = self.turn.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |turn| (turn > NO_TURN).then_some(NO_TURN));
    }

    pub(super) fn pause(&self) {
        self.paused.fetch_add(1, Ordering::AcqRel);
    }

    /// Ends one pause. When the last one ends, watching starts afresh so the paused time is not a hang.
    pub(super) fn resume(&self, epoch: Instant) {
        // The fresh start is stored before the pause count drops. The other way round, a check that ran
        // in between would see no pause and a start from before it, and report the whole pause as a hang.
        if self.paused.load(Ordering::Acquire) == 1 {
            self.start_afresh(epoch);
        }
        let _ = self.paused.fetch_update(Ordering::AcqRel, Ordering::Acquire, |depth| depth.checked_sub(1));
    }

    /// Moves the start of whatever is being watched to now: the last beat if there has been one, the open
    /// turn if there is one. Starts nothing that was not being watched.
    fn start_afresh(&self, epoch: Instant) {
        let now = Self::stamp(epoch);
        let _ = self.last_beat.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |beat| (beat != 0).then_some(now));
        let _ = self.turn.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |turn| (turn > NO_TURN).then_some(now.saturating_add(1)));
    }
}

/// Value of [`State::turn`] between turns.
const NO_TURN: u64 = 1;

/// True when the time since the watchdog last looked is far more than its poll interval: it was not
/// running, because the machine was asleep or the process was stopped. The main loop was not running
/// either, so that time says nothing about a hang.
fn was_suspended(gap: Duration, poll: Duration) -> bool {
    gap > poll.saturating_mul(10).saturating_add(Duration::from_secs(2))
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
    let mut looked = Instant::now();
    loop {
        {
            let Some(shared) = weak.upgrade() else { return };
            if shared.hang.stop.load(Ordering::Acquire) {
                return;
            }
            let now = Instant::now();
            if was_suspended(now.saturating_duration_since(looked), poll) {
                shared.hang.start_afresh(shared.epoch);
            } else {
                check(&shared, State::stamp(shared.epoch));
            }
            looked = now;
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

/// Reports a hang if, as of `now` (a stamp from [`State::stamp`]), the open turn is older than the
/// threshold or, for a host that does not use turns, the last beat is. Returns true when a report was made.
pub(super) fn check(shared: &super::Shared, now: u64) -> bool {
    let state = &shared.hang;
    if state.paused.load(Ordering::Acquire) > 0 {
        return false;
    }
    // `beat` names what is being watched, so that one hang is reported once; `since` is when it began.
    let (beat, since) = match state.turn.load(Ordering::Relaxed) {
        0 => {
            let beat = state.last_beat.load(Ordering::Relaxed);
            (beat, beat)
        }
        NO_TURN => return false,
        turn => (turn, turn - 1),
    };
    if beat == 0 || state.reported_for.load(Ordering::Relaxed) == beat {
        return false;
    }
    let blocked = now.saturating_sub(since);
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

    use ashlar_trace_proto::{CrashKind, ItemBody, Signal};

    use super::*;
    use crate::signals::testutil::{Fixture, wait_until};

    fn hang_items(fixture: &Fixture) -> Vec<ashlar_trace_proto::Crash> {
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
    fn resuming_starts_afresh_before_the_pause_is_lifted() {
        let fixture = manual();
        let shared = &fixture.signals.shared;
        shared.hang.last_beat.store(1, Ordering::Relaxed);
        fixture.signals.pause_hang_detection();
        // Exactly what `resume` does first. Even if the watchdog looks at this moment, with the pause
        // still counted or just lifted, the old beat is no longer there to be measured from.
        shared.hang.start_afresh(shared.epoch);
        let beat = shared.hang.last_beat.load(Ordering::Relaxed);
        assert!(beat > 1 || State::stamp(shared.epoch) == 1);
        shared.hang.paused.store(0, Ordering::Relaxed);
        assert!(!check(shared, beat + 50));
        // And `resume` itself leaves the same state behind.
        shared.hang.last_beat.store(1, Ordering::Relaxed);
        fixture.signals.pause_hang_detection();
        std::thread::sleep(Duration::from_millis(5));
        fixture.signals.resume_hang_detection();
        assert!(shared.hang.last_beat.load(Ordering::Relaxed) > 1);
        assert_eq!(shared.hang.paused.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn an_open_turn_longer_than_the_threshold_is_a_hang_and_idle_time_never_is() {
        let fixture = manual();
        let shared = &fixture.signals.shared;
        // A host that repaints only on input: long waits between turns, short turns.
        fixture.signals.begin_turn();
        let start = shared.hang.turn.load(Ordering::Relaxed) - 1;
        assert!(!check(shared, start + 50), "below threshold");
        fixture.signals.end_turn();
        assert!(!check(shared, start + 3_600_000), "waiting for input is not a hang");
        assert!(!check(shared, u64::MAX));

        // A turn that gets stuck is reported once, with how long it has been stuck.
        shared.hang.turn.store(10_001, Ordering::Relaxed);
        assert!(!check(shared, 10_099));
        assert!(check(shared, 10_250));
        assert!(!check(shared, 20_000), "same hang is not reported again");
        fixture.signals.end_turn();
        assert!(!check(shared, 30_000));
        // The next stuck turn is a new hang.
        shared.hang.turn.store(40_001, Ordering::Relaxed);
        assert!(check(shared, 40_100));
        let items = hang_items(&fixture);
        assert_eq!(items.iter().map(|crash| crash.blocked_ms).collect::<Vec<_>>(), vec![Some(250), Some(100)]);
    }

    #[test]
    fn once_turns_are_used_heartbeats_no_longer_count() {
        let fixture = manual();
        let shared = &fixture.signals.shared;
        fixture.signals.heartbeat();
        fixture.signals.begin_turn();
        fixture.signals.end_turn();
        // An old beat and no open turn: under the heartbeat rule this would be a hang.
        shared.hang.last_beat.store(1, Ordering::Relaxed);
        assert!(!check(shared, 1_000_000));
        fixture.signals.heartbeat();
        assert!(!check(shared, u64::MAX));
        assert!(hang_items(&fixture).is_empty());
    }

    #[test]
    fn nested_and_unbalanced_turn_calls_are_harmless() {
        let fixture = manual();
        let shared = &fixture.signals.shared;
        // Ending a turn that was never begun does nothing, and does not switch the heartbeat rule off.
        fixture.signals.end_turn();
        assert_eq!(shared.hang.turn.load(Ordering::Relaxed), 0);
        shared.hang.last_beat.store(1_000, Ordering::Relaxed);
        assert!(check(shared, 1_100), "the heartbeat rule still applies");

        // A second begin while a turn is open keeps the original start.
        shared.hang.turn.store(5_001, Ordering::Relaxed);
        fixture.signals.begin_turn();
        assert_eq!(shared.hang.turn.load(Ordering::Relaxed), 5_001);
        assert!(check(shared, 5_200));
        // One end closes it, however many begins there were; further ends do nothing.
        fixture.signals.end_turn();
        fixture.signals.end_turn();
        assert_eq!(shared.hang.turn.load(Ordering::Relaxed), NO_TURN);
        assert!(!check(shared, u64::MAX));
    }

    #[test]
    fn pausing_covers_an_open_turn_and_resuming_restarts_it() {
        let fixture = manual();
        let shared = &fixture.signals.shared;
        shared.hang.turn.store(2, Ordering::Relaxed);
        fixture.signals.pause_hang_detection();
        assert!(!check(shared, u64::MAX));
        fixture.signals.resume_hang_detection();
        let start = shared.hang.turn.load(Ordering::Relaxed) - 1;
        assert_eq!(start, shared.hang.last_beat.load(Ordering::Relaxed).max(start), "the turn restarts at the resume");
        assert!(!check(shared, start + 50));
        assert!(check(shared, start + 100));
        // Resuming between turns opens none.
        fixture.signals.end_turn();
        fixture.signals.pause_hang_detection();
        fixture.signals.resume_hang_detection();
        assert_eq!(shared.hang.turn.load(Ordering::Relaxed), NO_TURN);
    }

    #[test]
    fn a_gap_far_beyond_the_poll_interval_means_the_watchdog_was_suspended() {
        let poll = Duration::from_millis(500);
        assert!(!was_suspended(poll, poll));
        assert!(!was_suspended(Duration::from_secs(3), poll), "a busy machine can make it late");
        assert!(was_suspended(Duration::from_secs(8), poll));
        assert!(was_suspended(Duration::from_secs(3 * 3600), poll));
        assert!(!was_suspended(Duration::MAX, Duration::MAX));
        // Starting afresh moves an open turn and the last beat, and starts nothing that was not watched.
        let fixture = manual();
        let shared = &fixture.signals.shared;
        shared.hang.start_afresh(shared.epoch);
        assert_eq!((shared.hang.last_beat.load(Ordering::Relaxed), shared.hang.turn.load(Ordering::Relaxed)), (0, 0));
        shared.hang.turn.store(2, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(5));
        shared.hang.start_afresh(shared.epoch);
        assert!(shared.hang.turn.load(Ordering::Relaxed) > 2);
    }

    #[test]
    fn watchdog_thread_reports_a_stuck_turn_but_not_the_wait_between_turns() {
        let fixture = Fixture::new(|config| {
            config.signals = vec![Signal::Hang];
            config.hang_threshold = Duration::from_millis(200);
        });
        fixture.signals.begin_turn();
        fixture.signals.end_turn();
        std::thread::sleep(Duration::from_millis(700));
        assert!(hang_items(&fixture).is_empty(), "idle between turns was reported as a hang");
        fixture.signals.begin_turn();
        assert!(wait_until(|| hang_items(&fixture).len() == 1), "the stuck turn was not reported");
        fixture.signals.end_turn();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(hang_items(&fixture).len(), 1);
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
