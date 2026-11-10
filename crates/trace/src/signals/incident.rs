//! Incident rules over the frame and memory streams: a run of long frames, a stall, and steady memory
//! growth. Each rule fires at most once per performance window; repeats in the same window only raise
//! the entry's count and worst figure. The rules see durations and byte counts only, never the UI.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ashlar_trace_proto::{
    DISPLAY_BUDGET_US, Incident, IncidentRule, LONG_FRAMES_RUN, LONG_FRAMES_SPAN_MS, MEMORY_FLOORS, MEMORY_GROWTH_MIB, MEMORY_GROWTH_PCT, STALL_US,
};

const MINUTE: Duration = Duration::from_secs(60);
const MIB: u64 = 1024 * 1024;

pub(crate) struct Rules {
    epoch: Instant,
    /// End times and lengths of recent frames over budget, never more than [`LONG_FRAMES_RUN`].
    long: VecDeque<(Instant, u32)>,
    /// The open minute: when it started and the lowest memory seen in it.
    minute: Option<(Instant, u64)>,
    /// Floors of the last closed minutes, oldest first, never more than [`MEMORY_FLOORS`].
    floors: VecDeque<u64>,
    /// What fired in the open window, at most one entry per rule.
    fired: Vec<Incident>,
}

impl Rules {
    pub(crate) fn new(epoch: Instant) -> Self {
        Self { epoch, long: VecDeque::new(), minute: None, floors: VecDeque::new(), fired: Vec::new() }
    }

    /// A frame of `us` microseconds that ended at `now`. Frames within budget are not looked at.
    pub(crate) fn frame(&mut self, now: Instant, us: u32) {
        if us <= DISPLAY_BUDGET_US {
            return;
        }
        if us >= STALL_US {
            self.fire(IncidentRule::Stall, now, u64::from(us));
        }
        let span = Duration::from_millis(LONG_FRAMES_SPAN_MS);
        while self.long.front().is_some_and(|&(at, _)| now.saturating_duration_since(at) > span) {
            self.long.pop_front();
        }
        self.long.push_back((now, us));
        if self.long.len() >= LONG_FRAMES_RUN {
            let worst = self.long.iter().map(|&(_, us)| us).max().unwrap_or(us);
            // a new run needs three fresh frames, so a long burst fires once per three frames, not per frame
            self.long.clear();
            self.fire(IncidentRule::LongFrames, now, u64::from(worst));
        }
    }

    /// A resident-memory sample taken at `now`.
    pub(crate) fn memory(&mut self, now: Instant, rss_bytes: u64) {
        let (start, floor) = match self.minute {
            None => {
                self.minute = Some((now, rss_bytes));
                return;
            }
            Some(minute) => minute,
        };
        if now.saturating_duration_since(start) < MINUTE {
            self.minute = Some((start, floor.min(rss_bytes)));
            return;
        }
        // the open minute is over: its floor joins the list, and this sample opens the next minute
        self.minute = Some((now, rss_bytes));
        if self.floors.len() >= MEMORY_FLOORS {
            self.floors.pop_front();
        }
        self.floors.push_back(floor);
        if let Some(growth) = self.growth() {
            self.fire(IncidentRule::MemoryGrowth, now, growth / MIB);
        }
    }

    /// The growth in bytes when the last [`MEMORY_FLOORS`] floors rise each minute and the last is far
    /// enough above the first.
    fn growth(&self) -> Option<u64> {
        if self.floors.len() < MEMORY_FLOORS {
            return None;
        }
        let rising = self.floors.iter().zip(self.floors.iter().skip(1)).all(|(a, b)| b > a);
        let (first, last) = (*self.floors.front()?, *self.floors.back()?);
        let growth = last.checked_sub(first)?;
        let enough = growth >= MEMORY_GROWTH_MIB.saturating_mul(MIB) && growth.saturating_mul(100) >= first.saturating_mul(MEMORY_GROWTH_PCT);
        (rising && enough).then_some(growth)
    }

    fn fire(&mut self, rule: IncidentRule, now: Instant, value: u64) {
        if let Some(seen) = self.fired.iter_mut().find(|seen| seen.rule == rule) {
            seen.count = seen.count.saturating_add(1);
            seen.value = seen.value.max(value);
            return;
        }
        let at_ms = u64::try_from(now.saturating_duration_since(self.epoch).as_millis()).unwrap_or(u64::MAX);
        self.fired.push(Incident { rule, at_ms, count: 1, value });
    }

