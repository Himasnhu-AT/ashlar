//! Undo history of labelled document snapshots.
//!
//! The working line is two stacks: `undo` holds the state *before* each step (oldest first, with
//! the step's label), `redo` holds undone steps (the next one last, with the state each leads to).
//! Each entry also carries an extra value `X` (selection, caret…) restored with the document, and
//! the state's [`Revision`].
//!
//! - **Merging.** A continuous gesture (a drag, a scrub) passes the same merge key on every edit;
//!   while the key and label match the last step, edits fold into that one step.
//! - **Limits.** At most `max_steps` undo steps, and optionally at most `max_bytes` as measured by
//!   a caller-supplied size function (approximate: snapshots that share structure are counted in
//!   full each time).
//! - **Branching** (opt-in). Making a new step after undoing normally drops the undone steps.
//!   With branching on they are kept as a [`Branch`] hanging off the state they grew from; when
//!   that state is current again, [`History::switch_branch`] makes a branch the redo line.

use std::collections::HashSet;
use std::sync::Arc;

use crate::revision::Revision;

/// Undo steps kept unless set with [`History::set_max_steps`].
pub const DEFAULT_MAX_STEPS: usize = 100;

/// One state of the history: the document, the extra state that goes with it, and the step label
/// (for `undo` entries the step that followed this state; for `redo` and branch entries the step
/// that made it).
#[derive(Debug)]
pub struct Entry<D, X = ()> {
    pub label: String,
    pub doc: Arc<D>,
    pub extra: X,
    pub revision: Revision,
    bytes: usize,
}

/// Undone steps kept after a new step was made from the state `parent`.
#[derive(Debug)]
pub struct Branch<D, X = ()> {
    /// Revision of the state the branch grows from.
    pub parent: Revision,
    /// The branch's states in order, each with the label of the step that made it.
    pub steps: Vec<Entry<D, X>>,
}

// Manual impls: cloning shares the snapshots, so `D` itself need not be `Clone`.
impl<D, X: Clone> Clone for Entry<D, X> {
    fn clone(&self) -> Self {
        Entry { label: self.label.clone(), doc: Arc::clone(&self.doc), extra: self.extra.clone(), revision: self.revision, bytes: self.bytes }
    }
}

impl<D, X: Clone> Clone for Branch<D, X> {
    fn clone(&self) -> Self {
        Branch { parent: self.parent, steps: self.steps.clone() }
    }
}

type SizeFn<D> = Arc<dyn Fn(&D) -> usize + Send + Sync>;

/// Undo/redo stacks with merge keys, limits and optional branches.
pub struct History<D, X = ()> {
    undo: Vec<Entry<D, X>>,
    redo: Vec<Entry<D, X>>,
    branches: Vec<Branch<D, X>>,
    merge_key: Option<String>,
    max_steps: usize,
    max_bytes: Option<usize>,
    size: Option<SizeFn<D>>,
    branching: bool,
    bytes: usize,
}

impl<D, X: Clone> Clone for History<D, X> {
    fn clone(&self) -> Self {
        History {
            undo: self.undo.clone(),
            redo: self.redo.clone(),
            branches: self.branches.clone(),
            merge_key: self.merge_key.clone(),
            max_steps: self.max_steps,
            max_bytes: self.max_bytes,
            size: self.size.clone(),
            branching: self.branching,
            bytes: self.bytes,
        }
    }
}

impl<D, X> Default for History<D, X> {
    fn default() -> Self {
        History::new()
    }
}

impl<D, X> std::fmt::Debug for History<D, X> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("History")
            .field("undo", &self.undo.iter().map(|e| &e.label).collect::<Vec<_>>())
            .field("redo", &self.redo.iter().map(|e| &e.label).collect::<Vec<_>>())
            .field("branches", &self.branches.len())
            .field("merge_key", &self.merge_key)
            .field("bytes", &self.bytes)
            .finish()
    }
}

