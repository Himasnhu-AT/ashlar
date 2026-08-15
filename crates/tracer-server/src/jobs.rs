//! Background jobs: rollups, retention and stale-session closing. Each is a plain function over a [`Store`]
//! so tests and the CLI can call it directly; [`spawn_timers`] runs them on timers inside `serve`.

use std::sync::Arc;
use std::time::Duration;

use crate::ingest::DAY_MS;
use crate::store::{PruneStats, Store, StoreResult};

/// Days (including today) recomputed by the rollup timer.
pub const ROLLUP_DAYS: u32 = 3;
pub const ROLLUP_EVERY: Duration = Duration::from_secs(5 * 60);
pub const RETENTION_EVERY: Duration = Duration::from_secs(60 * 60);
/// A session still `ok` this long after its last item is closed as `abnormal`.
pub const STALE_AFTER_MS: i64 = DAY_MS;

/// Recomputes the `daily_*` rollups for the last `days` days ending today. Days older than the retention
/// window are never touched: their raw rows are gone, and recomputing would erase the kept rollups.
pub fn rollup_recent(store: &dyn Store, now: i64, days: u32, retention_days: u32) -> StoreResult<()> {
    let today = now.div_euclid(DAY_MS);
    let from = (today - i64::from(days.max(1)) + 1).max(today - i64::from(retention_days));
    store.rollup(from, today)
}

/// Closes stale sessions, then deletes raw rows past retention.
pub fn run_retention(store: &dyn Store, now: i64, retention_days: u32) -> StoreResult<PruneStats> {
    close_stale_sessions(store, now)?;
    store.prune(now, retention_days)
}

/// Marks sessions still `ok` whose last item is over a day old as `abnormal`. Returns how many.
pub fn close_stale_sessions(store: &dyn Store, now: i64) -> StoreResult<u64> {
    store.close_stale_sessions(now - STALE_AFTER_MS)
}

/// Starts the rollup and retention timers on the current tokio runtime. Abort the handles to stop them.
pub fn spawn_timers(store: Arc<dyn Store>, retention_days: u32) -> Vec<tokio::task::JoinHandle<()>> {
    let rollup_store = Arc::clone(&store);
    let rollup = tokio::spawn(async move {
        let mut tick = tokio::time::interval(ROLLUP_EVERY);
        loop {
            tick.tick().await;
            let store = Arc::clone(&rollup_store);
            let result = tokio::task::spawn_blocking(move || {
                close_stale_sessions(store.as_ref(), crate::now_ms())?;
                rollup_recent(store.as_ref(), crate::now_ms(), ROLLUP_DAYS, retention_days)
            })
            .await;
            match result {
                Ok(Ok(())) => log::debug!("rollup finished"),
                Ok(Err(error)) => log::error!("rollup failed: {error}"),
                Err(_) => log::error!("rollup task failed"),
            }
        }
    });
    let retention = tokio::spawn(async move {
        let mut tick = tokio::time::interval(RETENTION_EVERY);
        loop {
            tick.tick().await;
            let store = Arc::clone(&store);
            match tokio::task::spawn_blocking(move || run_retention(store.as_ref(), crate::now_ms(), retention_days)).await {
                Ok(Ok(stats)) => log::info!(
                    "retention removed {} events, {} perf rows, {} crashes, {} sessions",
                    stats.events,
                    stats.perf,
                    stats.crashes,
                    stats.sessions
                ),
                Ok(Err(error)) => log::error!("retention failed: {error}"),
                Err(_) => log::error!("retention task failed"),
            }
        }
    });
    vec![rollup, retention]
}
