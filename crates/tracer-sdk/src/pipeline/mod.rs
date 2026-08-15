//! Queue, spool, session, consent and sending. Implements [`Sink`] for the signal modules.
//!
//! Host threads only ever do an atomic store, a short lock or a `try_send`. One worker thread
//! (`tracer-worker`, in `sender`) drains the queue into the spool, keeps the session's time, ticks the
//! signal modules and is the only thread that touches the network.

mod consent;
mod files;
mod identity;
mod inspect;
mod sender;
mod session;
mod spool;
mod usage;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracer_proto::{
    AppInfo, Catalog, ConsentCategory, Crash, CrashKind, Item, ItemBody, SCHEMA_VERSION, SessionEnd, SessionStart, SessionStatus, SessionTotals,
    Signal, UnixMs,
};

use self::consent::Consent;
use self::files::{lock, lock_soft, now_ms, read_json, write_json};
use self::identity::{Identity, new_id};
use self::inspect::Outbox;
use self::sender::{Message, Worker};
use self::session::{Marker, SELECTION_FILE, SESSION_FILE, Selection, SessionClock, catalog_hash};
use self::spool::{DROP_IO, Header, Spool, category_code};
use self::usage::{DROP_PROP, Listening, build_event};
use crate::config::Config;
use crate::signals::{self, Signals};
use crate::sink::Sink;
use crate::transport::Transport;

/// Folder the SDK keeps its files in, inside the host's data directory.
const SDK_DIR: &str = "tracer";
/// File the signal modules keep recent log lines in. Named here only so `forget` can delete it.
const BREADCRUMBS_FILE: &str = "breadcrumbs.jsonl";

const DROP_QUEUE_FULL: &str = "queue_full";
const DROP_BUSY: &str = "spool_busy";

/// What the SDK threw away since the last report, by reason.
#[derive(Debug, Default)]
pub(crate) struct DropCounts(Mutex<BTreeMap<String, u64>>);

impl DropCounts {
    fn add(&self, reason: &str, count: u64) {
        if count > 0 {
            let mut counts = lock(&self.0);
            match counts.get_mut(reason) {
                Some(total) => *total = total.saturating_add(count),
                None => {
                    counts.insert(reason.to_owned(), count);
                }
            }
        }
    }

    fn merge(&self, other: BTreeMap<String, u64>) {
        for (reason, count) in other {
            self.add(&reason, count);
        }
    }

    /// Returns the counts and resets them.
    fn take(&self) -> BTreeMap<String, u64> {
        std::mem::take(&mut *lock(&self.0))
    }
}

/// The spool together with the header new items are stored under. One lock for both keeps an item and
/// the header it is filed under consistent across `forget`.
struct Disk {
    spool: Spool,
    header: Header,
}

/// Requests from host threads to the worker, and its answers.
#[derive(Debug, Default)]
struct Control {
    flush_requested: u64,
    flush_done: u64,
    /// Whether the last flush left nothing to send.
    flush_clean: bool,
    /// Category bytes whose stored lines must be deleted.
    purge: Vec<u8>,
    /// Something needs sending without waiting for the flush interval.
    kick: bool,
    stop: bool,
    stopped: bool,
}

/// Intervals and limits copied from the config.
#[derive(Debug, Clone, Copy)]
struct Timing {
    tick: Duration,
    flush: Duration,
    heartbeat: Duration,
    idle_after: Duration,
    retry_min: Duration,
    retry_max: Duration,
    shutdown: Duration,
    batch_items: u64,
}

