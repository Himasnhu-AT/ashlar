//! Queue, spool, session, consent and sending. Implements [`Sink`] for the signal modules.
//!
//! Host threads only ever do an atomic store, a short lock or a `try_send`. One worker thread
//! (`tracer-worker`, in `sender`) drains the queue into the spool, keeps the session's time, ticks the
//! signal modules and is the only thread that touches the network.
//!
//! Several processes of one host may run on the same data directory at once. Each owns one session
//! folder in the spool (see `spool`); the install record, the consent file and the selection file are
//! shared, and the worker re-reads the first two on every tick so that a choice made in one process
//! reaches the others.

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
use std::time::{Duration, Instant, SystemTime};

use tracer_proto::{
    AppInfo, Catalog, ConsentCategory, Crash, CrashKind, Item, ItemBody, SCHEMA_VERSION, SessionEnd, SessionStart, SessionStatus, SessionTotals,
    Signal, UnixMs,
};

use self::consent::Consent;
use self::files::{deadline_in, lock, lock_soft, now_ms, read_json, wait_step, write_json};
use self::identity::{Identity, derived_id, new_id, next_item_id};
use self::inspect::Outbox;
use self::sender::{Message, Worker};
use self::session::{Marker, SELECTION_FILE, SESSION_FILE, Selection, SessionClock, catalog_hash};
use self::spool::{DROP_IO, Header, MAX_LINE_BYTES, SPOOL_DIR, Spool, category_code};
use self::usage::{DROP_PROP, Listening};
use crate::config::Config;
use crate::signals::{self, Signals};
use crate::sink::{LOG_TARGET, Sink};
use crate::transport::Transport;

/// Folder the SDK keeps its files in, inside the host's data directory.
const SDK_DIR: &str = "tracer";
/// File the signal modules keep recent log lines in, inside the session folder. Named here only so
/// `forget` can delete it.
const BREADCRUMBS_FILE: &str = "breadcrumbs.jsonl";

/// Longest tick, and longest wait between retries, whatever the config says. Keeps every sum of a time
/// and an interval inside what the clock can express.
const MAX_TICK: Duration = Duration::from_secs(60 * 60);
const MAX_RETRY: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// One tick may add at most this many tick intervals to the session's time, and never less than a
/// minute's worth, so that a worker held up by a slow request loses nothing but a machine that slept does
/// not have its sleep counted.
const MAX_TICKS_PER_SAMPLE: u32 = 5;
const MIN_SAMPLE_CAP: Duration = Duration::from_secs(60);

/// Most properties of one event that are copied into the queue. More than the wire allows, so that the
/// worker still sees and counts the ones over the limit, and few enough to bound the copy.
const MAX_QUEUED_PROPS: usize = 64;

const DROP_QUEUE_FULL: &str = "queue_full";
const DROP_BUSY: &str = "spool_busy";

/// What the SDK threw away since the last report, by reason.
#[derive(Debug, Default)]
pub(crate) struct DropCounts {
    counts: Mutex<BTreeMap<String, u64>>,
    /// The one reason a host thread counts on its hot path. Kept apart so that a full queue costs the
    /// host an atomic add and never a lock the worker might be holding.
    queue_full: AtomicU64,
}

impl DropCounts {
    fn add_queue_full(&self) {
        self.queue_full.fetch_add(1, Ordering::Relaxed);
    }

    fn add(&self, reason: &str, count: u64) {
        if count > 0 {
            let mut counts = lock(&self.counts);
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
        self.add(DROP_QUEUE_FULL, self.queue_full.swap(0, Ordering::Relaxed));
        std::mem::take(&mut *lock(&self.counts))
    }
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
    queue_capacity: usize,
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
    /// The install record. Held across file reads and writes, so host threads do not wait on it; they
    /// read `install_id`.
    identity: Mutex<Identity>,
    /// A copy of the current install id that is only ever locked for a moment.
    install_id: Mutex<String>,
    /// The install record in memory has not reached the file, because saving failed. Until it has, the
    /// file is not believed, and every tick tries the save again.
    identity_unsaved: AtomicBool,
    /// The install file was written and has not been flushed to the disk yet.
    identity_unsynced: AtomicBool,
    /// Install ids this process has rotated away from. A file that still names one of them is behind.
    retired: Mutex<Vec<String>>,
    /// Nothing is sent until the user has seen the host's notice.
    hold_until_notice: bool,
    session_id: String,
    /// This process's folder in the spool: `spool/<session id>`.
    session_dir: PathBuf,
    started: Instant,
    started_ms: UnixMs,
    /// The spool, which also holds the header new items are stored under. One lock for both keeps an
    /// item and the header it is filed under consistent across `forget`.
    disk: Mutex<Spool>,
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
        Item { id: next_item_id(), ts: now_ms(), body }
    }

