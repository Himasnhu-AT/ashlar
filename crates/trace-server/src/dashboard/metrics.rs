//! Metric definitions, computed from rollup rows. Pure functions so the definitions are testable and the
//! pages, the reports and the JSON export all agree.
//!
//! - Crash-free sessions = 1 - (crashed + abnormal) / sessions that have ended.
//! - Crash-free installs = 1 - installs with a crashed or abnormal session / active installs, counted per
//!   day and summed over the range (install-days), so an install that crashes on two days counts twice.
//! - Daily opens = sessions started that day with process kind `gui`.

use std::collections::BTreeMap;

use ashlar_trace_proto::FrameStats;
use serde::Serialize;

use super::fmt;
use crate::store::{DayTotals, PerfRow};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SessionSummary {
    pub sessions: i64,
    pub opens: i64,
    /// Sessions that have ended: exited, crashed or abnormal.
    pub ended: i64,
    pub crashed: i64,
    pub abnormal: i64,
    /// Active installs summed over the days of the range.
    pub install_days: i64,
    pub crashed_install_days: i64,
    pub duration_s: i64,
    pub focused_s: i64,
    pub active_s: i64,
    pub idle_s: i64,
    pub crash_free_sessions: Option<f64>,
    pub crash_free_installs: Option<f64>,
    pub avg_session_s: Option<f64>,
}

impl SessionSummary {
    /// `active_by_day` is the exact number of distinct active installs per day, the denominator of the
    /// crash-free installs rate.
    pub fn from_days(days: &[DayTotals], active_by_day: &[(i64, i64)]) -> Self {
        let mut s = Self { install_days: active_by_day.iter().map(|(_, n)| *n).sum(), ..Self::default() };
        for d in days {
            s.sessions += d.sessions;
            s.opens += d.opens;
            s.ended += d.exited + d.crashed + d.abnormal;
            s.crashed += d.crashed;
            s.abnormal += d.abnormal;
            s.crashed_install_days += d.crashed_installs;
            s.duration_s += d.duration_s;
            s.focused_s += d.focused_s;
            s.active_s += d.active_s;
            s.idle_s += d.idle_s;
        }
        s.crash_free_sessions = fmt::ratio(s.crashed + s.abnormal, s.ended).map(|bad| 1.0 - bad);
        s.crash_free_installs = fmt::ratio(s.crashed_install_days, s.install_days).map(|bad| 1.0 - bad);
        s.avg_session_s = fmt::ratio(s.duration_s, s.sessions);
        s
    }
}

/// Values for each day in `from..=to`, zero where the rollup has no row.
pub fn fill_days<T: Copy + Default>(from: i64, to: i64, rows: impl Iterator<Item = (i64, T)>) -> Vec<T> {
    let len = usize::try_from(to - from + 1).unwrap_or(0);
    let mut out = vec![T::default(); len];
    for (day, value) in rows {
        if let Ok(index) = usize::try_from(day - from)
            && let Some(slot) = out.get_mut(index)
        {
            *slot = value;
        }
    }
    out
}

/// Frame, CPU, memory and startup figures for a set of perf rollup rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PerfSummary {
    pub frames: u64,
    pub p50_us: Option<u32>,
    pub p90_us: Option<u32>,
    pub p99_us: Option<u32>,
    pub max_us: u32,
    pub slow_rate: Option<f64>,
    pub frozen_rate: Option<f64>,
    pub startup_ms: Option<f64>,
    pub cpu_pct: Option<f64>,
    pub rss_mb: Option<f64>,
    pub rss_peak_mb: i64,
}

pub struct PerfAcc {
    pub frames: FrameStats,
    cpu_sum: f64,
    cpu_n: i64,
    rss_sum: i64,
    rss_n: i64,
    rss_peak: i64,
    startup_sum: i64,
    startup_n: i64,
}

impl Default for PerfAcc {
    fn default() -> Self {
        Self { frames: FrameStats::empty(), cpu_sum: 0.0, cpu_n: 0, rss_sum: 0, rss_n: 0, rss_peak: 0, startup_sum: 0, startup_n: 0 }
    }
}

