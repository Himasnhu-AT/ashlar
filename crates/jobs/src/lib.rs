//! Background jobs: progress, cancel, panic isolation, apply-on-UI-thread.
//!
//! A job runs on a worker thread with a snapshot of what it needs (moved in: an `Arc` of the
//! document, a path…). It reports progress through [`Progress`], stops when its [`CancelToken`] is
//! cancelled, and runs under `catch_unwind`. It finishes with an [`Apply`]: a closure the UI thread
//! runs on the app state when [`Jobs::drain_ready`] sees it (each frame, or when the `wake` hook
//! asked for one). That is where the document is edited, as one undo step.
//!
//! [`Jobs::list`] / [`Jobs::to_json`] describe every job for the progress panel and for agents.
//! On wasm32 jobs run inline (no threads); long work there uses [`Jobs::spawn_stepped`], which
//! `drain_ready` advances a few steps per frame.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod pool;

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What a finished job does to the app state, on the UI thread.
pub type Apply<S> = Box<dyn FnOnce(&mut S) -> Result<Value, String> + Send>;

/// Box an apply closure.
pub fn apply<S>(f: impl FnOnce(&mut S) -> Result<Value, String> + Send + 'static) -> Apply<S> {
    Box::new(f)
}

/// Called (from any thread) when a job has something to show: started, progressed, finished.
/// Apps request a repaint here, so an idle app with no reporting jobs does no work.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Finished jobs remembered for [`Jobs::recent`].
const HISTORY: usize = 50;
/// Progress moves smaller than this do not wake the UI.
const WAKE_STEP: f64 = 0.005;
/// Longest progress message kept.
const MAX_MESSAGE: usize = 512;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Monotonic time, where the platform has a clock (not on wasm32-unknown-unknown).
mod clock {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub type Instant = std::time::Instant;
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub fn now() -> Option<Instant> {
        Some(Instant::now())
    }

    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    #[derive(Clone, Copy, Debug)]
    pub struct Instant;
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    impl Instant {
        pub fn elapsed(&self) -> std::time::Duration {
            std::time::Duration::ZERO
        }
    }
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    pub fn now() -> Option<Instant> {
        None
    }
}

/// A job's id, unique within one [`Jobs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(pub u64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Cooperative cancellation: the job checks it (or the `bool` its progress calls return).
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Where a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Waiting for a worker.
    Queued,
    Running,
    /// Finished on its worker; applied at the next [`Jobs::drain_ready`].
    Ready,
    /// Applied.
    Done,
    Failed,
    Cancelled,
}

impl Status {
    pub fn is_finished(self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Cancelled)
    }
}

struct State {
    status: Status,
    fraction: Option<f64>,
    message: String,
    started: Option<clock::Instant>,
    woken_fraction: f64,
}

struct Shared {
    cancel: CancelToken,
    state: Mutex<State>,
    wake: Arc<Mutex<Option<Wake>>>,
}

impl Shared {
    fn wake(&self) {
        let w = lock(&self.wake).clone();
        if let Some(w) = w {
            // the app's hook must not take a worker down
            let _ = catch_unwind(AssertUnwindSafe(|| w()));
        }
    }

    /// Queued → Running; `false` when the job was cancelled before it started.
    fn begin(&self) -> bool {
        {
            let mut s = lock(&self.state);
            if s.status != Status::Queued {
                return false;
            }
            s.status = Status::Running;
            s.started = clock::now();
        }
        self.wake();
        true
    }

    fn set_status(&self, status: Status) {
        lock(&self.state).status = status;
    }

    fn status(&self) -> Status {
        lock(&self.state).status
    }
}

/// Handed to a running job: report progress, check for cancellation.
pub struct Progress {
    shared: Arc<Shared>,
}

impl Progress {
    /// Report progress 0..=1 (clamped; NaN ignored). Returns `false` once the job was cancelled.
    pub fn set(&self, fraction: f64) -> bool {
        self.update(Some(fraction), None)
    }