    /// Writes one item to this session's folder in the spool, on the calling thread. With `soft`, gives
    /// up rather than wait for a lock that may never be released.
    fn store(&self, category: u8, item: &Item, soft: bool) -> bool {
        self.store_if(category, item, soft, || true)
    }

    /// Stores an item that waited in the queue, unless a `forget` or a consent change has overtaken it.
    /// That is decided with the spool locked, the same lock `forget` wipes under, so an item queued under
    /// the old install id cannot land under the new header.
    fn store_queued(&self, epoch: u64, category: u8, item: &Item) -> bool {
        self.store_if(category, item, false, || epoch == self.epoch.load(Ordering::Relaxed) && self.category_allowed(category))
    }

    /// Stores an item without counting it as dropped if that fails. For the `drops` item only.
    fn store_uncounted(&self, category: u8, item: &Item) -> bool {
        let Ok(json) = serde_json::to_string(item) else { return false };
        let stored = lock(&self.disk).try_append(category, &json, &self.drops).is_ok();
        if stored {
            self.unsent.fetch_add(1, Ordering::Relaxed);
        }
        stored
    }

    /// True while sending waits for the user to have seen the host's notice.
    fn sending_held(&self) -> bool {
        self.hold_until_notice && !self.consent.notice_shown()
    }

    fn store_if(&self, category: u8, item: &Item, soft: bool, still_wanted: impl FnOnce() -> bool) -> bool {
        let Ok(json) = serde_json::to_string(item) else {
            self.drops.add(DROP_IO, 1);
            return false;
        };
        let guard = if soft { lock_soft(&self.disk) } else { Some(lock(&self.disk)) };
        let Some(mut spool) = guard else {
            self.drops.add(DROP_BUSY, 1);
            return false;
        };
        let stored = still_wanted() && spool.append(category, &json, &self.drops);
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
        let spool = if soft { lock_soft(&self.disk) } else { Some(lock(&self.disk)) };
        let Some(header) = spool.and_then(|spool| spool.own_header().cloned()) else { return };
        let clock = if soft { lock_soft(&self.clock) } else { Some(lock(&self.clock)) };
        let marker = Marker {
            header,
            started: self.started_ms,
            updated: now_ms(),
            totals: clock.map(|clock| clock.totals()).unwrap_or_default(),
            errors: self.errors.load(Ordering::Relaxed),
            panicked: self.panicked.load(Ordering::Relaxed),
        };
        write_json(&self.session_dir.join(SESSION_FILE), &marker);
        // The marker counts towards the spool's size.
        let spool = if soft { lock_soft(&self.disk) } else { Some(lock(&self.disk)) };
        if let Some(mut spool) = spool {
            spool.note_own_files();
        }
    }

    /// Looks after the session folders of processes that are gone: adopts them, reports the ones that
    /// left a marker, and drops data past the age limit. Runs at startup and before every send.
    ///
    /// A session that left its marker behind did not shut down cleanly. It gets a `session_end`, and
    /// unless it had already reported a panic, an `unclean_exit` crash with the log lines it saved. Both
    /// go into its own folder, under its own header. The process that adopted the folder holds its lock,
    /// so no other process reports it; and the item ids are derived from the session id, so even a
    /// report repeated after a crash at the wrong moment reaches the server as one.
    fn maintain(&self) {
        if self.off.load(Ordering::Relaxed) {
            return;
        }
        let install_id = lock(&self.identity).install_id.clone();
        let mut spool = lock(&self.disk);
        if spool.restore_own_dir() {
            // The session folder was deleted under the running session. It is back; so must the marker be.
            drop(spool);
            self.write_marker(false);
            spool = lock(&self.disk);
        }
        for unclean in spool.adopt_orphans(&install_id, &self.drops) {
            let breadcrumbs = signals::saved_breadcrumbs(&unclean.dir);
            let previous = unclean.marker;
            // The time of the last sign of life, but never later than now: a marker can say anything.
            let ts = (if previous.updated == 0 { previous.started } else { previous.updated }).min(now_ms());
            let mut items = Vec::new();
            if self.permitted(Signal::Session) {
                let status = if previous.panicked { SessionStatus::Crashed } else { SessionStatus::Abnormal };
                let end = SessionEnd { status, totals: previous.totals, errors: errors_of(previous.errors, previous.panicked) };
                items.push((Signal::Session, Item { id: derived_id(&unclean.session_id, "session_end"), ts, body: ItemBody::SessionEnd(end) }));
            }
            if !previous.panicked && self.permitted(Signal::Crash) {
                let crash = Crash { kind: CrashKind::UncleanExit, fatal: true, breadcrumbs, ..Crash::default() };
                items.push((Signal::Crash, Item { id: derived_id(&unclean.session_id, "unclean_exit"), ts, body: ItemBody::Crash(crash) }));
            }
            let mut lines = Vec::new();
            for (signal, mut item) in items {
                let mut json = serde_json::to_string(&item).unwrap_or_default();
                // Saved log lines are the only part that can be large; the report matters more than they do.
                if json.len() as u64 >= MAX_LINE_BYTES
                    && let ItemBody::Crash(crash) = &mut item.body
                {
                    crash.breadcrumbs.clear();
                    json = serde_json::to_string(&item).unwrap_or_default();
                }
                if !json.is_empty() {
                    lines.push((category_code(signal.consent()), json));
                }
            }
            if spool.add_recovery(&unclean.session_id, &lines) {
                self.unsent.fetch_add(lines.len() as u64, Ordering::Relaxed);
            }
            // Only now that the report is on disk and the marker is gone.
            let _ = std::fs::remove_file(unclean.dir.join(BREADCRUMBS_FILE));
        }
        spool.expire(SystemTime::now(), &self.drops);
    }

