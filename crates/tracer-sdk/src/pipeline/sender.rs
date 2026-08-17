//! The worker thread: drains the queue into the spool, keeps the session's time, ticks the signal
//! modules, and sends batches.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde::Serialize;
use tracer_proto::{
    AppInfo, Device, Drops, ForgetRequest, IngestResponse, Item, ItemBody, MAX_ENVELOPE_BYTES, SCHEMA_VERSION, Signal, UnixMs, routes,
};

use super::Shared;
use super::files::{lock, now_ms, write_json};
use super::identity::random_u64;
use super::session::SELECTION_FILE;
use super::spool::{Batch, CAT_META, Header, category_code};
use crate::sink::LOG_TARGET;
use crate::transport::{Request, Transport};

const DROP_REJECTED: &str = "server_rejected";
const DROP_TICK: &str = "tick_panicked";
/// A forget request the server refused in a way that trying again cannot change.
const DROP_FORGET: &str = "forget_rejected";

/// Room left in an envelope for its header, on top of the items.
const HEADER_ALLOWANCE: u64 = 16 * 1024;

/// Longest `Retry-After` the worker will wait out.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Most queued messages handled before the worker looks at its timers again.
const DRAIN_STEP: usize = 256;

pub(super) enum Message {
    Item {
        /// The value of the forget counter when the item was queued.
        epoch: u64,
        category: u8,
        /// Boxed so that a queue slot stays small.
        item: Box<Item>,
    },
    /// Nothing to store; the control state changed.
    Wake,
}

/// What the server made of one request.
enum Outcome {
    /// Stored. Carries the reply body.
    Delivered(Vec<u8>),
    /// Refused for good; sending it again cannot help.
    Rejected,
    /// The key is not accepted. Nothing more is sent until restart.
    Halted,
    /// Not delivered this time.
    Retry,
}

pub(super) struct Worker {
    shared: Arc<Shared>,
    queue: Receiver<Message>,
    transport: Option<Box<dyn Transport>>,
    /// Id of this session's `session_start` while it carries a catalog the server has not acknowledged.
    catalog_item: Option<String>,
    halted: bool,
    failures: u32,
    /// Earliest time of the next attempt after a failure. An explicit flush may go earlier.
    backoff_until: Option<Instant>,
    /// Earliest time the server allows the next attempt. Nothing goes earlier.
    retry_after_until: Option<Instant>,
    /// Forget requests are best effort and keep their own count and clock, so that one that keeps failing
    /// neither holds up ingest nor is hurried along by it.
    forget_failures: u32,
    forget_retry_at: Option<Instant>,
    last_cycle: Instant,
    last_upkeep: Instant,
    last_heartbeat: Instant,
    want_send: bool,
    flush_done: u64,
    /// The host has been told once that sending waits for `notice_shown`.
    warned_held: bool,
}

impl Worker {
    pub(super) fn new(shared: Arc<Shared>, queue: Receiver<Message>, transport: Option<Box<dyn Transport>>, catalog_item: Option<String>) -> Self {
        let now = Instant::now();
        Self {
            shared,
            queue,
            transport,
            catalog_item,
            halted: false,
            failures: 0,
            backoff_until: None,
            retry_after_until: None,
            forget_failures: 0,
            forget_retry_at: None,
            warned_held: false,
            last_cycle: now,
            last_upkeep: now,
            last_heartbeat: now,
            // Once at startup, for anything left from earlier runs.
            want_send: true,
            flush_done: 0,
        }
    }