/// State shared by host threads and the worker.
struct Shared {
    dir: PathBuf,
    app_key: String,
    app_version: String,
    timing: Timing,
    /// Nothing is recorded: kill switch, unusable folder, or the worker could not be started.
    off: AtomicBool,
    /// The session has ended.
    closed: AtomicBool,
    /// Bit set of the signals the host enabled.
    enabled: u8,
    consent: Consent,
    identity: Mutex<Identity>,
    session_id: String,
    started: Instant,
    started_ms: UnixMs,
    disk: Mutex<Disk>,
    drops: DropCounts,
    clock: Mutex<SessionClock>,
    /// Serialises writes of `session.json`; true once the marker has been removed for good.
    marker_removed: Mutex<bool>,
    focused: AtomicBool,
    /// Milliseconds from `started` to the last input, plus one. Zero means no input yet.
    last_input: AtomicU64,
    panicked: AtomicBool,
    errors: AtomicU32,
    listening: Listening,
    /// Raised by `forget`. Items queued under an earlier value are discarded.
    epoch: AtomicU64,
    /// Items stored since the last successful send.
    unsent: AtomicU64,
    control: Mutex<Control>,
    control_changed: Condvar,
    signals: Mutex<Option<Arc<Signals>>>,
    selection: Mutex<Selection>,
    catalog_hash: Option<String>,
}

impl Shared {
    /// True when the host enabled the signal and the end user allows its category.
    fn permitted(&self, signal: Signal) -> bool {
        !self.off.load(Ordering::Relaxed) && self.enabled & signal_bit(signal) != 0 && self.consent.get(signal.consent())
    }

    /// Whether stored lines with this category byte may still be kept and sent.
    fn category_allowed(&self, category: u8) -> bool {
        [ConsentCategory::Crash, ConsentCategory::Performance, ConsentCategory::Usage]
            .into_iter()
            .find(|candidate| category_code(*candidate) == category)
            .map_or_else(|| self.consent.any(), |candidate| self.consent.get(candidate))
    }

    fn item(&self, body: ItemBody) -> Item {
        Item { id: new_id(), ts: now_ms(), body }
    }

    /// Writes one item to the spool on the calling thread: under `header`, or under the current session.
    /// With `soft`, gives up rather than wait for a lock that may never be released.
    fn store(&self, header: Option<&Header>, category: u8, item: &Item, soft: bool) -> bool {
        let Ok(json) = serde_json::to_string(item) else {
            self.drops.add(DROP_IO, 1);
            return false;
        };
        let guard = if soft { lock_soft(&self.disk) } else { Some(lock(&self.disk)) };
        let Some(mut disk) = guard else {
            self.drops.add(DROP_BUSY, 1);
            return false;
        };
        let Disk { spool, header: current } = &mut *disk;
        let stored = spool.append(header.unwrap_or(current), category, &json, &self.drops);
        if stored {
            self.unsent.fetch_add(1, Ordering::Relaxed);
        }
        stored
    }

    /// Credits the time since the last sample to the session and returns the running totals.
    fn sample(&self, now: Instant) -> SessionTotals {
        let focused = self.focused.load(Ordering::Relaxed);
        let last_input = self.last_input.load(Ordering::Relaxed);
        let since_start = u64::try_from(now.saturating_duration_since(self.started).as_millis()).unwrap_or(u64::MAX);
        let idle_after = u64::try_from(self.timing.idle_after.as_millis()).unwrap_or(u64::MAX);
        let active = last_input != 0 && since_start.saturating_sub(last_input - 1) <= idle_after;
        let mut clock = lock(&self.clock);
        clock.sample(now, focused, active);
        clock.totals()
    }

    /// Rewrites `session.json` with the current totals. Does nothing once the session has ended.
    fn write_marker(&self, soft: bool) {
        let removed = if soft { lock_soft(&self.marker_removed) } else { Some(lock(&self.marker_removed)) };
        let Some(removed) = removed else { return };
        if *removed || self.off.load(Ordering::Relaxed) {
            return;
        }
        let disk = if soft { lock_soft(&self.disk) } else { Some(lock(&self.disk)) };
        let Some(header) = disk.map(|disk| disk.header.clone()) else { return };
        let clock = if soft { lock_soft(&self.clock) } else { Some(lock(&self.clock)) };
        let marker = Marker {
            header,
            started: self.started_ms,
            updated: now_ms(),
            totals: clock.map(|clock| clock.totals()).unwrap_or_default(),
            errors: self.errors.load(Ordering::Relaxed),
            panicked: self.panicked.load(Ordering::Relaxed),
        };
        write_json(&self.dir.join(SESSION_FILE), &marker);
    }