impl<D, X> History<D, X> {
    /// An empty history keeping [`DEFAULT_MAX_STEPS`] steps, no byte limit, no branching.
    pub fn new() -> History<D, X> {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            branches: Vec::new(),
            merge_key: None,
            max_steps: DEFAULT_MAX_STEPS,
            max_bytes: None,
            size: None,
            branching: false,
            bytes: 0,
        }
    }

    // ------------------------------------------------------------------ configuration

    /// Keep at most `n` undo steps (0 = no undo); drops the oldest at once if over.
    pub fn set_max_steps(&mut self, n: usize) {
        self.max_steps = n;
        self.trim();
    }

    pub fn max_steps(&self) -> usize {
        self.max_steps
    }

    /// Keep the history under about `bytes` (needs [`History::set_size_fn`]); the newest undo
    /// step is always kept. `None` = no byte limit.
    pub fn set_max_bytes(&mut self, bytes: Option<usize>) {
        self.max_bytes = bytes;
        self.trim();
    }

    /// How many bytes a snapshot holds, for the byte limit. Entries already recorded keep the size
    /// they were recorded with.
    pub fn set_size_fn(&mut self, f: impl Fn(&D) -> usize + Send + Sync + 'static) {
        self.size = Some(Arc::new(f));
    }

    /// Keep undone steps as branches instead of dropping them.
    pub fn set_branching(&mut self, on: bool) {
        self.branching = on;
        if !on {
            self.drop_branches(0..self.branches.len());
        }
    }

    // ------------------------------------------------------------------ queries

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Number of undo steps.
    pub fn depth(&self) -> usize {
        self.undo.len()
    }

    /// The undo stack, oldest first.
    pub fn undo_entries(&self) -> &[Entry<D, X>] {
        &self.undo
    }

    /// The redo stack, next one last.
    pub fn redo_entries(&self) -> &[Entry<D, X>] {
        &self.redo
    }

    /// Labels of the undo steps, oldest first.
    pub fn undo_labels(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.undo.iter().map(|e| e.label.as_str())
    }

    /// Labels of the redo steps, next one first.
    pub fn redo_labels(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.redo.iter().rev().map(|e| e.label.as_str())
    }

    /// Label of the step Undo would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    /// Label of the step Redo would apply.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    pub fn branches(&self) -> &[Branch<D, X>] {
        &self.branches
    }

    /// Approximate bytes held (0 without a size function).
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn merge_key(&self) -> Option<&str> {
        self.merge_key.as_deref()
    }

    /// Would an edit with this label and merge key fold into the last step?
    pub fn can_merge(&self, label: &str, key: &str) -> bool {
        self.merge_key.as_deref() == Some(key) && self.undo.last().is_some_and(|e| e.label == label)
    }

    // ------------------------------------------------------------------ changes

    /// Set (or clear) the key a following [`History::can_merge`] compares with.
    pub fn set_merge_key(&mut self, key: Option<&str>) {
        self.merge_key = key.map(str::to_string);
    }

    /// Record a new step `label` made from the state (`doc`, `extra`, `revision`). The redo line
    /// is dropped, or kept as a branch when branching is on. Clears the merge key.
    pub fn record(&mut self, label: &str, doc: Arc<D>, extra: X, revision: Revision) {
        if !self.redo.is_empty() {
            let steps: Vec<Entry<D, X>> = self.redo.drain(..).rev().collect();
            if self.branching {
                self.branches.push(Branch { parent: revision, steps });
            }
        }
        let e = self.entry(label.to_string(), doc, extra, revision);
        self.undo.push(e);
        self.merge_key = None;
        self.trim();
    }

    /// Undo: given the current state, return the state to go back to (its label is the step
    /// undone). The current state moves to the redo stack.
    pub fn undo(&mut self, doc: Arc<D>, extra: X, revision: Revision) -> Option<Entry<D, X>> {
        let prev = self.undo.pop()?;
        let cur = self.entry(prev.label.clone(), doc, extra, revision);
        self.redo.push(cur);
        self.merge_key = None;
        self.recount();
        Some(prev)
    }

    /// Redo: given the current state, return the state to go forward to (its label is the step
    /// redone). The current state moves to the undo stack.
    pub fn redo(&mut self, doc: Arc<D>, extra: X, revision: Revision) -> Option<Entry<D, X>> {
        let next = self.redo.pop()?;
        let cur = self.entry(next.label.clone(), doc, extra, revision);
        self.undo.push(cur);
        self.merge_key = None;
        self.recount();
        Some(next)
    }

    /// Make branch `index` the redo line. Only possible when the branch grows from the current
    /// state (`current`); the redo line it replaces becomes a branch itself. Returns false (and
    /// changes nothing) otherwise.
    pub fn switch_branch(&mut self, index: usize, current: Revision) -> bool {
        if self.branches.get(index).is_none_or(|b| b.parent != current) {
            return false;
        }
        let b = self.branches.remove(index);
        if !self.redo.is_empty() {
            let steps: Vec<Entry<D, X>> = self.redo.drain(..).rev().collect();
            self.branches.push(Branch { parent: current, steps });
        }
        self.redo = b.steps.into_iter().rev().collect();
        self.merge_key = None;
        true
    }

    /// Fold every step recorded after the first `depth` into one step called `label` (a command
    /// that ran several edits becomes one undo step). No-op when there are none.
    pub fn squash_since(&mut self, depth: usize, label: &str) {
        if self.undo.len() <= depth {
            return;
        }
        let removed: HashSet<Revision> = self.undo.drain(depth.saturating_add(1)..).map(|e| e.revision).collect();
        if let Some(e) = self.undo.last_mut() {
            e.label = label.to_string();
        }
        self.prune_orphans(removed);
    }

    /// Forget everything (a new or reopened document).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.branches.clear();
        self.merge_key = None;
        self.bytes = 0;
    }

    // ------------------------------------------------------------------ internals

    fn entry(&self, label: String, doc: Arc<D>, extra: X, revision: Revision) -> Entry<D, X> {
        let bytes = self.size.as_ref().map(|f| f(&doc)).unwrap_or(0);
        Entry { label, doc, extra, revision, bytes }
    }

    fn trim(&mut self) {
        let mut removed: HashSet<Revision> = HashSet::new();
        while self.undo.len() > self.max_steps {
            let e = self.undo.remove(0);
            removed.insert(e.revision);
        }
        // Branch states count against the same step limit; the oldest branches go first.
        let mut states: usize = self.branches.iter().map(|b| b.steps.len()).sum();
        let mut drop = 0;
        while states > self.max_steps && drop < self.branches.len() {
            states -= self.branches.get(drop).map_or(0, |b| b.steps.len());
            drop += 1;
        }
        self.drop_branches(0..drop);
        self.recount();
        if let Some(max) = self.max_bytes {
            while self.bytes > max {
                if !self.branches.is_empty() {
                    self.drop_branches(0..1);
                } else if self.undo.len() > 1 {
                    let e = self.undo.remove(0);
                    removed.insert(e.revision);
                    self.bytes = self.bytes.saturating_sub(e.bytes);
                } else {
                    break;
                }
            }
        }
        self.prune_orphans(removed);
    }

    /// Recompute the byte total from the entries (the source of truth).
    fn recount(&mut self) {
        let line: usize = self.undo.iter().chain(&self.redo).map(|e| e.bytes).sum();
        let branches: usize = self.branches.iter().flat_map(|b| &b.steps).map(|e| e.bytes).sum();
        self.bytes = line.saturating_add(branches);
    }

    fn drop_branches(&mut self, range: std::ops::Range<usize>) {
        let gone: HashSet<Revision> = self.branches.drain(range).flat_map(|b| b.steps.into_iter().map(|e| e.revision)).collect();
        self.prune_orphans(gone);
    }

    /// Drop branches whose parent state is gone (repeatedly: branches of dropped branches too).
    fn prune_orphans(&mut self, mut gone: HashSet<Revision>) {
        while !gone.is_empty() && !self.branches.is_empty() {
            let (dead, live): (Vec<_>, Vec<_>) = self.branches.drain(..).partition(|b| gone.contains(&b.parent));
            self.branches = live;
            gone = dead.into_iter().flat_map(|b| b.steps.into_iter().map(|e| e.revision)).collect();
        }
        self.recount();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type H = History<i32, &'static str>;

    /// A tiny driver: the current state plus the history.
    struct Doc {
        cur: Arc<i32>,
        rev: Revision,
        h: H,
    }

    impl Doc {
        fn new() -> Doc {
            Doc { cur: Arc::new(0), rev: Revision::next(), h: H::new() }
        }
        fn edit(&mut self, label: &str, v: i32) {
            self.h.record(label, self.cur.clone(), "x", self.rev);
            self.cur = Arc::new(v);
            self.rev = Revision::next();
        }
        fn undo(&mut self) -> Option<String> {
            let e = self.h.undo(self.cur.clone(), "x", self.rev)?;
            self.cur = e.doc;
            self.rev = e.revision;
            Some(e.label)
        }
        fn redo(&mut self) -> Option<String> {
            let e = self.h.redo(self.cur.clone(), "x", self.rev)?;
            self.cur = e.doc;
            self.rev = e.revision;
            Some(e.label)
        }
    }

    #[test]
    fn undo_redo_walk_the_line() {
        let mut d = Doc::new();
        assert_eq!(d.undo(), None);
        assert_eq!(d.redo(), None);
        d.edit("a", 1);
        d.edit("b", 2);
        assert_eq!(d.h.undo_labels().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(d.undo().as_deref(), Some("b"));
        assert_eq!(*d.cur, 1);
        assert_eq!(d.h.redo_label(), Some("b"));
        assert_eq!(d.undo().as_deref(), Some("a"));
        assert_eq!(*d.cur, 0);
        assert_eq!(d.h.redo_labels().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(d.redo().as_deref(), Some("a"));
        assert_eq!(d.redo().as_deref(), Some("b"));
        assert_eq!(*d.cur, 2);
        d.undo();
        d.edit("c", 3);
        assert!(!d.h.can_redo(), "a new step drops the redo line");
        assert!(d.h.branches().is_empty());
    }

    #[test]
    fn step_limit_drops_oldest() {
        let mut d = Doc::new();
        d.h.set_max_steps(3);
        for i in 1..=5 {
            d.edit(&format!("e{i}"), i);
        }
        assert_eq!(d.h.undo_labels().collect::<Vec<_>>(), ["e3", "e4", "e5"]);
        d.h.set_max_steps(1);
        assert_eq!(d.h.undo_labels().collect::<Vec<_>>(), ["e5"]);
        d.h.set_max_steps(0);
        d.edit("e6", 6);
        assert!(!d.h.can_undo());
    }

    #[test]
    fn byte_limit_keeps_newest_step() {
        let mut d = Doc::new();
        d.h.set_size_fn(|v: &i32| (*v as usize + 1) * 100);
        d.h.set_max_bytes(Some(450));
        for i in 0..5 {
            d.edit("e", i + 1);
        }
        // states before each step: 0,1,2,3,4 → 100,200,300,400,500 bytes; only the newest fits
        assert_eq!(d.h.depth(), 1);
        assert_eq!(d.h.bytes(), 500);
        d.h.set_max_bytes(None);
        for i in 0..3 {
            d.edit("f", i);
        }
        assert_eq!(d.h.depth(), 4);
        assert_eq!(d.h.bytes(), 500 + 600 + 100 + 200);
        d.undo();
        assert_eq!(d.h.bytes(), 500 + 600 + 100 + 300, "the undone state moved to redo");
        d.edit("g", 0);
        assert_eq!(d.h.bytes(), 500 + 600 + 100 + 200, "redo dropped");
    }

    #[test]
    fn merge_key_rules() {
        let mut d = Doc::new();
        assert!(!d.h.can_merge("drag", "k"), "nothing to merge into");
        d.edit("drag", 1);
        d.h.set_merge_key(Some("k"));
        assert!(d.h.can_merge("drag", "k"));
        assert!(!d.h.can_merge("drag", "other"));
        assert!(!d.h.can_merge("other label", "k"));
        d.undo();
        assert!(!d.h.can_merge("drag", "k"), "undo ends the gesture");
        d.redo();
        assert!(!d.h.can_merge("drag", "k"));
        d.h.set_merge_key(Some("k"));
        d.edit("x", 2);
        assert_eq!(d.h.merge_key(), None, "a plain step ends the gesture");
        d.h.set_merge_key(Some(""));
        assert!(!d.h.can_merge("drag", ""), "label must match too");
        assert!(d.h.can_merge("x", ""), "empty key is a key");
    }

    #[test]
    fn squash_since() {
        let mut d = Doc::new();
        d.edit("a", 1);
        let depth = d.h.depth();
        d.h.squash_since(depth, "nothing");
        assert_eq!(d.h.undo_labels().collect::<Vec<_>>(), ["a"]);
        d.edit("b", 2);
        d.edit("c", 3);
        d.h.squash_since(depth, "b+c");
        assert_eq!(d.h.undo_labels().collect::<Vec<_>>(), ["a", "b+c"]);
        assert_eq!(d.undo().as_deref(), Some("b+c"));
        assert_eq!(*d.cur, 1);
        d.h.squash_since(usize::MAX, "x");
    }

    #[test]
    fn branches_are_kept_listed_and_switchable() {
        let mut d = Doc::new();
        d.h.set_branching(true);
        d.edit("a", 1);
        d.edit("b", 2);
        d.undo(); // at 1, redo: b→2
        let fork = d.rev;
        d.edit("c", 3); // b-line becomes a branch off state 1
        assert_eq!(d.h.branches().len(), 1);
        let b = &d.h.branches()[0];
        assert_eq!(b.parent, fork);
        assert_eq!(b.steps.iter().map(|e| (e.label.as_str(), *e.doc)).collect::<Vec<_>>(), [("b", 2)]);
        assert!(!d.h.switch_branch(0, d.rev), "not at the fork");
        assert!(!d.h.switch_branch(9, fork));
        d.undo(); // back at the fork, redo: c→3
        assert!(d.h.switch_branch(0, d.rev));
        assert_eq!(d.redo().as_deref(), Some("b"));
        assert_eq!(*d.cur, 2);
        assert_eq!(d.h.branches().len(), 1, "the c-line is now the branch");
        assert_eq!(*d.h.branches()[0].steps[0].doc, 3);
    }

    #[test]
    fn branches_die_with_their_parent_and_count_against_limits() {
        let mut d = Doc::new();
        d.h.set_branching(true);
        d.h.set_max_steps(3);
        d.edit("a", 1);
        d.undo();
        d.edit("b", 2); // branch [a] off state 0
        assert_eq!(d.h.branches().len(), 1);
        for i in 0..3 {
            d.edit("c", 10 + i); // state 0 falls off the undo stack
        }
        assert!(d.h.branches().is_empty(), "orphan branch dropped");

        let mut d = Doc::new();
        d.h.set_branching(true);
        d.h.set_max_steps(2);
        for i in 0..3 {
            d.edit("x", i + 1);
            d.undo();
        }
        // three single-state branches off state 0, limit 2
        assert_eq!(d.h.branches().len(), 2);
        d.h.set_branching(false);
        assert!(d.h.branches().is_empty());
    }

    #[test]
    fn nested_branches_are_pruned_with_their_parent_branch() {
        let mut d = Doc::new();
        d.h.set_branching(true);
        d.edit("a", 1);
        d.edit("b", 2);
        d.undo();
        d.undo(); // at 0, redo a, b
        d.edit("c", 3); // branch1: [a(1), b(2)] off 0
        assert_eq!(d.h.branches().len(), 1);
        d.undo();
        assert!(d.h.switch_branch(0, d.rev)); // redo line a,b ; branch [c] off 0
        d.redo(); // at 1 (a), redo b
        d.edit("d", 4); // branch [b] off state 1, which lives in the undo stack
        assert_eq!(d.h.branches().len(), 2);
        // Disable undo entirely: states 0 and 1 vanish, so both branches go.
        d.h.set_max_steps(0);
        assert!(d.h.branches().is_empty());
        assert_eq!(d.h.bytes(), 0);
    }
}