    /// What fired since the last call, and starts the next window with nothing fired. The streams'
    /// history (recent long frames, memory floors) carries over.
    pub(crate) fn take(&mut self) -> Vec<Incident> {
        std::mem::take(&mut self.fired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    struct Stream {
        rules: Rules,
        start: Instant,
    }

    impl Stream {
        fn new() -> Self {
            let start = Instant::now();
            Self { rules: Rules::new(start), start }
        }

        /// Frames as (end time in ms, length in ms).
        fn frames(&mut self, frames: &[(u64, u32)]) -> Vec<Incident> {
            for &(at, ms) in frames {
                self.rules.frame(self.start + MS * u32::try_from(at).unwrap_or(u32::MAX), ms * 1_000);
            }
            self.rules.take()
        }

        /// One memory sample every 10 s with the given MiB values per minute (6 samples each).
        fn minutes(&mut self, from_minute: u64, mib: &[&[u64]]) -> Vec<Incident> {
            for (m, samples) in mib.iter().enumerate() {
                for (s, value) in samples.iter().enumerate() {
                    let at = Duration::from_secs((from_minute + m as u64) * 60 + s as u64 * 10);
                    self.rules.memory(self.start + at, value * MIB);
                }
            }
            self.rules.take()
        }
    }

    fn rules(incidents: &[Incident]) -> Vec<(IncidentRule, u32, u64)> {
        incidents.iter().map(|i| (i.rule, i.count, i.value)).collect()
    }

    #[test]
    fn three_long_frames_within_a_second_fire_once() {
        let mut stream = Stream::new();
        let fired = stream.frames(&[(0, 9), (16, 4), (400, 12), (900, 20)]);
        assert_eq!(rules(&fired), vec![(IncidentRule::LongFrames, 1, 20_000)]);
        assert_eq!(fired[0].at_ms, 900);
    }

    #[test]
    fn long_frames_spread_over_more_than_a_second_do_not_fire() {
        let mut stream = Stream::new();
        assert!(stream.frames(&[(0, 9), (600, 9), (1_200, 9), (1_900, 9)]).is_empty());
        // frames at exactly the budget are within it
        assert!(stream.frames(&[(3_000, 0), (3_001, 0)]).is_empty());
        let budget = DISPLAY_BUDGET_US;
        for at in [4_000, 4_010, 4_020] {
            stream.rules.frame(stream.start + MS * at, budget);
        }
        assert!(stream.rules.take().is_empty());
    }

    #[test]
    fn a_burst_is_deduplicated_within_the_window_and_reported_again_in_the_next() {
        let mut stream = Stream::new();
        let burst: Vec<(u64, u32)> = (0..30).map(|i| (i * 17, 10)).collect();
        let fired = stream.frames(&burst);
        assert_eq!(rules(&fired), vec![(IncidentRule::LongFrames, 10, 10_000)], "one entry, counting each run of three");
        let later: Vec<(u64, u32)> = (0..3).map(|i| (60_000 + i * 17, 30)).collect();
        assert_eq!(rules(&stream.frames(&later)), vec![(IncidentRule::LongFrames, 1, 30_000)]);
        assert!(stream.frames(&[]).is_empty(), "nothing is reported twice");
    }

    #[test]
    fn a_stall_fires_on_one_frame() {
        let mut stream = Stream::new();
        assert!(stream.frames(&[(0, 249)]).is_empty(), "under 250 ms is not a stall");
        let fired = stream.frames(&[(5_000, 250), (9_000, 600)]);
        assert_eq!(rules(&fired), vec![(IncidentRule::Stall, 2, 600_000)], "the worst stall is kept");
        assert_eq!(fired[0].at_ms, 5_000, "the time is the first one in the window");
    }

    #[test]
    fn stalls_close_together_are_also_long_frames() {
        let mut stream = Stream::new();
        let fired = stream.frames(&[(0, 300), (310, 300), (620, 300)]);
        assert_eq!(rules(&fired), vec![(IncidentRule::Stall, 3, 300_000), (IncidentRule::LongFrames, 1, 300_000)]);
    }

    #[test]
    fn steady_memory_growth_fires() {
        let mut stream = Stream::new();
        // floors 200, 220, 240, 260, 280 MiB (+80 MiB, +40%); spikes within a minute do not count
        let minutes: [&[u64]; 6] = [&[200, 900, 210, 205, 200, 260], &[220, 230, 240, 250, 260, 270], &[240; 6], &[260; 6], &[280; 6], &[300]];
        let fired = stream.minutes(0, &minutes);
        assert_eq!(rules(&fired), vec![(IncidentRule::MemoryGrowth, 1, 80)]);
        assert_eq!(fired[0].at_ms, 300_000, "it fires when the fifth minute closes");
    }

    #[test]
    fn memory_growth_needs_every_floor_to_rise() {
        let mut stream = Stream::new();
        let dip: [&[u64]; 6] = [&[200; 6], &[260; 6], &[250; 6], &[290; 6], &[330; 6], &[330]];
        assert!(stream.minutes(0, &dip).is_empty());
    }

    #[test]
    fn small_or_relative_growth_alone_does_not_fire() {
        // +40 MiB is +20% of 200 MiB but under 64 MiB
        let mut stream = Stream::new();
        let small: [&[u64]; 6] = [&[200; 6], &[210; 6], &[220; 6], &[230; 6], &[240; 6], &[240]];
        assert!(stream.minutes(0, &small).is_empty());
        // +100 MiB is over 64 MiB but under 20% of 1000 MiB
        let mut stream = Stream::new();
        let large: [&[u64]; 6] = [&[1_000; 6], &[1_025; 6], &[1_050; 6], &[1_075; 6], &[1_100; 6], &[1_100]];
        assert!(stream.minutes(0, &large).is_empty());
    }

    #[test]
    fn memory_that_keeps_growing_fires_again_in_later_windows() {
        let mut stream = Stream::new();
        let first: [&[u64]; 5] = [&[100; 6], &[130; 6], &[160; 6], &[190; 6], &[220; 6]];
        assert!(stream.minutes(0, &first).is_empty(), "the fifth floor closes with the next minute's first sample");
        assert_eq!(rules(&stream.minutes(5, &[&[250; 6]])), vec![(IncidentRule::MemoryGrowth, 1, 120)]);
        assert_eq!(rules(&stream.minutes(6, &[&[280; 6]])), vec![(IncidentRule::MemoryGrowth, 1, 120)]);
        assert_eq!(rules(&stream.minutes(7, &[&[280; 6]])), vec![(IncidentRule::MemoryGrowth, 1, 120)]);
        // growth stops: the minute at 280 MiB again closes a floor that does not rise, and nothing fires
        assert!(stream.minutes(8, &[&[280; 6]]).is_empty());
    }
}