    /// Reports a previous run that left its marker behind: a `session_end`, and unless it had already
    /// reported a panic, an `unclean_exit` crash. Both go under the previous session's header.
    fn recover_previous_session(&self) {
        let Some(previous) = read_json::<Marker>(&self.dir.join(SESSION_FILE)) else { return };
        let breadcrumbs = signals::take_saved_breadcrumbs(&self.dir);
        let ts = if previous.updated == 0 { previous.started } else { previous.updated };
        if self.permitted(Signal::Session) {
            let status = if previous.panicked { SessionStatus::Crashed } else { SessionStatus::Abnormal };
            let end = SessionEnd { status, totals: previous.totals, errors: previous.errors };
            let item = Item { id: new_id(), ts, body: ItemBody::SessionEnd(end) };
            self.store(Some(&previous.header), category_code(Signal::Session.consent()), &item, false);
        }
        if !previous.panicked && self.permitted(Signal::Crash) {
            let crash = Crash { kind: CrashKind::UncleanExit, fatal: true, breadcrumbs, ..Crash::default() };
            let item = Item { id: new_id(), ts, body: ItemBody::Crash(crash) };
            self.store(Some(&previous.header), category_code(Signal::Crash.consent()), &item, false);
        }
    }

    /// Wakes anything waiting on the control state. The lock is taken so a waiter cannot miss it.
    fn notify(&self, change: impl FnOnce(&mut Control)) {
        change(&mut lock(&self.control));
        self.control_changed.notify_all();
    }
}

fn signal_bit(signal: Signal) -> u8 {
    match signal {
        Signal::Session => 1,
        Signal::Usage => 2,
        Signal::Crash => 4,
        Signal::Hang => 8,
        Signal::Perf => 16,
        Signal::Sys => 32,
        Signal::Logs => 64,
    }
}

pub(crate) struct Pipeline {
    dir: PathBuf,
    shared: Arc<Shared>,
    queue: SyncSender<Message>,
    worker: Mutex<Option<JoinHandle<()>>>,
    /// The receiving end of the queue when no worker took it. Kept so the queue stays a queue that
    /// fills up, rather than one that reports its reader gone.
    _parked: Mutex<Option<Receiver<Message>>>,
}

impl Pipeline {
    /// Starts the worker thread and opens the session. `transport` overrides the built-in HTTP client.
    pub(crate) fn start(config: &Config, transport: Option<Box<dyn Transport>>) -> Arc<Self> {
        Self::start_with(config, transport, consent::kill_switch(), true)
    }