    pub(super) fn run(mut self) {
        // `timing.tick` is clamped when it is built, so adding it to the clock cannot overflow.
        let mut next_tick = Instant::now() + self.shared.timing.tick;
        loop {
            let (flush_requested, stop, purge) = {
                let mut control = lock(&self.shared.control);
                self.want_send |= std::mem::take(&mut control.kick);
                (control.flush_requested, control.stop, std::mem::take(&mut control.purge))
            };
            if !purge.is_empty() {
                let mut spool = lock(&self.shared.disk);
                for category in purge {
                    spool.purge(category);
                }
            }
            let now = Instant::now();
            if now >= next_tick && !stop {
                self.tick(now);
                next_tick = now + self.shared.timing.tick;
            }
            if stop || flush_requested > self.flush_done {
                self.drain_queue();
                self.note_held();
                self.send_cycle(true);
                let clean = self.nothing_left();
                self.flush_done = flush_requested;
                self.shared.notify(|control| {
                    control.flush_done = flush_requested;
                    control.flush_clean = clean;
                });
                if stop {
                    break;
                }
            } else if self.send_due(now) {
                self.send_cycle(false);
            } else if now.saturating_duration_since(self.last_upkeep) >= self.shared.timing.flush {
                // Nothing to send, or no way to send it: orphans are still adopted and the caps kept.
                self.upkeep(now);
                self.note_held();
            }
            match self.queue.recv_timeout(next_tick.saturating_duration_since(Instant::now())) {
                Ok(message) => {
                    self.handle(message);
                    for _ in 0..DRAIN_STEP {
                        match self.queue.try_recv() {
                            Ok(message) => self.handle(message),
                            Err(_) => break,
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // Every handle is gone without a shutdown; there is nobody left to work for.
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        *lock(&self.shared.signals) = None;
        self.shared.notify(|control| control.stopped = true);
    }

    fn handle(&mut self, message: Message) {
        if let Message::Item { epoch, category, item } = message {
            self.shared.store_queued(epoch, category, &item);
        }
    }

    /// Tells the host, once, why nothing is arriving: data is waiting and sending is held until the user
    /// has seen the notice. A host that forgot to call `notice_shown` finds out from its log.
    fn note_held(&mut self) {
        if !self.warned_held && self.transport.is_some() && self.shared.sending_held() && self.has_work() {
            self.warned_held = true;
            log::warn!(
                target: LOG_TARGET,
                "telemetry is recorded but not sent until Tracer::notice_shown() has been called; see Builder::send_before_notice"
            );
        }
    }

    fn upkeep(&mut self, now: Instant) {
        self.last_upkeep = now;
        self.shared.maintain();
    }

    /// Stores everything that is queued right now.
    fn drain_queue(&mut self) {
        while let Ok(message) = self.queue.try_recv() {
            self.handle(message);
        }
    }

    fn tick(&mut self, now: Instant) {
        let shared = self.shared.clone();
        shared.sync_shared_files();
        shared.sample(now);
        if now.saturating_duration_since(self.last_heartbeat) >= shared.timing.heartbeat {
            self.last_heartbeat = now;
            // Stored while the clock is held, as the session end is, so the two cannot change places.
            let mut clock = lock(&shared.clock);
            if let Some(totals) = clock.next_heartbeat()
                && shared.permitted(Signal::Session)
            {
                shared.store(category_code(Signal::Session.consent()), &shared.item(ItemBody::SessionHeartbeat(totals)), false);
            }
            drop(clock);
            shared.write_marker(false);
        }
        let signals = lock(&shared.signals).clone();
        // A fault in a signal module must not end the worker.
        if let Some(signals) = signals
            && catch_unwind(AssertUnwindSafe(|| signals.tick())).is_err()
        {
            shared.drops.add(DROP_TICK, 1);
        }
    }

    fn can_send(&self) -> bool {
        self.transport.is_some() && !self.halted && !self.shared.off.load(Ordering::Relaxed) && !self.shared.sending_held()
    }

    fn has_work(&self) -> bool {
        // Two statements on purpose: each lock is let go before the next is taken. Held together they
        // would be taken in the opposite order to `forget`, and the two threads could wait on each other.
        let spooled = !lock(&self.shared.disk).is_empty();
        let forgets = !lock(&self.shared.identity).pending_forget.is_empty();
        spooled || forgets
    }

    /// True when a flush has nothing more it could send.
    fn nothing_left(&self) -> bool {
        self.transport.is_none() || self.shared.off.load(Ordering::Relaxed) || !self.has_work()
    }

    fn send_due(&self, now: Instant) -> bool {
        if !self.can_send() {
            return false;
        }
        self.want_send
            || self.shared.unsent.load(Ordering::Relaxed) >= self.shared.timing.batch_items
            || (now.saturating_duration_since(self.last_cycle) >= self.shared.timing.flush && self.has_work())
    }

    /// Sends everything that is waiting, oldest session first, until done or until a request fails.
    /// `forced` is an explicit flush, which does not wait out the backoff after a failure.
    fn send_cycle(&mut self, forced: bool) {
        if !self.can_send() {
            return;
        }
        let now = Instant::now();
        if self.retry_after_until.is_some_and(|until| now < until) || (!forced && self.backoff_until.is_some_and(|until| now < until)) {
            return;
        }
        self.last_cycle = now;
        self.want_send = true;
        self.upkeep(now);
        if !self.send_forget_requests(forced) {
            return;
        }
        self.store_drop_counts();
        let shared = self.shared.clone();
        let budget = (MAX_ENVELOPE_BYTES as u64).saturating_sub(HEADER_ALLOWANCE);
        loop {
            let batch = lock(&shared.disk).next_batch(budget, &|category| shared.category_allowed(category));
            let Some(batch) = batch else { break };
            if batch.items.is_empty() {
                lock(&shared.disk).complete(&batch, &shared.drops);
                continue;
            }
            let Some(body) = envelope_json(&batch.header, now_ms(), &batch.items) else {
                lock(&shared.disk).complete(&batch, &shared.drops);
                shared.drops.add(DROP_REJECTED, batch.items.len() as u64);
                continue;
            };
            let outcome = match gzip(&body) {
                Some(compressed) => self.post(routes::INGEST, &compressed, true),
                None => self.post(routes::INGEST, &body, false),
            };
            match outcome {
                Outcome::Delivered(reply) => {
                    lock(&shared.disk).complete(&batch, &shared.drops);
                    self.note_catalog(&batch, &reply);
                }
                Outcome::Rejected => {
                    lock(&shared.disk).complete(&batch, &shared.drops);
                    shared.drops.add(DROP_REJECTED, batch.items.len() as u64);
                }
                Outcome::Halted | Outcome::Retry => return,
            }
        }
        self.want_send = false;
        shared.unsent.store(0, Ordering::Relaxed);
    }

    /// Asks the server to delete earlier install ids. Best effort: a request that fails is tried again
    /// later, with its own backoff, here and after the next launch, and never holds up ingest. One the
    /// server refuses for good (a 4xx other than 401, 403 and 429) is given up and counted. False only
    /// when the server does not accept the key, which stops all sending.
    fn send_forget_requests(&mut self, forced: bool) -> bool {
        if !forced && self.forget_retry_at.is_some_and(|at| Instant::now() < at) {
            return true;
        }
        let pending = lock(&self.shared.identity).pending_forget.clone();
        for install_id in pending {
            let Ok(body) = serde_json::to_vec(&ForgetRequest { schema: SCHEMA_VERSION, install_id: install_id.clone() }) else { continue };
            let Some(transport) = self.transport.as_mut() else { return true };
            let reply = transport.send(&Request { path: routes::FORGET, app_key: &self.shared.app_key, body: &body, gzip: false });
            match reply.as_ref().map(|reply| reply.status) {
                Ok(200..=299) => {
                    self.forget_failures = 0;
                    self.forget_retry_at = None;
                    self.settle_forget(&install_id);
                }
                Ok(401 | 403) => {
                    self.halted = true;
                    return false;
                }
                Ok(status @ 400..=499) if status != 429 => {
                    self.shared.drops.add(DROP_FORGET, 1);
                    self.settle_forget(&install_id);
                }
                _ => {
                    let wait = self.backoff_wait(self.forget_failures);
                    let asked = reply.ok().and_then(|reply| reply.retry_after).map_or(wait, |asked| asked.clamp(wait, MAX_RETRY_AFTER));
                    self.forget_failures = self.forget_failures.saturating_add(1);
                    self.forget_retry_at = Instant::now().checked_add(asked);
                    break;
                }
            }
        }
        true
    }

    /// Takes an install id off the list of forgets still owed.
    fn settle_forget(&mut self, install_id: &str) {
        let shared = &self.shared;
        let mut identity = lock(&shared.identity);
        // The record is merged with what other processes wrote. If that shows a newer install id, this
        // process starts over under it, exactly as it would have on its next tick.
        if shared.change_identity(&mut identity, |record| record.pending_forget.retain(|pending| pending != install_id)) {
            shared.start_over(&identity.install_id);
        }
    }

    /// Turns the drop counts into a `drops` item in the spool, so it leaves with the next batch and is
    /// not lost if that batch has to be retried.
    fn store_drop_counts(&mut self) {
        if !self.shared.consent.any() {
            return;
        }
        let reasons = self.shared.drops.take();
        if !reasons.is_empty() {
            let item = self.shared.item(ItemBody::Drops(Drops { reasons: reasons.clone() }));
            if !self.shared.store(CAT_META, &item, false) {
                self.shared.drops.merge(reasons);
            }
        }
    }

    fn post(&mut self, path: &str, body: &[u8], gzip: bool) -> Outcome {
        let Some(transport) = self.transport.as_mut() else { return Outcome::Retry };
        let result = transport.send(&Request { path, app_key: &self.shared.app_key, body, gzip });
        let now = Instant::now();
        match result {
            Ok(reply) => match reply.status {
                200..=299 => {
                    self.failures = 0;
                    self.backoff_until = None;
                    Outcome::Delivered(reply.body)
                }
                400 | 413 | 422 => Outcome::Rejected,
                401 | 403 => {
                    self.halted = true;
                    Outcome::Halted
                }
                429 if reply.retry_after.is_some() => {
                    // Never less than the shortest retry wait: a server that says "retry after 0" would
                    // otherwise be asked again on every tick.
                    let floor = self.shared.timing.retry_min.min(MAX_RETRY_AFTER);
                    self.retry_after_until = reply.retry_after.and_then(|wait| now.checked_add(wait.clamp(floor, MAX_RETRY_AFTER)));
                    Outcome::Retry
                }
                _ => self.back_off(now),
            },
            Err(_) => self.back_off(now),
        }
    }

    /// Exponential backoff with jitter: the wait doubles from `retry_min` to `retry_max`, and the actual
    /// wait is a random point in its upper half.
    fn back_off(&mut self, now: Instant) -> Outcome {
        let wait = self.backoff_wait(self.failures);
        self.failures = self.failures.saturating_add(1);
        self.backoff_until = now.checked_add(wait);
        Outcome::Retry
    }

    /// The wait after the given number of failures in a row.
    fn backoff_wait(&self, failures: u32) -> Duration {
        let timing = self.shared.timing;
        let ceiling = timing.retry_min.saturating_mul(1u32 << failures.min(20)).min(timing.retry_max);
        jitter(ceiling, random_u64())
    }

    /// Keeps the catalog acknowledgement in step with what the server said.
    fn note_catalog(&mut self, batch: &Batch, reply: &[u8]) {
        let shared = &self.shared;
        // Only a reply that can be read says anything about the catalog. An empty or foreign body, from a
        // proxy for instance, must not be taken for "the server has it".
        let Ok(IngestResponse { need_catalog, .. }) = serde_json::from_slice::<IngestResponse>(reply) else { return };
        let mut selection = lock(&shared.selection);
        let changed = if need_catalog {
            self.catalog_item = None;
            selection.clear_acknowledged(&shared.app_version)
        } else if let (Some(id), Some(hash)) = (&self.catalog_item, &shared.catalog_hash)
            && batch.header.session_id == shared.session_id
            && batch.items.iter().any(|json| json.contains(id.as_str()))
        {
            self.catalog_item = None;
            selection.acknowledge(&shared.app_version, hash)
        } else {
            false
        };
        if changed {
            write_json(&shared.dir.join(SELECTION_FILE), &*selection);
        }
    }
}

/// A wait between half of `ceiling` and all of it.
fn jitter(ceiling: Duration, random: u64) -> Duration {
    let half = ceiling / 2;
    let spread = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
    half + Duration::from_nanos(random % spread.saturating_add(1))
}

/// The envelope fields that come before `items`.
#[derive(Serialize)]
struct Head<'a> {
    schema: u32,
    app: &'a AppInfo,
    install_id: &'a str,
    session_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<&'a Device>,
    sent_at: UnixMs,
}

/// Assembles an envelope from a header and items that are already JSON, without parsing them again.
fn envelope_json(header: &Header, sent_at: UnixMs, items: &[String]) -> Option<Vec<u8>> {
    let head = Head {
        schema: header.schema,
        app: &header.app,
        install_id: &header.install_id,
        session_id: &header.session_id,
        device: header.device.as_ref(),
        sent_at,
    };
    let mut body = serde_json::to_vec(&head).ok()?;
    if body.pop() != Some(b'}') {
        return None;
    }
    body.reserve(items.iter().map(|item| item.len() + 1).sum::<usize>() + 16);
    body.extend_from_slice(b",\"items\":[");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            body.push(b',');
        }
        body.extend_from_slice(item.as_bytes());
    }
    body.extend_from_slice(b"]}");
    Some(body)
}

#[cfg(not(target_arch = "wasm32"))]
fn gzip(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;

    let mut encoder = flate2::write::GzEncoder::new(Vec::with_capacity(bytes.len() / 4 + 64), flate2::Compression::default());
    encoder.write_all(bytes).ok()?;
    encoder.finish().ok()
}

/// The web target has no compressor. The SDK is a no-op there, so this is never reached.
#[cfg(target_arch = "wasm32")]
fn gzip(_bytes: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use tracer_proto::{Envelope, ProcessKind};

    use super::*;

    #[test]
    fn jitter_stays_in_the_upper_half() {
        let ceiling = Duration::from_secs(10);
        for random in [0, 1, 4_999_999_999, 5_000_000_000, 5_000_000_001, u64::MAX] {
            let wait = jitter(ceiling, random);
            assert!(wait >= ceiling / 2 && wait <= ceiling, "{wait:?}");
        }
        assert_eq!(jitter(Duration::ZERO, 7), Duration::ZERO);
    }

    #[test]
    fn an_assembled_envelope_parses_as_the_wire_type() {
        let header = Header {
            schema: SCHEMA_VERSION,
            app: AppInfo { name: "host".to_owned(), version: "1.2.3".to_owned(), build: Some("abc".to_owned()), process: ProcessKind::Cli },
            install_id: "a".repeat(32),
            session_id: "b".repeat(32),
            device: Some(Device { os_name: "os".to_owned(), arch: "arch".to_owned(), ..Device::default() }),
            created: 5,
        };
        let items: Vec<String> = (0..3)
            .map(|number| serde_json::to_string(&Item { id: format!("{number:032}"), ts: 9, body: ItemBody::Drops(Drops::default()) }).unwrap())
            .collect();
        for count in [0, 1, 3] {
            let body = envelope_json(&header, 77, &items[..count]).unwrap();
            let envelope: Envelope = serde_json::from_slice(&body).unwrap();
            assert_eq!((envelope.schema, envelope.sent_at, envelope.items.len()), (SCHEMA_VERSION, 77, count));
            assert_eq!(
                (envelope.app, envelope.install_id, envelope.session_id),
                (header.app.clone(), header.install_id.clone(), header.session_id.clone())
            );
            assert_eq!(envelope.device, header.device);
            // `created` is the spool's own bookkeeping and stays off the wire.
            assert!(!String::from_utf8(body).unwrap().contains("created"));
        }
    }

    #[test]
    fn gzip_round_trips() {
        use std::io::Read;

        let body = br#"{"schema":1,"items":[]}"#.repeat(50);
        let compressed = gzip(&body).unwrap();
        assert!(compressed.len() < body.len());
        let mut restored = Vec::new();
        flate2::read::GzDecoder::new(compressed.as_slice()).read_to_end(&mut restored).unwrap();
        assert_eq!(restored, body);
    }
}