    /// Report `done` of `total` steps. Returns `false` once the job was cancelled.
    pub fn steps(&self, done: u64, total: u64) -> bool {
        let f = if total == 0 { 0.0 } else { done as f64 / total as f64 };
        self.update(Some(f), None)
    }

    /// Say what the job is doing ("Reading frame 12 of 300").
    pub fn message(&self, message: impl Into<String>) {
        self.update(None, Some(message.into()));
    }

    /// Progress and message together. Returns `false` once the job was cancelled.
    pub fn report(&self, fraction: f64, message: impl Into<String>) -> bool {
        self.update(Some(fraction), Some(message.into()))
    }

    pub fn is_cancelled(&self) -> bool {
        self.shared.cancel.is_cancelled()
    }

    /// A token to hand to code that only needs to check for cancellation.
    pub fn token(&self) -> CancelToken {
        self.shared.cancel.clone()
    }

    fn update(&self, fraction: Option<f64>, message: Option<String>) -> bool {
        let wake = {
            let mut s = lock(&self.shared.state);
            let mut wake = false;
            if let Some(f) = fraction.filter(|f| !f.is_nan()) {
                let f = f.clamp(0.0, 1.0);
                s.fraction = Some(f);
                if (f - s.woken_fraction).abs() >= WAKE_STEP || f >= 1.0 {
                    s.woken_fraction = f;
                    wake = true;
                }
            }
            if let Some(mut m) = message {
                if let Some((i, _)) = m.char_indices().nth(MAX_MESSAGE) {
                    m.truncate(i);
                }
                if m != s.message {
                    s.message = m;
                    wake = true;
                }
            }
            wake
        };
        if wake {
            self.shared.wake();
        }
        !self.is_cancelled()
    }
}

/// What a stepped job's step returns.
pub enum Step<S> {
    /// More to do: call the step again.
    Continue,
    Done(Apply<S>),
    Failed(String),
}

enum Outcome<S> {
    Apply(Apply<S>),
    Failed(String),
    Cancelled,
}

struct Inbox<S> {
    items: Mutex<VecDeque<(JobId, Outcome<S>)>>,
    cv: Condvar,
}

impl<S> Inbox<S> {
    fn push(&self, id: JobId, outcome: Outcome<S>) {
        lock(&self.items).push_back((id, outcome));
        self.cv.notify_all();
    }
}

struct Entry {
    id: JobId,
    kind: &'static str,
    label: String,
    shared: Arc<Shared>,
}

type StepFn<S> = Box<dyn FnMut(&Progress) -> Step<S> + Send>;

struct SteppedEntry<S> {
    id: JobId,
    shared: Arc<Shared>,
    step: StepFn<S>,
}

/// One row of the progress panel / `jobs.list`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobInfo {
    pub id: JobId,
    pub kind: String,
    pub label: String,
    pub status: Status,
    /// 0..=1, `None` until the job reports.
    pub fraction: Option<f64>,
    pub message: String,
    pub elapsed_secs: Option<f64>,
    /// Estimated seconds left, from the elapsed time and the fraction done.
    pub eta_secs: Option<f64>,
    pub cancellable: bool,
}

/// A finished job (kept for the progress panel and agents).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JobRecord {
    pub id: JobId,
    pub kind: String,
    pub label: String,
    /// `done`, `failed` or `cancelled`.
    pub status: Status,
    /// The error for a failed job, else the last progress message.
    pub message: String,
    /// What the apply returned.
    pub result: Value,
    pub seconds: Option<f64>,
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown error".into())
}

/// The background jobs of one app state `S`.
pub struct Jobs<S: 'static> {
    pool: Option<pool::Pool>,
    entries: Vec<Entry>,
    stepped: Vec<SteppedEntry<S>>,
    steps_per_drain: usize,
    inbox: Arc<Inbox<S>>,
    history: VecDeque<JobRecord>,
    next_id: u64,
    wake: Arc<Mutex<Option<Wake>>>,
}

