//! Execution state owned by the host: command nesting depth and the journal of mutating commands.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Entries kept before the oldest are dropped.
pub const DEFAULT_CAP: usize = 10_000;
/// How many of the oldest entries are dropped at once when the cap is reached.
pub const DEFAULT_DROP: usize = 1_000;

/// One recorded command call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Increases by one per recorded call over the journal's life (survives dropping old entries).
    pub seq: u64,
    pub id: String,
    pub params: Value,
}

/// One top-level command that ran, as the chokepoint saw it: the registered id (a fixed program
/// string, never its parameters), whether it succeeded and how long its body took.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommandRun {
    pub id: &'static str,
    pub ok: bool,
    pub took: Duration,
}

/// Told about every top-level command that ran (not about calls refused before running, and not
/// about nested calls). Runs on the thread that executed the command, so it must be cheap.
pub type RunObserver = Arc<dyn Fn(CommandRun) + Send + Sync>;

/// The journal of a host: the mutating commands that succeeded, in order, with scrubbed params
/// (replay, session logs, agent "what happened"). Also tracks the current nesting depth of
/// [`crate::Registry::execute`].
///
/// A capped ring: past `cap` entries the oldest `drop` are removed in one go. By default only
/// top-level calls (depth 0) are recorded; [`Journal::record_nested`] records nested calls too.
#[derive(Debug, Clone)]
pub struct Journal {
    entries: VecDeque<JournalEntry>,
    cap: usize,
    drop: usize,
    nested: bool,
    depth: usize,
    next_seq: u64,
    observer: Observer,
}

/// [`Journal`]'s observer slot (the closure itself has no `Debug`).
#[derive(Clone, Default)]
struct Observer(Option<RunObserver>);

impl std::fmt::Debug for Observer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "Observer(set)" } else { "Observer(none)" })
    }
}

impl Default for Journal {
    fn default() -> Self {
        Self::new()
    }
}

impl Journal {
    pub fn new() -> Self {
        Self::with_cap(DEFAULT_CAP, DEFAULT_DROP)
    }

    /// Keep at most `cap` entries (at least 1), dropping the oldest `drop` (1..=cap) when full.
    pub fn with_cap(cap: usize, drop: usize) -> Self {
        let cap = cap.max(1);
        Self { entries: VecDeque::new(), cap, drop: drop.clamp(1, cap), nested: false, depth: 0, next_seq: 0, observer: Observer::default() }
    }

    /// Also record commands run from inside other commands.
    pub fn record_nested(mut self, yes: bool) -> Self {
        self.nested = yes;
        self
    }

    /// Tell `observer` about every top-level command that runs from now on (`None`: nobody).
    /// Telemetry uses this; [`crate::Registry::execute`] calls it after [`crate::Host::after`].
    pub fn set_observer(&mut self, observer: Option<RunObserver>) {
        self.observer = Observer(observer);
    }

    pub fn observer(&self) -> Option<&RunObserver> {
        self.observer.0.as_ref()
    }

    /// Current nesting depth: 0 outside any command, 1 inside a top-level command…
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Set the nesting depth. [`crate::Registry::execute`] does this around every command; a host
    /// sets it only to run a command from one of its hooks as nested (see [`crate::Host`]), and
    /// puts the old value back afterwards.
    pub fn set_depth(&mut self, depth: usize) {
        self.depth = depth;
    }

    /// Whether a call at `depth` is recorded.
    pub fn records(&self, depth: usize) -> bool {
        depth == 0 || self.nested
    }

    /// Record a call (the chokepoint does this; hosts may record extra entries).
    pub fn push(&mut self, id: impl Into<String>, params: Value) {
        self.entries.push_back(JournalEntry { seq: self.next_seq, id: id.into(), params });
        self.next_seq = self.next_seq.saturating_add(1);
        if self.entries.len() > self.cap {
            let n = self.drop.min(self.entries.len());
            self.entries.drain(..n);
        }
    }

    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &JournalEntry> + ExactSizeIterator {
        self.entries.iter()
    }

    /// Entries recorded at or after sequence number `seq`.
    pub fn since(&self, seq: u64) -> impl Iterator<Item = &JournalEntry> {
        self.entries.iter().filter(move |e| e.seq >= seq)
    }

    pub fn last(&self) -> Option<&JournalEntry> {
        self.entries.back()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The sequence number the next recorded entry gets.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Keep only the first `len` entries (drop the newest; e.g. calls undone and then replaced).
    /// Sequence numbers already handed out are not reused.
    pub fn truncate(&mut self, len: usize) {
        self.entries.truncate(len);
    }

    /// Keep only the entries `keep` returns true for, in order.
    pub fn retain(&mut self, mut keep: impl FnMut(&JournalEntry) -> bool) {
        self.entries.retain(|e| keep(e));
    }

    /// Remove and return the newest entry.
    pub fn pop(&mut self) -> Option<JournalEntry> {
        self.entries.pop_back()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The entries as a JSON array (`[{seq, id, params}]`).
    pub fn to_json(&self) -> Value {
        Value::Array(self.entries.iter().filter_map(|e| serde_json::to_value(e).ok()).collect())
    }
}
