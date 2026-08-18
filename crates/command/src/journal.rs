//! Execution state owned by the host: command nesting depth and the journal of mutating commands.

use std::collections::VecDeque;

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
        Self { entries: VecDeque::new(), cap, drop: drop.clamp(1, cap), nested: false, depth: 0, next_seq: 0 }
    }

    /// Also record commands run from inside other commands.
    pub fn record_nested(mut self, yes: bool) -> Self {
        self.nested = yes;
        self
    }

    /// Current nesting depth: 0 outside any command, 1 inside a top-level command…
    pub fn depth(&self) -> usize {
        self.depth
    }

    pub(crate) fn set_depth(&mut self, depth: usize) {
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

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The entries as a JSON array (`[{seq, id, params}]`).
    pub fn to_json(&self) -> Value {
        Value::Array(self.entries.iter().filter_map(|e| serde_json::to_value(e).ok()).collect())
    }
}