    /// Makes the small state files durable, if they changed since the last call. They are written by
    /// temp file and rename without waiting for the disk, because that happens on host threads; this is
    /// the worker catching up, within a tick. Until then a power loss could bring back an older consent
    /// choice or install id.
    fn sync_state_files(&self) {
        self.consent.sync();
        if self.identity_unsynced.swap(false, Ordering::Relaxed) {
            files::sync_file(&self.dir.join(identity::INSTALL_FILE));
        }
    }

    /// Takes over what other processes of the host wrote to the shared files: a consent change, or a new
    /// install id after a `forget`. Called by the worker on every tick.
    fn sync_shared_files(&self) {
        for category in self.consent.refresh() {
            lock(&self.disk).purge(category_code(category));
        }
        let mut identity = lock(&self.identity);
        if self.identity_unsaved.load(Ordering::Relaxed) {
            // Memory is ahead of the file; try again to bring the file up to it.
            self.change_identity(&mut identity, |_| {});
            return;
        }
        let Some(saved) = Identity::load(&self.dir) else {
            // The install file is gone or unreadable while the session runs. Left like that, the next
            // launch would take a new install id; put the record back.
            if !identity.install_id.is_empty() {
                self.identity_unsaved.store(true, Ordering::Relaxed);
                self.change_identity(&mut identity, |_| {});
            }
            return;
        };
        if saved != *identity {
            if lock(&self.retired).contains(&saved.install_id) {
                // The file names an id this process has already rotated away from: it is behind, and
                // must not bring the forgotten id back. Put the current record in its place.
                self.identity_unsaved.store(true, Ordering::Relaxed);
                self.change_identity(&mut identity, |_| {});
                return;
            }
            let forgotten = saved.install_id != identity.install_id;
            self.set_identity(&mut identity, saved);
            if forgotten {
                self.start_over(&identity.install_id);
            }
        }
    }

    /// Replaces the install record in memory, and the copy of the id that host threads read.
    fn set_identity(&self, identity: &mut Identity, next: Identity) {
        *lock(&self.install_id) = next.install_id.clone();
        *identity = next;
    }

    /// Applies `change` to the install record and saves it. The record in memory changes whether or not
    /// the save works; a failed save is remembered and tried again on later ticks. Returns true when the
    /// install id is now a different one. The caller holds the `identity` lock.
    fn change_identity(&self, identity: &mut Identity, change: impl FnOnce(&mut Identity)) -> bool {
        let unsaved = self.identity_unsaved.load(Ordering::Relaxed);
        let retired = lock(&self.retired).clone();
        let (next, saved) = Identity::update(&self.dir, identity, |file| !unsaved && !retired.contains(&file.install_id), change);
        self.identity_unsaved.store(!saved, Ordering::Relaxed);
        self.identity_unsynced.store(saved, Ordering::Relaxed);
        let changed = next.install_id != identity.install_id;
        self.set_identity(identity, next);
        changed
    }