impl PerfAcc {
    /// Merges one row in; histograms are added with [`FrameStats::merge`].
    pub fn add(&mut self, row: &PerfRow) {
        self.frames.merge(&row.frames);
        self.cpu_sum += row.cpu_sum;
        self.cpu_n += row.cpu_n;
        self.rss_sum += row.rss_sum;
        self.rss_n += row.rss_n;
        self.rss_peak = self.rss_peak.max(row.rss_peak_mb);
        self.startup_sum += row.startup_sum;
        self.startup_n += row.startup_n;
    }

    pub fn summary(&self) -> PerfSummary {
        let f = &self.frames;
        let count = i64::try_from(f.count).unwrap_or(i64::MAX);
        PerfSummary {
            frames: f.count,
            p50_us: f.quantile_us(0.50),
            p90_us: f.quantile_us(0.90),
            p99_us: f.quantile_us(0.99),
            max_us: f.max_us,
            slow_rate: fmt::ratio(i64::try_from(f.slow).unwrap_or(i64::MAX), count),
            frozen_rate: fmt::ratio(i64::try_from(f.frozen).unwrap_or(i64::MAX), count),
            startup_ms: fmt::ratio(self.startup_sum, self.startup_n),
            cpu_pct: (self.cpu_n > 0).then(|| self.cpu_sum / self.cpu_n as f64),
            rss_mb: fmt::ratio(self.rss_sum, self.rss_n),
            rss_peak_mb: self.rss_peak,
        }
    }
}

pub fn summarize_perf<'a>(rows: impl Iterator<Item = &'a PerfRow>) -> (PerfAcc, PerfSummary) {
    let mut acc = PerfAcc::default();
    for row in rows {
        acc.add(row);
    }
    let summary = acc.summary();
    (acc, summary)
}

/// Per-day summaries for `from..=to`; days without rows have an all-empty summary.
pub fn perf_by_day(rows: &[PerfRow], from: i64, to: i64) -> Vec<PerfSummary> {
    let mut accs: BTreeMap<i64, PerfAcc> = BTreeMap::new();
    for row in rows {
        accs.entry(row.day).or_default().add(row);
    }
    let len = usize::try_from(to - from + 1).unwrap_or(0);
    (0..len).map(|i| accs.get(&(from + i as i64)).map(PerfAcc::summary).unwrap_or_default()).collect()
}