    /// `killed` is the state of the kill switches. Without `spawn_worker` nothing drains the queue, which
    /// lets tests look at the calling-thread paths on their own.
    fn start_with(config: &Config, transport: Option<Box<dyn Transport>>, killed: bool, spawn_worker: bool) -> Arc<Self> {
        let dir = config.data_dir.join(SDK_DIR);
        let usable = !killed && std::fs::create_dir_all(&dir).is_ok();
        let identity = if usable { Identity::load_or_create(&dir) } else { Identity::load(&dir).unwrap_or_default() };
        let now = Instant::now();
        let started_ms = now_ms();
        let session_id = new_id();
        let header = Header {
            schema: SCHEMA_VERSION,
            app: AppInfo { name: config.app_name.clone(), version: config.app_version.clone(), build: config.build.clone(), process: config.process },
            install_id: identity.install_id.clone(),
            session_id: session_id.clone(),
            device: usable.then(|| signals::device_info(config)),
            created: started_ms,
        };
        let outbox = (usable && config.inspect).then(|| Outbox::open(&dir));
        let mut spool = Spool::new(&dir, config.spool_max_bytes, config.spool_max_age, outbox);
        let listening = Listening::new(config);
        let hash = catalog_hash(&config.catalog);
        let mut selection = Selection::default();
        let mut carry_catalog = false;
        if usable {
            spool.scan();
            let saved = read_json::<Selection>(&dir.join(SELECTION_FILE)).unwrap_or_default();
            carry_catalog = hash.as_ref().is_some_and(|hash| !saved.is_acknowledged(&config.app_version, hash));
            let mut events = config.listen_events.clone();
            events.sort();
            events.dedup();
            selection = Selection {
                catalog_hash: hash.clone(),
                categories: listening.categories(),
                events,
                signals: enabled_names(&config.signals),
                acknowledged: saved.acknowledged.clone(),
            };
            if selection != saved {
                write_json(&dir.join(SELECTION_FILE), &selection);
            }
        }
        let transport = if usable && config.endpoint.is_some() { transport.or_else(|| built_in_transport(config)) } else { None };
        let (queue, receiver) = sync_channel(config.queue_capacity.max(1));
        let shared = Arc::new(Shared {
            dir: dir.clone(),
            app_key: config.app_key.clone(),
            app_version: config.app_version.clone(),
            timing: Timing {
                tick: config.tick_interval.max(Duration::from_millis(1)),
                flush: config.flush_interval,
                heartbeat: config.heartbeat_interval,
                idle_after: config.idle_after,
                retry_min: config.retry_min.max(Duration::from_millis(1)),
                retry_max: config.retry_max.max(config.retry_min).max(Duration::from_millis(1)),
                shutdown: config.shutdown_timeout,
                batch_items: u64::try_from(config.batch_items.max(1)).unwrap_or(u64::MAX),
            },
            off: AtomicBool::new(!usable),
            closed: AtomicBool::new(false),
            enabled: config.signals.iter().fold(0, |bits, signal| bits | signal_bit(*signal)),
            consent: Consent::load(&dir),
            identity: Mutex::new(identity),
            session_id,
            started: now,
            started_ms,
            disk: Mutex::new(Disk { spool, header }),
            drops: DropCounts::default(),
            clock: Mutex::new(SessionClock::new(now)),
            marker_removed: Mutex::new(false),
            focused: AtomicBool::new(true),
            last_input: AtomicU64::new(0),
            panicked: AtomicBool::new(false),
            errors: AtomicU32::new(0),
            listening,
            epoch: AtomicU64::new(0),
            unsent: AtomicU64::new(0),
            control: Mutex::new(Control::default()),
            control_changed: Condvar::new(),
            signals: Mutex::new(None),
            selection: Mutex::new(selection),
            catalog_hash: hash.clone(),
        });
        let mut worker = None;
        let mut parked = Some(receiver);
        if usable {
            shared.recover_previous_session();
            shared.write_marker(false);
            let mut catalog_item = None;
            if shared.permitted(Signal::Session) {
                let mut catalog = carry_catalog.then(|| config.catalog.clone());
                if let Some(Catalog { events }) = &mut catalog {
                    events.sort();
                    events.dedup();
                }
                let start =
                    SessionStart { catalog_hash: hash, catalog, listening: shared.listening.categories(), signals: enabled_names(&config.signals) };
                let item = shared.item(ItemBody::SessionStart(start));
                if shared.store(None, category_code(Signal::Session.consent()), &item, false) && carry_catalog {
                    catalog_item = Some(item.id);
                }
            }
            if spawn_worker && let Some(receiver) = parked.take() {
                let state = Worker::new(shared.clone(), receiver, transport, catalog_item);
                match std::thread::Builder::new().name("tracer-worker".to_owned()).spawn(move || state.run()) {
                    Ok(handle) => worker = Some(handle),
                    Err(_) => shared.off.store(true, Ordering::Relaxed),
                }
            }
        }
        Arc::new(Self { dir, shared, queue, worker: Mutex::new(worker), _parked: Mutex::new(parked) })
    }