    /// The local half of `forget`: deletes what this process may delete and carries on under the new
    /// install id. Items still in the queue belong to the old one and are discarded by the worker.
    fn start_over(&self, install_id: &str) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
        {
            let mut spool = lock(&self.disk);
            spool.wipe();
            spool.set_install_id(install_id);
        }
        let _ = std::fs::remove_file(self.session_dir.join(BREADCRUMBS_FILE));
        // What the signal modules keep in memory was gathered under the old id: recent log lines, the
        // last crash, the last performance window. None of it may follow into the new one.
        let signals = lock(&self.signals).clone();
        if let Some(signals) = signals {
            signals.forget();
        }
        self.drops.take();
        self.unsent.store(0, Ordering::Relaxed);
        self.write_marker(false);
    }

    /// A few `key: value` lines about this install and session, for the local report.
    fn session_summary(&self) -> String {
        let install_id = lock(&self.install_id).clone();
        let totals = lock(&self.clock).totals();
        format!(
            "install_id: {install_id}\nsession_id: {}\nstarted_ms: {}\nduration_s: {}\nfocused_s: {}\nactive_s: {}\nidle_s: {}\nerrors: {}\n",
            self.session_id,
            self.started_ms,
            totals.duration_s,
            totals.focused_s,
            totals.active_s,
            totals.idle_s,
            self.errors.load(Ordering::Relaxed)
        )
    }

    /// Wakes anything waiting on the control state. The lock is taken so a waiter cannot miss it.
    fn notify(&self, change: impl FnOnce(&mut Control)) {
        change(&mut lock(&self.control));
        self.control_changed.notify_all();
    }
}