impl<S: 'static> Default for Jobs<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: 'static> Jobs<S> {
    /// Workers bounded by the machine (cores − 1, at least 1, at most 8); inline on wasm32.
    pub fn new() -> Self {
        if cfg!(target_arch = "wasm32") {
            return Self::inline();
        }
        let n = std::thread::available_parallelism().map(|n| n.get().saturating_sub(1)).unwrap_or(1).clamp(1, 8);
        Self::with_workers(n)
    }

    /// At most `n` jobs run at once (at least 1).
    pub fn with_workers(n: usize) -> Self {
        Self::build(Some(pool::Pool::new(n)))
    }

    /// No threads: `spawn` runs the job before returning (its apply still waits for
    /// `drain_ready`); stepped jobs advance in `drain_ready`. What wasm32 uses; handy in tests.
    pub fn inline() -> Self {
        Self::build(None)
    }

    fn build(pool: Option<pool::Pool>) -> Self {
        Self {
            pool,
            entries: vec![],
            stepped: vec![],
            steps_per_drain: 4,
            inbox: Arc::new(Inbox { items: Mutex::new(VecDeque::new()), cv: Condvar::new() }),
            history: VecDeque::new(),
            next_id: 1,
            wake: Arc::default(),
        }
    }

    /// The bound on concurrently running jobs (0 when inline).
    pub fn workers(&self) -> usize {
        self.pool.as_ref().map_or(0, pool::Pool::workers)
    }

    /// Steps each inline stepped job takes per [`Jobs::drain_ready`] (at least 1).
    pub fn set_steps_per_drain(&mut self, n: usize) {
        self.steps_per_drain = n.max(1);
    }

    /// The hook called when a job starts, reports progress or finishes (from any thread).
    pub fn set_wake(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        *lock(&self.wake) = Some(Arc::new(wake));
    }

    fn register(&mut self, kind: &'static str, label: String) -> (JobId, Arc<Shared>) {
        let id = JobId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let shared = Arc::new(Shared {
            cancel: CancelToken::default(),
            state: Mutex::new(State { status: Status::Queued, fraction: None, message: String::new(), started: None, woken_fraction: 0.0 }),
            wake: self.wake.clone(),
        });
        self.entries.push(Entry { id, kind, label, shared: shared.clone() });
        (id, shared)
    }

