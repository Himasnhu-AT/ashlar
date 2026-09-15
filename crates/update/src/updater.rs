//! The background checker an app keeps for its whole run: one worker thread that checks shortly
//! after launch and then every [`UpdateConfig::interval`] while automatic checks are on, checks at
//! once when asked, and keeps the last result (in memory and in the state file).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::saved::Saved;
use crate::{Fetch, Outcome, Release, UpdateError};

/// The wait after launch before the first automatic check.
pub const FIRST_DELAY: Duration = Duration::from_secs(15);
/// The time between automatic checks while the app runs.
pub const INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
/// The wait before an automatic check is tried again after one failed (offline, server down).
pub const RETRY: Duration = Duration::from_secs(60 * 60);

/// What an [`Updater`] checks and where it keeps its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateConfig {
    /// The releases API's app id (`my-app`).
    pub app: String,
    /// The running version (`env!("CARGO_PKG_VERSION")`).
    pub current_version: String,
    /// The releases API's base URL (no path). Empty: never check.
    pub base_url: String,
    pub first_delay: Duration,
    pub interval: Duration,
    pub retry: Duration,
    /// The JSON file the last result and "later" are kept in. `None`: memory only.
    pub state_file: Option<PathBuf>,
    /// The download page used when a release has no usable link of its own.
    pub fallback_download: String,
}

impl UpdateConfig {
    pub fn new(app: impl Into<String>, current_version: impl Into<String>, base_url: impl Into<String>) -> Self {
        UpdateConfig {
            app: app.into(),
            current_version: current_version.into(),
            base_url: base_url.into(),
            first_delay: FIRST_DELAY,
            interval: INTERVAL,
            retry: RETRY,
            state_file: None,
            fallback_download: String::new(),
        }
    }

    pub fn state_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_file = Some(path.into());
        self
    }

    pub fn fallback_download(mut self, url: impl Into<String>) -> Self {
        self.fallback_download = url.into();
        self
    }

    /// Overrides the timings (tests, or an app with its own cadence).
    pub fn timings(mut self, first_delay: Duration, interval: Duration, retry: Duration) -> Self {
        self.first_delay = first_delay;
        self.interval = interval;
        self.retry = retry;
        self
    }
}

/// When the next automatic check is due: `None` while automatic checks are off; `first_delay`
/// after `started` before any check was tried; else `interval` after the last success or `retry`
/// after the last failure.
pub fn next_due(automatic: bool, started: Instant, last: Option<(Instant, bool)>, cfg: &UpdateConfig) -> Option<Instant> {
    if !automatic {
        return None;
    }
    // an unrepresentable instant (a huge interval) means "not in this run"
    match last {
        None => started.checked_add(cfg.first_delay),
        Some((at, true)) => at.checked_add(cfg.interval),
        Some((at, false)) => at.checked_add(cfg.retry),
    }
}

/// How a check the person asked for ended (Check for Updates…), for the "up to date" answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finished {
    Available(Release),
    UpToDate,
    Failed(String),
}

/// A snapshot of the checker, cheap to take every frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub current_version: String,
    pub automatic: bool,
    /// A check is running.
    pub checking: bool,
    /// The newest release the last successful check (or the state file) found.
    pub latest: Option<Release>,
    /// When that check finished, Unix milliseconds.
    pub checked_at: Option<i64>,
    /// Why the last check failed, when it did.
    pub error: Option<String>,
    /// The version "later" was chosen for.
    pub dismissed: Option<String>,
    /// Bumped on every change.
    pub revision: u64,
}

impl Status {
    /// The newer release to tell the person about: newer than the running version and not put
    /// off with "later".
    pub fn available(&self) -> Option<&Release> {
        self.latest.as_ref().filter(|r| r.is_newer_than(&self.current_version) && self.dismissed.as_deref() != Some(r.version.as_str()))
    }