/// The `errors` a session ends with. Every panic is counted when it happens, because nobody can know
/// then whether the process will survive it. The one that ended the session did not survive, and a
/// session that ends `crashed` already says so, so it is taken off again.
fn errors_of(counted: u32, crashed: bool) -> u32 {
    if crashed { counted.saturating_sub(1) } else { counted }
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
    session_dir: PathBuf,
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
        let mut usable = !killed && std::fs::create_dir_all(&dir).is_ok();
        let identity = if usable { Identity::load_or_create(&dir) } else { Identity::load(&dir).unwrap_or_default() };
        // If the record could not be written, it is kept in memory and the worker goes on trying.
        let identity_unsaved = usable && Identity::load(&dir).as_ref() != Some(&identity);
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
        let session_dir = dir.join(SPOOL_DIR).join(&session_id);
        // From here on this process owns its session folder, and holds its lock until it ends.
        usable = usable && spool.open_own(header);
        let listening = Listening::new(config);
        let hash = catalog_hash(&config.catalog);
        let mut selection = Selection::default();
        let mut carry_catalog = false;
        if usable {
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
                tick: config.tick_interval.clamp(Duration::from_millis(1), MAX_TICK),
                flush: config.flush_interval,
                heartbeat: config.heartbeat_interval,
                idle_after: config.idle_after,
                retry_min: config.retry_min.clamp(Duration::from_millis(1), MAX_RETRY),
                retry_max: config.retry_max.max(config.retry_min).clamp(Duration::from_millis(1), MAX_RETRY),
                shutdown: config.shutdown_timeout,
                batch_items: u64::try_from(config.batch_items.max(1)).unwrap_or(u64::MAX),
                queue_capacity: config.queue_capacity.max(1),
            },
            off: AtomicBool::new(!usable),
            closed: AtomicBool::new(false),
            enabled: config.signals.iter().fold(0, |bits, signal| bits | signal_bit(*signal)),
            consent: Consent::load(&dir),
            install_id: Mutex::new(identity.install_id.clone()),
            identity: Mutex::new(identity),
            identity_unsaved: AtomicBool::new(identity_unsaved),
            identity_unsynced: AtomicBool::new(usable),
            retired: Mutex::new(Vec::new()),
            hold_until_notice: !config.send_before_notice,
            session_id,
            session_dir: session_dir.clone(),
            started: now,
            started_ms,
            disk: Mutex::new(spool),
            drops: DropCounts::default(),
            clock: Mutex::new(SessionClock::new(now, config.tick_interval.min(MAX_TICK).saturating_mul(MAX_TICKS_PER_SAMPLE).max(MIN_SAMPLE_CAP))),
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
            if !config.listen_categories.is_empty() && config.catalog.events.is_empty() {
                log::warn!(target: LOG_TARGET, "categories are listened to but no events were declared; declared events will not be recorded");
            }
            shared.write_marker(false);
            // Looking through what earlier runs left behind can take a while when there is a lot of it,
            // and `init` is on the host's startup path: the worker does it, first thing. Only a pipeline
            // without a worker, as some tests build, does it here.
            if !spawn_worker {
                shared.maintain();
            }
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
                if shared.store(category_code(Signal::Session.consent()), &item, false) && carry_catalog {
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
        Arc::new(Self { session_dir, shared, queue, worker: Mutex::new(worker), _parked: Mutex::new(parked) })
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
        // The host thread only hands over what it was given: fixed strings and a few plain values.
        // Checking the limits, building the wire event, giving it an id and writing it out all happen on
        // the worker. Without properties this allocates nothing.
        let props = match props.split_at_checked(MAX_QUEUED_PROPS) {
            Some((kept, over)) => {
                self.shared.drops.add(DROP_PROP, over.len() as u64);
                kept
            }
            None => props,
        };
        let message = Message::Event {
            epoch: self.shared.epoch.load(Ordering::Relaxed),
            ts: now_ms(),
            name,
            category,
            dynamic,
            props: (!props.is_empty()).then(|| props.into()),
        };
        if self.queue.try_send(message).is_err() {
            self.shared.drops.add_queue_full();
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
        if self.shared.off.load(Ordering::Relaxed) {
            // Switched off for the whole process: nothing is recorded whatever the choice, and nothing
            // is written, not even the choice.
            self.shared.consent.set_in_memory(category, allowed);
            return;
        }
        self.shared.consent.set(category, allowed);
        if !allowed {
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
        if !self.shared.off.load(Ordering::Relaxed) {
            self.shared.consent.set_notice_shown();
        }
    }

    pub(crate) fn forget(&self) {
        let shared = &self.shared;
        let mut identity = lock(&shared.identity);
        if identity.install_id.is_empty() && shared.off.load(Ordering::Relaxed) {
            // Switched off and never recorded anything: only stale local files could exist.
            lock(&shared.disk).wipe();
            return;
        }
        // Other running processes of the host see the new id in the install record on their next tick,
        // and each clears its own session folder then.
        let before = identity.install_id.clone();
        shared.change_identity(&mut identity, |record| {
            record.rotate(true);
        });
        {
            // Whatever this process has turned away from stays turned away from, even if the file could
            // not be rewritten and still names it.
            let mut retired = lock(&shared.retired);
            for id in identity.pending_forget.iter().chain([&before]) {
                if !retired.contains(id) {
                    retired.push(id.clone());
                }
            }
        }
        shared.start_over(&identity.install_id);
        drop(identity);
        shared.notify(|control| control.kick = true);
        self.wake();
    }

    pub(crate) fn install_id(&self) -> String {
        lock(&self.shared.install_id).clone()
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
        // A timeout too long to add to the clock is no deadline at all.
        let deadline = deadline_in(timeout);
        let mut control = lock(&shared.control);
        control.flush_requested += 1;
        let wanted = control.flush_requested;
        self.wake();
        loop {
            if control.flush_done >= wanted {
                return control.flush_clean;
            }
            let Some(step) = wait_step(deadline).filter(|_| !control.stopped) else { return false };
            control = shared.control_changed.wait_timeout(control, step).unwrap_or_else(PoisonError::into_inner).0;
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
                let end = SessionEnd { status, totals, errors: errors_of(shared.errors.load(Ordering::Relaxed), crashed) };
                shared.store(category_code(Signal::Session.consent()), &shared.item(ItemBody::SessionEnd(end)), false);
            }
            drop(clock);
            let mut removed = lock(&shared.marker_removed);
            *removed = true;
            let _ = std::fs::remove_file(self.session_dir.join(SESSION_FILE));
        }
        let handle = lock(&self.worker).take();
        if let Some(handle) = handle {
            let deadline = deadline_in(shared.timing.shutdown);
            let mut control = lock(&shared.control);
            control.stop = true;
            self.wake();
            while !control.stopped {
                let Some(step) = wait_step(deadline) else { break };
                control = shared.control_changed.wait_timeout(control, step).unwrap_or_else(PoisonError::into_inner).0;
            }
            let stopped = control.stopped;
            drop(control);
            if stopped {
                let _ = handle.join();
            }
        }
        // The signal modules hold this pipeline as their sink; letting go of them here breaks the cycle.
        *lock(&shared.signals) = None;
        // Give up the session folder. What could not be sent stays in it, without a marker, and the next
        // process to look sends it.
        lock(&shared.disk).release();
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
            ts: now_ms(),
            category: category_code(signal.consent()),
            body: Box::new(body),
        };
        if self.queue.try_send(message).is_err() {
            self.shared.drops.add_queue_full();
        }
    }

    fn submit_now(&self, signal: Signal, body: ItemBody) {
        if self.allows(signal) {
            self.shared.store(category_code(signal.consent()), &self.shared.item(body), true);
        }
    }

    fn allows(&self, signal: Signal) -> bool {
        !self.shared.closed.load(Ordering::Relaxed) && self.shared.permitted(signal)
    }

    fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    fn session_summary(&self) -> String {
        self.shared.session_summary()
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