/// Summaries grouped by a key such as version or operating system, busiest group first.
pub fn perf_grouped(rows: &[PerfRow], key: impl Fn(&PerfRow) -> &str) -> Vec<(String, PerfSummary)> {
    let mut accs: BTreeMap<String, PerfAcc> = BTreeMap::new();
    for row in rows {
        accs.entry(key(row).to_string()).or_default().add(row);
    }
    let mut out: Vec<(String, PerfSummary)> = accs.into_iter().map(|(name, acc)| (name, acc.summary())).collect();
    out.sort_by(|a, b| b.1.frames.cmp(&a.1.frames).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Host-defined metrics gathered from recent raw rows: counters are summed, gauges averaged.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CustomMetrics {
    pub counters: Vec<(String, u64)>,
    pub gauges: Vec<(String, f64)>,
    pub windows: usize,
}

pub fn custom_metrics(rows: &[crate::store::CustomRow]) -> CustomMetrics {
    let mut counters: BTreeMap<String, u64> = BTreeMap::new();
    let mut gauges: BTreeMap<String, (f64, u64)> = BTreeMap::new();
    for row in rows {
        if let Some(map) = row.counters.as_deref().and_then(|text| serde_json::from_str::<BTreeMap<String, u64>>(text).ok()) {
            for (name, value) in map {
                let slot = counters.entry(name).or_default();
                *slot = slot.saturating_add(value);
            }
        }
        if let Some(map) = row.gauges.as_deref().and_then(|text| serde_json::from_str::<BTreeMap<String, f64>>(text).ok()) {
            for (name, value) in map.into_iter().filter(|(_, v)| v.is_finite()) {
                let slot = gauges.entry(name).or_default();
                slot.0 += value;
                slot.1 += 1;
            }
        }
    }
    CustomMetrics {
        counters: counters.into_iter().collect(),
        gauges: gauges.into_iter().map(|(name, (sum, n))| (name, sum / n.max(1) as f64)).collect(),
        windows: rows.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(day: i64, sessions: i64, exited: i64, crashed: i64, abnormal: i64, installs: i64, crashed_installs: i64) -> DayTotals {
        DayTotals {
            day,
            sessions,
            opens: sessions,
            installs,
            exited,
            crashed,
            abnormal,
            crashed_installs,
            duration_s: sessions * 60,
            ..DayTotals::default()
        }
    }

    #[test]
    fn crash_free_rates_follow_the_definitions() {
        let s = SessionSummary::from_days(&[day(1, 4, 3, 1, 0, 3, 1), day(2, 6, 4, 1, 1, 4, 1)], &[(1, 3), (2, 4)]);
        assert_eq!(s.sessions, 10);
        assert_eq!(s.ended, 10);
        assert!((s.crash_free_sessions.unwrap() - 0.7).abs() < 1e-12);
        assert!((s.crash_free_installs.unwrap() - (1.0 - 2.0 / 7.0)).abs() < 1e-12);
        assert_eq!(s.avg_session_s, Some(60.0));
    }

    #[test]
    fn nothing_ended_means_unknown_not_zero_or_nan() {
        let s = SessionSummary::from_days(&[day(1, 2, 0, 0, 0, 1, 0)], &[(1, 1)]);
        assert_eq!(s.crash_free_sessions, None);
        assert_eq!(SessionSummary::from_days(&[], &[]).crash_free_installs, None);
        assert_eq!(SessionSummary::from_days(&[], &[]).avg_session_s, None);
    }

    #[test]
    fn days_are_filled_and_stray_rows_ignored() {
        let filled = fill_days(10, 12, [(11, 5i64), (99, 1), (3, 2)].into_iter());
        assert_eq!(filled, vec![0, 5, 0]);
        assert!(fill_days::<i64>(5, 1, std::iter::empty()).is_empty());
    }

    fn perf_row(day: i64, version: &str, os: &str, frames: &[u32]) -> PerfRow {
        let mut stats = FrameStats::empty();
        for us in frames {
            stats.record(*us);
        }
        PerfRow {
            day,
            version: version.into(),
            os_name: os.into(),
            frames: stats,
            cpu_sum: 20.0,
            cpu_n: 2,
            startup_sum: 300,
            startup_n: 1,
            ..PerfRow::default()
        }
    }

    #[test]
    fn percentiles_come_from_merged_histograms() {
        let rows = vec![perf_row(1, "1.0", "linux", &[5_000; 9]), perf_row(1, "1.0", "mac", &[30_000])];
        let (_, summary) = summarize_perf(rows.iter());
        assert_eq!(summary.frames, 10);
        assert_eq!(summary.p50_us, Some(6_000));
        assert_eq!(summary.p90_us, Some(6_000));
        assert_eq!(summary.p99_us, Some(30_000));
        assert_eq!(summary.slow_rate, Some(0.1));
        assert_eq!(summary.cpu_pct, Some(10.0));
        assert_eq!(summary.startup_ms, Some(300.0));
        let grouped = perf_grouped(&rows, |r| r.os_name.as_str());
        assert_eq!(grouped[0].0, "linux");
        let by_day = perf_by_day(&rows, 0, 2);
        assert_eq!((by_day[0].frames, by_day[1].frames, by_day[2].p90_us), (0, 10, None));
    }

    #[test]
    fn custom_metrics_sum_counters_and_average_gauges() {
        let rows = vec![
            crate::store::CustomRow { counters: Some(r#"{"saves":2}"#.into()), gauges: Some(r#"{"layers":4.0}"#.into()) },
            crate::store::CustomRow { counters: Some(r#"{"saves":3}"#.into()), gauges: Some(r#"{"layers":6.0}"#.into()) },
            crate::store::CustomRow { counters: Some("not json".into()), gauges: None },
        ];
        let m = custom_metrics(&rows);
        assert_eq!(m.counters, vec![("saves".to_string(), 5)]);
        assert_eq!(m.gauges, vec![("layers".to_string(), 5.0)]);
        assert_eq!(m.windows, 3);
    }
}