    /// Hands the pipeline the signal modules so its worker can call [`Signals::tick`] about once a second.
    pub(crate) fn attach(&self, signals: Arc<Signals>) {
        if !self.shared.off.load(Ordering::Relaxed) && !self.shared.closed.load(Ordering::Relaxed) {
            *lock(&self.shared.signals) = Some(signals);
        }
    }

    pub(crate) fn track(&self, name: &'static str, category: &'static str, dynamic: bool, props: &[(&'static str, crate::Prop)]) {
        if !self.allows(Signal::Usage) || !self.shared.listening.wants(name, category, dynamic) {
            return;
        }
        match build_event(name, category, dynamic, props) {
            Ok((event, dropped_props)) => {
                self.shared.drops.add(DROP_PROP, dropped_props);
                self.submit(Signal::Usage, ItemBody::Event(event));
            }
            Err(reason) => self.shared.drops.add(reason, 1),
        }
    }

    pub(crate) fn set_focused(&self, focused: bool) {
        self.shared.focused.store(focused, Ordering::Relaxed);
    }

    pub(crate) fn note_input(&self) {
        let since_start = u64::try_from(self.shared.started.elapsed().as_millis()).unwrap_or(u64::MAX - 1);
        self.shared.last_input.store(since_start.saturating_add(1), Ordering::Relaxed);
    }

    pub(crate) fn consent(&self, category: ConsentCategory) -> bool {
        self.shared.consent.get(category)
    }

    pub(crate) fn set_consent(&self, category: ConsentCategory, allowed: bool) {
        self.shared.consent.set(category, allowed);
        if !allowed && !self.shared.off.load(Ordering::Relaxed) {
            // From here on nothing of this category is queued, stored or sent. The worker deletes what is
            // already on disk, so the host thread does not wait on the spool.
            self.shared.notify(|control| control.purge.push(category_code(category)));
            self.wake();
        }
    }

    pub(crate) fn notice_pending(&self) -> bool {
        !self.shared.off.load(Ordering::Relaxed) && !self.shared.consent.notice_shown()
    }

    pub(crate) fn notice_shown(&self) {
        self.shared.consent.set_notice_shown();
    }

    pub(crate) fn forget(&self) {
        let shared = &self.shared;
        let mut identity = lock(&shared.identity);
        if identity.install_id.is_empty() && shared.off.load(Ordering::Relaxed) {
            // Switched off and never recorded anything: only stale local files could exist.
            lock(&shared.disk).spool.wipe();
            let _ = std::fs::remove_file(self.dir.join(BREADCRUMBS_FILE));
            return;
        }
        identity.rotate(true);
        identity.save(&self.dir);
        shared.epoch.fetch_add(1, Ordering::Relaxed);
        {
            let mut disk = lock(&shared.disk);
            disk.spool.wipe();
            disk.header.install_id = identity.install_id.clone();
        }
        drop(identity);
        let _ = std::fs::remove_file(self.dir.join(BREADCRUMBS_FILE));
        shared.drops.take();
        shared.unsent.store(0, Ordering::Relaxed);
        shared.write_marker(false);
        shared.notify(|control| control.kick = true);
        self.wake();
    }

    pub(crate) fn install_id(&self) -> String {
        lock(&self.shared.identity).install_id.clone()
    }

    pub(crate) fn session_id(&self) -> String {
        self.shared.session_id.clone()
    }

    /// Sends what is waiting and returns when done or when `timeout` passes. True when nothing is left.
    pub(crate) fn flush_blocking(&self, timeout: Duration) -> bool {
        let shared = &self.shared;
        if shared.off.load(Ordering::Relaxed) {
            return true;
        }
        let deadline = Instant::now() + timeout;
        let mut control = lock(&shared.control);
        control.flush_requested += 1;
        let wanted = control.flush_requested;
        self.wake();
        loop {
            if control.flush_done >= wanted {
                return control.flush_clean;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if control.stopped || left.is_zero() {
                return false;
            }
            control = shared.control_changed.wait_timeout(control, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Ends the session and stops the worker. Called once, from the last [`crate::Tracer`] guard.
    pub(crate) fn shutdown(&self) {
        let shared = &self.shared;
        if shared.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if !shared.off.load(Ordering::Relaxed) {
            // The end of the session is written here, on the calling thread, so that it is on disk even
            // if the worker is stuck in a slow request and the wait below times out.
            shared.sample(Instant::now());
            // The clock is held across the write so that the worker cannot slip a heartbeat in after it.
            let mut clock = lock(&shared.clock);
            let totals = clock.end();
            if shared.permitted(Signal::Session) {
                let crashed = shared.panicked.load(Ordering::Relaxed) && std::thread::panicking();
                let status = if crashed { SessionStatus::Crashed } else { SessionStatus::Exited };
                let end = SessionEnd { status, totals, errors: shared.errors.load(Ordering::Relaxed) };
                shared.store(None, category_code(Signal::Session.consent()), &shared.item(ItemBody::SessionEnd(end)), false);
            }
            drop(clock);
            let mut removed = lock(&shared.marker_removed);
            *removed = true;
            let _ = std::fs::remove_file(self.dir.join(SESSION_FILE));
        }
        let handle = lock(&self.worker).take();
        if let Some(handle) = handle {
            let deadline = Instant::now() + shared.timing.shutdown;
            let mut control = lock(&shared.control);
            control.stop = true;
            self.wake();
            while !control.stopped {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                control = shared.control_changed.wait_timeout(control, left).unwrap_or_else(PoisonError::into_inner).0;
            }
            let stopped = control.stopped;
            drop(control);
            if stopped {
                let _ = handle.join();
            }
        }
        // The signal modules hold this pipeline as their sink; letting go of them here breaks the cycle.
        *lock(&shared.signals) = None;
    }

    /// Nudges the worker out of its wait. If the queue is full it is awake already.
    fn wake(&self) {
        let _ = self.queue.try_send(Message::Wake);
    }
}

impl Sink for Pipeline {
    fn submit(&self, signal: Signal, body: ItemBody) {
        if !self.allows(signal) {
            return;
        }
        let message = Message::Item {
            epoch: self.shared.epoch.load(Ordering::Relaxed),
            category: category_code(signal.consent()),
            item: Box::new(self.shared.item(body)),
        };
        if self.queue.try_send(message).is_err() {
            self.shared.drops.add(DROP_QUEUE_FULL, 1);
        }
    }

    fn submit_now(&self, signal: Signal, body: ItemBody) {
        if self.allows(signal) {
            self.shared.store(None, category_code(signal.consent()), &self.shared.item(body), true);
        }
    }

    fn allows(&self, signal: Signal) -> bool {
        !self.shared.closed.load(Ordering::Relaxed) && self.shared.permitted(signal)
    }

    fn data_dir(&self) -> &Path {
        &self.dir
    }

    fn count_drop(&self, reason: &'static str) {
        if !self.shared.off.load(Ordering::Relaxed) {
            self.shared.drops.add(reason, 1);
        }
    }

    fn mark_panicked(&self) {
        self.shared.panicked.store(true, Ordering::Relaxed);
        self.shared.write_marker(true);
    }

    fn count_error(&self) {
        self.shared.errors.fetch_add(1, Ordering::Relaxed);
    }
}

/// Names of the enabled signals, in the order of [`Signal::ALL`].
fn enabled_names(enabled: &[Signal]) -> Vec<String> {
    Signal::ALL.iter().filter(|signal| enabled.contains(signal)).map(|signal| signal.as_str().to_owned()).collect()
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
fn built_in_transport(config: &Config) -> Option<Box<dyn Transport>> {
    let endpoint = config.endpoint.as_deref()?;
    Some(Box::new(crate::transport::HttpTransport::new(endpoint)))
}

#[cfg(not(all(feature = "http", not(target_arch = "wasm32"))))]
fn built_in_transport(_config: &Config) -> Option<Box<dyn Transport>> {
    None
}