    /// Start a job. `input` is the snapshot it works on; `work` returns the [`Apply`] that lands
    /// the result, or an error message. A panic in `work` fails the job, nothing else.
    pub fn spawn<I: Send + 'static>(
        &mut self,
        kind: &'static str,
        label: impl Into<String>,
        input: I,
        work: impl FnOnce(I, &Progress) -> Result<Apply<S>, String> + Send + 'static,
    ) -> JobId {
        let (id, shared) = self.register(kind, label.into());
        let inbox = self.inbox.clone();
        let task = move || {
            if !shared.begin() {
                return;
            }
            let progress = Progress { shared: shared.clone() };
            let r = catch_unwind(AssertUnwindSafe(|| work(input, &progress)));
            let cancelled = shared.cancel.is_cancelled();
            let outcome = match r {
                _ if cancelled => Outcome::Cancelled,
                Ok(Ok(apply)) => Outcome::Apply(apply),
                Ok(Err(e)) => Outcome::Failed(e),
                Err(p) => {
                    let msg = panic_message(p.as_ref());
                    log::error!("background job panicked: {msg}");
                    Outcome::Failed(format!("internal error ({msg}); the job was stopped"))
                }
            };
            shared.set_status(Status::Ready);
            inbox.push(id, outcome);
            shared.wake();
        };
        match &mut self.pool {
            Some(pool) => pool.submit(Box::new(task)),
            None => task(),
        }
        id
    }

    /// Start a job made of steps (`step` is called until it returns `Done`/`Failed`). On threads it
    /// runs like [`Jobs::spawn`]; inline (wasm32) [`Jobs::drain_ready`] advances it a few steps per
    /// call, so long work never freezes a frame. Apps should keep frames coming while
    /// [`Jobs::needs_frames`] is true.
    pub fn spawn_stepped<I: Send + 'static>(
        &mut self,
        kind: &'static str,
        label: impl Into<String>,
        input: I,
        mut step: impl FnMut(&mut I, &Progress) -> Step<S> + Send + 'static,
    ) -> JobId {
        let mut input = input;
        if self.pool.is_some() {
            return self.spawn(kind, label, (), move |(), p| {
                loop {
                    if p.is_cancelled() {
                        return Err("cancelled".into());
                    }
                    match step(&mut input, p) {
                        Step::Continue => {}
                        Step::Done(apply) => return Ok(apply),
                        Step::Failed(e) => return Err(e),
                    }
                }
            });
        }
        let (id, shared) = self.register(kind, label.into());
        self.stepped.push(SteppedEntry { id, shared, step: Box::new(move |p| step(&mut input, p)) });
        id
    }

    /// Whether inline stepped jobs are waiting for `drain_ready` calls.
    pub fn needs_frames(&self) -> bool {
        !self.stepped.is_empty()
    }

    /// Whether any job is queued, running or waiting to be applied.
    pub fn is_busy(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Advance inline stepped jobs by up to `steps` steps each.
    fn drive(&mut self, steps: usize) {
        let inbox = self.inbox.clone();
        self.stepped.retain_mut(|job| {
            let status = job.shared.status();
            if status == Status::Queued && !job.shared.begin() {
                return false;
            }
            if status != Status::Queued && status != Status::Running {
                return false;
            }
            let progress = Progress { shared: job.shared.clone() };
            for _ in 0..steps {
                if job.shared.cancel.is_cancelled() {
                    job.shared.set_status(Status::Ready);
                    inbox.push(job.id, Outcome::Cancelled);
                    return false;
                }
                let outcome = match catch_unwind(AssertUnwindSafe(|| (job.step)(&progress))) {
                    Ok(Step::Continue) => continue,
                    Ok(Step::Done(apply)) => Outcome::Apply(apply),
                    Ok(Step::Failed(e)) => Outcome::Failed(e),
                    Err(p) => Outcome::Failed(format!("internal error ({}); the job was stopped", panic_message(p.as_ref()))),
                };
                job.shared.set_status(Status::Ready);
                inbox.push(job.id, outcome);
                return false;
            }
            true
        });
    }

    /// Call once per frame on the UI thread: advances inline stepped jobs, then applies every
    /// finished job to `state` in the order they finished (each apply under `catch_unwind`). A
    /// job cancelled after it finished is not applied. Returns what finished.
    pub fn drain_ready(&mut self, state: &mut S) -> Vec<JobRecord> {
        if !self.stepped.is_empty() {
            self.drive(self.steps_per_drain);
        }
        let ready: Vec<(JobId, Outcome<S>)> = lock(&self.inbox.items).drain(..).collect();
        let mut out = Vec::with_capacity(ready.len());
        for (id, outcome) in ready {
            let Some(pos) = self.entries.iter().position(|e| e.id == id) else { continue };
            let entry = self.entries.remove(pos);
            let cancelled = entry.shared.cancel.is_cancelled();
            let (status, message, result) = match outcome {
                Outcome::Apply(_) | Outcome::Cancelled if cancelled => (Status::Cancelled, String::new(), Value::Null),
                Outcome::Cancelled => (Status::Cancelled, String::new(), Value::Null),
                Outcome::Failed(e) => (Status::Failed, e, Value::Null),
                Outcome::Apply(apply) => match catch_unwind(AssertUnwindSafe(|| apply(state))) {
                    Ok(Ok(v)) => (Status::Done, lock(&entry.shared.state).message.clone(), v),
                    Ok(Err(e)) => (Status::Failed, e, Value::Null),
                    Err(p) => {
                        let msg = panic_message(p.as_ref());
                        log::error!("applying job `{}` panicked: {msg}", entry.label);
                        (Status::Failed, format!("internal error while applying the result ({msg})"), Value::Null)
                    }
                },
            };
            entry.shared.set_status(status);
            let seconds = lock(&entry.shared.state).started.map(|t| t.elapsed().as_secs_f64());
            let rec = JobRecord { id, kind: entry.kind.to_string(), label: entry.label, status, message, result, seconds };
            self.history.push_back(rec.clone());
            if self.history.len() > HISTORY {
                self.history.pop_front();
            }
            out.push(rec);
        }
        out
    }

    /// Ask a job to stop. A queued job never starts; a running one stops when it next checks; a
    /// finished one is not applied. Returns `false` when there is no such unfinished job.
    pub fn cancel(&mut self, id: JobId) -> bool {
        let Some(e) = self.entries.iter().find(|e| e.id == id) else { return false };
        e.shared.cancel.cancel();
        let was_queued = {
            let mut s = lock(&e.shared.state);
            let q = s.status == Status::Queued;
            if q {
                s.status = Status::Ready;
            }
            q
        };
        if was_queued {
            self.inbox.push(id, Outcome::Cancelled);
        }
        e.shared.wake();
        true
    }

    /// Cancel every unfinished job.
    pub fn cancel_all(&mut self) {
        let ids: Vec<JobId> = self.entries.iter().map(|e| e.id).collect();
        for id in ids {
            self.cancel(id);
        }
    }

    /// Block until every job has finished on its worker (not yet applied: call
    /// [`Jobs::drain_ready`] next) or `timeout` passes. Returns whether all finished. For CLIs,
    /// scripts and tests; never call it on a UI thread.
    pub fn wait_all(&mut self, timeout: Duration) -> bool {
        let deadline = clock::now().map(|t| (t, timeout));
        let mut budget: u64 = 10_000_000;
        while !self.stepped.is_empty() && budget > 0 {
            self.drive(1);
            budget -= 1;
            if deadline.is_some_and(|(t, d)| t.elapsed() >= d) {
                break;
            }
        }
        let mut items = lock(&self.inbox.items);
        loop {
            let pending = self.entries.iter().any(|e| matches!(e.shared.status(), Status::Queued | Status::Running));
            if !pending {
                return true;
            }
            let left = match deadline {
                Some((t, d)) => d.saturating_sub(t.elapsed()),
                None => return false,
            };
            if left.is_zero() {
                return false;
            }
            items = self.inbox.cv.wait_timeout(items, left.min(Duration::from_millis(50))).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Unfinished jobs (queued, running, waiting to be applied), oldest first.
    pub fn list(&self) -> Vec<JobInfo> {
        self.entries
            .iter()
            .map(|e| {
                let s = lock(&e.shared.state);
                let elapsed = s.started.map(|t| t.elapsed().as_secs_f64());
                let eta = match (elapsed, s.fraction) {
                    (Some(el), Some(f)) if f > 0.0 && f < 1.0 && el > 0.0 => Some(el * (1.0 - f) / f),
                    (_, Some(f)) if f >= 1.0 => Some(0.0),
                    _ => None,
                };
                JobInfo {
                    id: e.id,
                    kind: e.kind.to_string(),
                    label: e.label.clone(),
                    status: s.status,
                    fraction: s.fraction,
                    message: s.message.clone(),
                    elapsed_secs: elapsed,
                    eta_secs: eta,
                    cancellable: matches!(s.status, Status::Queued | Status::Running) && !e.shared.cancel.is_cancelled(),
                }
            })
            .collect()
    }

    /// The last finished jobs, oldest first.
    pub fn recent(&self) -> impl DoubleEndedIterator<Item = &JobRecord> {
        self.history.iter()
    }

    /// `{"active": [JobInfo…], "recent": [JobRecord…]}` for agents and the CLI.
    pub fn to_json(&self) -> Value {
        json!({"active": self.list(), "recent": self.history})
    }
}

impl<S: 'static> Drop for Jobs<S> {
    fn drop(&mut self) {
        for e in &self.entries {
            e.shared.cancel.cancel();
        }
    }
}