    /// A newer release exists, put off or not.
    pub fn newer(&self) -> Option<&Release> {
        self.latest.as_ref().filter(|r| r.is_newer_than(&self.current_version))
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

struct State {
    automatic: bool,
    requested: bool,
    shutdown: bool,
    save_pending: bool,
    checking: bool,
    latest: Option<Release>,
    checked_at: Option<i64>,
    error: Option<String>,
    dismissed: Option<String>,
    last_attempt: Option<(Instant, bool)>,
    finished: Option<Finished>,
    waker: Option<Waker>,
    revision: u64,
}

impl State {
    fn saved(&self) -> Saved {
        Saved { checked_at: self.checked_at.unwrap_or(0), latest: self.latest.clone(), dismissed: self.dismissed.clone() }
    }

    fn changed(&mut self) -> Option<Waker> {
        self.revision = self.revision.wrapping_add(1);
        self.waker.clone()
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn wake_ui(waker: Option<Waker>) {
    if let Some(waker) = waker {
        // a waker that panics must not take the checker with it
        let _ = catch_unwind(AssertUnwindSafe(|| waker()));
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).ok().and_then(|d| i64::try_from(d.as_millis()).ok()).unwrap_or(0)
}

/// The app's update checker. Dropping it stops the worker (a check in flight finishes in the
/// background and is discarded).
pub struct Updater {
    shared: Arc<Shared>,
    config: Arc<UpdateConfig>,
}

enum Job {
    Check { manual: bool },
    Save(Saved),
}

impl Updater {
    /// Starts the worker. `automatic`: the person's "check automatically" setting. Without a usable
    /// base URL the checker is [`Updater::off`]-like: it never fetches, and a manual check reports
    /// a failure.
    pub fn start(config: UpdateConfig, fetch: Box<dyn Fetch>, automatic: bool) -> Updater {
        let updater = Updater::idle(config, automatic);
        let shared = Arc::clone(&updater.shared);
        let config = Arc::clone(&updater.config);
        let spawned = std::thread::Builder::new().name("update-check".into()).spawn(move || work(&shared, fetch, &config));
        if let Err(e) = spawned {
            log::warn!("update checks off: no worker thread ({e})");
        }
        updater
    }

    /// A checker that never checks (no backend, tests): every call works, nothing is fetched.
    pub fn off(config: UpdateConfig) -> Updater {
        Updater::idle(config, false)
    }

    fn idle(config: UpdateConfig, automatic: bool) -> Updater {
        let state = State {
            automatic,
            requested: false,
            shutdown: false,
            save_pending: false,
            checking: false,
            latest: None,
            checked_at: None,
            error: None,
            dismissed: None,
            last_attempt: None,
            finished: None,
            waker: None,
            revision: 0,
        };
        Updater { shared: Arc::new(Shared { state: Mutex::new(state), wake: Condvar::new() }), config: Arc::new(config) }
    }

    pub fn config(&self) -> &UpdateConfig {
        &self.config
    }

    /// Called (from the worker thread) after every change, e.g. to request a repaint.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        self.shared.lock().waker = Some(Arc::new(waker));
    }

    /// Checks now (Check for Updates…). The answer comes back through [`Updater::take_finished`].
    /// Undoes "later": a release found this way is shown again.
    pub fn check_now(&self) {
        let mut state = self.shared.lock();
        state.requested = true;
        state.dismissed = None;
        let waker = state.changed();
        drop(state);
        self.shared.wake.notify_all();
        wake_ui(waker);
    }

    /// Turns automatic checks on or off (the person's setting).
    pub fn set_automatic(&self, on: bool) {
        let mut state = self.shared.lock();
        if state.automatic == on {
            return;
        }
        state.automatic = on;
        let waker = state.changed();
        drop(state);
        self.shared.wake.notify_all();
        wake_ui(waker);
    }

    pub fn automatic(&self) -> bool {
        self.shared.lock().automatic
    }

    pub fn status(&self) -> Status {
        let state = self.shared.lock();
        Status {
            current_version: self.config.current_version.clone(),
            automatic: state.automatic,
            checking: state.checking,
            latest: state.latest.clone(),
            checked_at: state.checked_at,
            error: state.error.clone(),
            dismissed: state.dismissed.clone(),
            revision: state.revision,
        }
    }

    /// The newer release to show, if any ([`Status::available`]).
    pub fn available(&self) -> Option<Release> {
        self.status().available().cloned()
    }

    /// Bumped on every change: a cheap "did anything change" for the UI.
    pub fn revision(&self) -> u64 {
        self.shared.lock().revision
    }

    /// "Later": no notice for `version` again (a newer release shows). Kept in the state file.
    pub fn dismiss(&self, version: &str) {
        let mut state = self.shared.lock();
        state.dismissed = Some(version.to_string());
        state.save_pending = true;
        let waker = state.changed();
        drop(state);
        self.shared.wake.notify_all();
        wake_ui(waker);
    }

    /// The end of the last check the person asked for, once.
    pub fn take_finished(&self) -> Option<Finished> {
        self.shared.lock().finished.take()
    }

    /// Where to download `release` ([`Release::download_url`] with the configured fallback).
    pub fn download_url(&self, release: &Release) -> Option<String> {
        release.download_url(&self.config.fallback_download)
    }

    /// Runs the worker's check on the calling thread, for tests of the state handling.
    #[doc(hidden)]
    pub fn apply(&self, result: Result<Outcome, UpdateError>, manual: bool) {
        if let Some(saved) = record(&self.shared, result, manual)
            && let Some(path) = &self.config.state_file
        {
            saved.store(path);
        }
    }
}

impl Drop for Updater {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.wake.notify_all();
    }
}

/// Stores a check's result; returns what to write to the state file after a success.
fn record(shared: &Shared, result: Result<Outcome, UpdateError>, manual: bool) -> Option<Saved> {
    let mut state = shared.lock();
    state.checking = false;
    state.last_attempt = Some((Instant::now(), result.is_ok()));
    let saved = match result {
        Ok(outcome) => {
            state.latest = outcome.latest().cloned();
            state.checked_at = Some(now_ms());
            state.error = None;
            if manual {
                state.finished = Some(match outcome {
                    Outcome::Available(release) => Finished::Available(release),
                    Outcome::UpToDate { .. } => Finished::UpToDate,
                });
            }
            Some(state.saved())
        }
        Err(e) => {
            log::info!("update check failed: {e}");
            state.error = Some(e.to_string());
            if manual {
                state.finished = Some(Finished::Failed(e.to_string()));
            }
            None
        }
    };
    let waker = state.changed();
    drop(state);
    wake_ui(waker);
    saved
}

fn next_job(shared: &Shared, started: Instant, config: &UpdateConfig) -> Option<Job> {
    let mut state = shared.lock();
    loop {
        if state.shutdown {
            return None;
        }
        if state.save_pending {
            state.save_pending = false;
            return Some(Job::Save(state.saved()));
        }
        if state.requested {
            state.requested = false;
            return Some(Job::Check { manual: true });
        }
        let now = Instant::now();
        match next_due(state.automatic, started, state.last_attempt, config) {
            Some(due) if due <= now => return Some(Job::Check { manual: false }),
            Some(due) => state = shared.wake.wait_timeout(state, due.saturating_duration_since(now)).unwrap_or_else(PoisonError::into_inner).0,
            None => state = shared.wake.wait(state).unwrap_or_else(PoisonError::into_inner),
        }
    }
}

fn work(shared: &Shared, mut fetch: Box<dyn Fetch>, config: &UpdateConfig) {
    let started = Instant::now();
    if let Some(path) = &config.state_file {
        let saved = Saved::load(path);
        let mut state = shared.lock();
        // only a release of this app; a stale one (older than or equal to the running version)
        // still answers "latest" until the first check replaces it
        state.latest = saved.latest.filter(|r| r.app.is_empty() || r.app == config.app);
        state.checked_at = (saved.checked_at > 0).then_some(saved.checked_at);
        if state.dismissed.is_none() && !state.save_pending {
            state.dismissed = saved.dismissed;
        }
        let waker = state.changed();
        drop(state);
        wake_ui(waker);
    }
    while let Some(job) = next_job(shared, started, config) {
        match job {
            Job::Save(saved) => {
                if let Some(path) = &config.state_file {
                    saved.store(path);
                }
            }
            Job::Check { manual } => {
                let waker = {
                    let mut state = shared.lock();
                    state.checking = true;
                    state.changed()
                };
                wake_ui(waker);
                let result = catch_unwind(AssertUnwindSafe(|| crate::check(fetch.as_mut(), &config.base_url, &config.app, &config.current_version)))
                    .unwrap_or_else(|_| Err(UpdateError::Network("the check stopped unexpectedly".into())));
                if let Some(saved) = record(shared, result, manual)
                    && let Some(path) = &config.state_file
                {
                    saved.store(path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> UpdateConfig {
        UpdateConfig::new("app", "1.0.0", "https://api.example.com").timings(
            Duration::from_secs(10),
            Duration::from_secs(100),
            Duration::from_secs(20),
        )
    }

    #[test]
    fn schedule_waits_first_then_interval_or_retry() {
        let t0 = Instant::now();
        let c = cfg();
        assert_eq!(next_due(false, t0, None, &c), None);
        assert_eq!(next_due(true, t0, None, &c), Some(t0 + Duration::from_secs(10)));
        let t1 = t0 + Duration::from_secs(30);
        assert_eq!(next_due(true, t0, Some((t1, true)), &c), Some(t1 + Duration::from_secs(100)));
        assert_eq!(next_due(true, t0, Some((t1, false)), &c), Some(t1 + Duration::from_secs(20)));
        assert_eq!(next_due(false, t0, Some((t1, true)), &c), None);
    }

    #[test]
    fn a_huge_interval_never_overflows() {
        let c = cfg().timings(Duration::MAX, Duration::MAX, Duration::MAX);
        let t0 = Instant::now();
        assert_eq!(next_due(true, t0, None, &c), None);
        assert_eq!(next_due(true, t0, Some((t0, true)), &c), None);
    }
}
