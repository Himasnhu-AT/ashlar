//! [`Store`]: a document as an `Arc` snapshot, edited copy-on-write, with revisions and undo.

use std::sync::Arc;

use crate::history::History;
use crate::revision::Revision;
use crate::snapshot::SnapshotCell;

/// Owns a document `D` and its undo history.
///
/// - Reads are free: [`Store::doc`] borrows the current snapshot, [`Store::snapshot`] clones the
///   `Arc` for a job or another thread.
/// - Edits are copy-on-write: the closure gets a fresh copy (`Arc::make_mut` while the history
///   still holds the old state), so its cost is `D::clone`. Make `D` cheap to clone by keeping
///   large parts in `Arc`s (chunks, tracks, pages); an edit then copies only the chunk it touches.
/// - Every change gets a new [`Revision`]; undo and redo restore the revision of the state they
///   bring back, so caches keyed by revision stay valid and [`Store::is_dirty`] is exact.
/// - `X` is extra state recorded with each step and restored by undo/redo (selection, caret). It
///   can be changed freely with [`Store::extra_mut`] without making a step.
pub struct Store<D, X = ()> {
    doc: Arc<D>,
    extra: X,
    revision: Revision,
    saved: Revision,
    history: History<D, X>,
    cell: Option<Arc<SnapshotCell<D>>>,
}

impl<D: std::fmt::Debug, X: std::fmt::Debug> std::fmt::Debug for Store<D, X> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("doc", &self.doc)
            .field("extra", &self.extra)
            .field("revision", &self.revision)
            .field("saved", &self.saved)
            .field("history", &self.history)
            .finish()
    }
}

impl<D: Clone, X: Clone + Default> Store<D, X> {
    /// A store holding `doc`, considered saved (not dirty).
    pub fn new(doc: D) -> Store<D, X> {
        Store::with_extra(doc, X::default())
    }
}

impl<D: Clone, X: Clone> Store<D, X> {
    /// A store holding `doc` with extra state `extra`, considered saved.
    pub fn with_extra(doc: D, extra: X) -> Store<D, X> {
        let revision = Revision::next();
        Store { doc: Arc::new(doc), extra, revision, saved: revision, history: History::new(), cell: None }
    }

    // ------------------------------------------------------------------ reading

    /// The current document.
    pub fn doc(&self) -> &Arc<D> {
        &self.doc
    }

    /// The current document, to keep or send to another thread (an `Arc` clone).
    pub fn snapshot(&self) -> Arc<D> {
        Arc::clone(&self.doc)
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn extra(&self) -> &X {
        &self.extra
    }

    /// Change the extra state without making an undo step.
    pub fn extra_mut(&mut self) -> &mut X {
        &mut self.extra
    }

    pub fn history(&self) -> &History<D, X> {
        &self.history
    }

    /// The history, to change its limits or squash steps.
    pub fn history_mut(&mut self) -> &mut History<D, X> {
        &mut self.history
    }

    // ------------------------------------------------------------------ editing

    /// Apply an undoable edit called `label`.
    pub fn edit<R>(&mut self, label: &str, f: impl FnOnce(&mut D) -> R) -> R {
        match self.try_edit_with::<R, std::convert::Infallible>(label, None, |d, _| Ok(f(d))) {
            Ok(r) => r,
            Err(never) => match never {},
        }
    }

    /// Apply an edit that may fail; on error (or a panic in `f`) nothing changes.
    pub fn try_edit<R, E>(&mut self, label: &str, f: impl FnOnce(&mut D) -> Result<R, E>) -> Result<R, E> {
        self.try_edit_with(label, None, |d, _| f(d))
    }

    /// [`Store::edit`] that folds into the previous step when that step has the same label and
    /// was made with the same `merge_key` with nothing in between (one undo step per drag).
    pub fn edit_merged<R>(&mut self, label: &str, merge_key: &str, f: impl FnOnce(&mut D) -> R) -> R {
        match self.try_edit_with::<R, std::convert::Infallible>(label, Some(merge_key), |d, _| Ok(f(d))) {
            Ok(r) => r,
            Err(never) => match never {},
        }
    }

    /// The general edit: optional merge key, access to the extra state, may fail. On error (or a
    /// panic in `f`) the document, the extra state, the revision and the history are unchanged.
    pub fn try_edit_with<R, E>(&mut self, label: &str, merge_key: Option<&str>, f: impl FnOnce(&mut D, &mut X) -> Result<R, E>) -> Result<R, E> {
        let mut doc = Arc::clone(&self.doc);
        let mut extra = self.extra.clone();
        // `self.doc` still holds the old state, so this clones it: the edit works on a copy.
        let r = f(Arc::make_mut(&mut doc), &mut extra)?;
        let merge = merge_key.is_some_and(|k| self.history.can_merge(label, k));
        let old_doc = std::mem::replace(&mut self.doc, doc);
        let old_extra = std::mem::replace(&mut self.extra, extra);
        if !merge {
            self.history.record(label, old_doc, old_extra, self.revision);
            self.history.set_merge_key(merge_key);
        }
        self.revision = Revision::next();
        self.publish();
        Ok(r)
    }

    /// Make `doc`, built elsewhere (a command's working copy, a job's result), the new state as
    /// one undo step `label`, without copying it. `merge_key` as in [`Store::edit_merged`].
    /// Committing the current snapshot itself (`Arc::ptr_eq`) changes nothing.
    pub fn commit(&mut self, label: &str, merge_key: Option<&str>, doc: Arc<D>) {
        if Arc::ptr_eq(&doc, &self.doc) {
            return;
        }
        let merge = merge_key.is_some_and(|k| self.history.can_merge(label, k));
        let old_doc = std::mem::replace(&mut self.doc, doc);
        if !merge {
            self.history.record(label, old_doc, self.extra.clone(), self.revision);
            self.history.set_merge_key(merge_key);
        }
        self.revision = Revision::next();
        self.publish();
    }

    /// Run `f`, which may make any number of edits, as one undo step called `label`.
    ///
    /// On `Ok` the edits fold into one step (none when they changed nothing); the steps before
    /// the transaction are untouched and the redo line is dropped only if something changed. On
    /// `Err`, or a panic in `f`, the document, extra state, revision, history (redo line and merge
    /// key included) and saved state are put back as they were. See [`Transaction`].
    pub fn transaction<T, E>(&mut self, label: &str, f: impl FnOnce(&mut Store<D, X>) -> Result<T, E>) -> Result<T, E> {
        let mut tx = self.begin_transaction(label);
        let r = f(&mut tx)?;
        tx.commit();
        Ok(r)
    }

    /// Start a [`Transaction`] called `label`: edit through it, then [`Transaction::commit`];
    /// dropping it without committing (an early return, a panic) rolls everything back.
    pub fn begin_transaction(&mut self, label: &str) -> Transaction<'_, D, X> {
        let scope = self.begin_scope(label);
        Transaction { store: self, scope: Some(scope) }
    }

    /// [`Store::transaction`] for a host that owns the store: `f` gets the whole host (to run
    /// commands that take `&mut H`), `store` finds the store in it. Same semantics: one step on
    /// `Ok`, everything put back on `Err` or a panic in `f` (the panic then continues), nesting
    /// as a savepoint, nothing recorded when nothing changed.
    ///
    /// ```
    /// # use ashlar_store::Store;
    /// struct Host { store: Store<Vec<i32>>, runs: u32 }
    /// let mut host = Host { store: Store::new(Vec::new()), runs: 0 };
    /// let r: Result<(), ()> = Store::transaction_on(&mut host, |h| &mut h.store, "Two", |h| {
    ///     h.runs += 1;
    ///     h.store.edit("a", |d| d.push(1));
    ///     h.store.edit("b", |d| d.push(2));
    ///     Ok(())
    /// });
    /// assert_eq!(r, Ok(()));
    /// assert_eq!(host.store.history().undo_labels().collect::<Vec<_>>(), ["Two"]);
    /// ```
    pub fn transaction_on<H, T, E>(
        host: &mut H,
        store: impl Fn(&mut H) -> &mut Store<D, X>,
        label: &str,
        f: impl FnOnce(&mut H) -> Result<T, E>,
    ) -> Result<T, E> {
        let scope = store(host).begin_scope(label);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut *host)));
        let s = store(host);
        match r {
            Ok(Ok(v)) => {
                s.commit_scope(scope);
                Ok(v)
            }
            Ok(Err(e)) => {
                s.rollback_scope(scope);
                Err(e)
            }
            Err(panic) => {
                s.rollback_scope(scope);
                std::panic::resume_unwind(panic)
            }
        }
    }

    /// Set the history aside for a scratch one and remember what a rollback puts back.
    fn begin_scope(&mut self, label: &str) -> Scope<D, X> {
        let mut scratch = History::new();
        scratch.set_max_steps(usize::MAX);
        let history = std::mem::replace(&mut self.history, scratch);
        Scope { label: label.to_string(), doc: Arc::clone(&self.doc), extra: self.extra.clone(), revision: self.revision, saved: self.saved, history }
    }

    /// Put the history back and record one step for the scope if anything changed.
    fn commit_scope(&mut self, b: Scope<D, X>) {
        self.history = b.history;
        if self.revision != b.revision {
            self.history.record(&b.label, b.doc, b.extra, b.revision);
        }
    }

    /// Put back the state at the start of the scope exactly.
    fn rollback_scope(&mut self, b: Scope<D, X>) {
        let changed = self.revision != b.revision;
        self.history = b.history;
        self.saved = b.saved;
        self.doc = b.doc;
        self.extra = b.extra;
        self.revision = b.revision;
        if changed {
            self.publish();
        }
    }

    /// Undo the last step; returns its label.
    pub fn undo(&mut self) -> Option<String> {
        let e = self.history.undo(Arc::clone(&self.doc), self.extra.clone(), self.revision)?;
        self.restore(e.doc, e.extra, e.revision);
        Some(e.label)
    }

    /// Redo the next undone step; returns its label.
    pub fn redo(&mut self) -> Option<String> {
        let e = self.history.redo(Arc::clone(&self.doc), self.extra.clone(), self.revision)?;
        self.restore(e.doc, e.extra, e.revision);
        Some(e.label)
    }

    /// Redo into branch `index` of [`History::branches`] (it must grow from the current state).
    pub fn redo_branch(&mut self, index: usize) -> Option<String> {
        if !self.history.switch_branch(index, self.revision) {
            return None;
        }
        self.redo()
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Replace the whole document (New, Open, Revert): clears the history, counts as saved.
    pub fn replace(&mut self, doc: D, extra: X) {
        self.doc = Arc::new(doc);
        self.extra = extra;
        self.history.clear();
        self.revision = Revision::next();
        self.saved = self.revision;
        self.publish();
    }

    fn restore(&mut self, doc: Arc<D>, extra: X, revision: Revision) {
        self.doc = doc;
        self.extra = extra;
        self.revision = revision;
        self.publish();
    }

    // ------------------------------------------------------------------ saving

    /// The document differs from the last saved (or loaded) state.
    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved
    }

    /// The current state was saved.
    pub fn mark_saved(&mut self) {
        self.saved = self.revision;
    }

    /// Record that the state with `revision` was saved (a background save of an older snapshot).
    pub fn mark_saved_at(&mut self, revision: Revision) {
        self.saved = revision;
    }

    pub fn saved_revision(&self) -> Revision {
        self.saved
    }

    // ------------------------------------------------------------------ other threads

    /// A cell other threads read the latest snapshot from without locking; the store publishes
    /// every change to it. The same cell is returned on every call.
    pub fn subscribe_snapshots(&mut self) -> Arc<SnapshotCell<D>> {
        let (doc, rev) = (Arc::clone(&self.doc), self.revision);
        Arc::clone(self.cell.get_or_insert_with(|| Arc::new(SnapshotCell::new(doc, rev))))
    }

    fn publish(&self) {
        if let Some(c) = &self.cell {
            c.publish(Arc::clone(&self.doc), self.revision);
        }
    }
}

/// An open transaction: its label, what a rollback puts back, and the history it set aside.
struct Scope<D, X> {
    label: String,
    doc: Arc<D>,
    extra: X,
    revision: Revision,
    saved: Revision,
    history: History<D, X>,
}

/// A compound edit in progress ([`Store::begin_transaction`]); derefs to the [`Store`].
///
/// While it is open the store's history is a scratch one holding only the transaction's own
/// steps (so undo inside it goes back no further than its start, and limits never push out an
/// older step); the real history is set aside untouched. [`Transaction::commit`] puts it back
/// and records one step `label` from the state at the start to the current one, or nothing if
/// the revision is the one at the start. Dropping it uncommitted puts back the state at the start
/// exactly: document, extra state, revision, history, merge key, saved revision.
///
/// Transactions nest: an inner one commits as one step into the outer one's scratch history
/// (folded again by the outer commit) and rolls back only its own edits (a savepoint).
pub struct Transaction<'a, D: Clone, X: Clone = ()> {
    store: &'a mut Store<D, X>,
    scope: Option<Scope<D, X>>,
}

impl<D: Clone, X: Clone> Transaction<'_, D, X> {
    /// Fold the transaction's edits into one undo step (none if nothing changed).
    pub fn commit(mut self) {
        if let Some(b) = self.scope.take() {
            self.store.commit_scope(b);
        }
    }

    /// Put everything back as it was at the start (the same as dropping it).
    pub fn rollback(self) {}

    /// Something changed since the start (a commit would record a step).
    pub fn changed(&self) -> bool {
        self.scope.as_ref().is_some_and(|b| b.revision != self.store.revision)
    }
}

impl<D: Clone, X: Clone> std::ops::Deref for Transaction<'_, D, X> {
    type Target = Store<D, X>;
    fn deref(&self) -> &Store<D, X> {
        self.store
    }
}

impl<D: Clone, X: Clone> std::ops::DerefMut for Transaction<'_, D, X> {
    fn deref_mut(&mut self) -> &mut Store<D, X> {
        self.store
    }
}

impl<D: Clone, X: Clone> Drop for Transaction<'_, D, X> {
    fn drop(&mut self) {
        if let Some(b) = self.scope.take() {
            self.store.rollback_scope(b);
        }
    }
}

impl<D: Clone + std::fmt::Debug, X: Clone + std::fmt::Debug> std::fmt::Debug for Transaction<'_, D, X> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transaction").field("label", &self.scope.as_ref().map(|b| b.label.as_str())).field("store", &self.store).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, Default, PartialEq)]
    struct Doc {
        items: Vec<i32>,
    }

    fn store() -> Store<Doc> {
        Store::new(Doc::default())
    }

    #[test]
    fn edit_is_copy_on_write_and_bumps_revision() {
        let mut s = store();
        let before = s.snapshot();
        let r0 = s.revision();
        assert!(!s.is_dirty());
        let n = s.edit("Add", |d| {
            d.items.push(1);
            d.items.len()
        });
        assert_eq!(n, 1);
        assert!(before.items.is_empty(), "old snapshot untouched");
        assert_ne!(s.revision(), r0);
        assert!(s.is_dirty());
        assert_eq!(s.history().undo_label(), Some("Add"));
    }

    #[test]
    fn failed_edit_changes_nothing() {
        let mut s = store();
        s.edit("a", |d| d.items.push(1));
        let rev = s.revision();
        let r: Result<(), &str> = s.try_edit("b", |d| {
            d.items.push(2);
            Err("no")
        });
        assert_eq!(r, Err("no"));
        assert_eq!(s.doc().items, [1]);
        assert_eq!(s.revision(), rev);
        assert_eq!(s.history().depth(), 1);
    }

    #[test]
    fn panicking_edit_changes_nothing() {
        let mut s = store();
        s.edit("a", |d| d.items.push(1));
        let rev = s.revision();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.edit("boom", |d| {
                d.items.push(2);
                panic!("bug in a command")
            })
        }));
        assert!(r.is_err());
        assert_eq!(s.doc().items, [1]);
        assert_eq!(s.revision(), rev);
        assert_eq!(s.history().undo_labels().collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn undo_restores_revision_and_dirty_state() {
        let mut s = store();
        let r0 = s.revision();
        s.edit("a", |d| d.items.push(1));
        let r1 = s.revision();
        s.mark_saved();
        s.edit("b", |d| d.items.push(2));
        assert!(s.is_dirty());
        assert_eq!(s.undo().as_deref(), Some("b"));
        assert_eq!(s.revision(), r1);
        assert!(!s.is_dirty(), "back at the saved state");
        s.undo();
        assert_eq!(s.revision(), r0);
        assert!(s.is_dirty());
        s.redo();
        assert!(!s.is_dirty());
        assert_eq!(s.redo().as_deref(), Some("b"));
        assert_eq!(s.doc().items, [1, 2]);
        assert_eq!(s.undo(), Some("b".into()));
        assert_eq!(s.undo(), Some("a".into()));
        assert_eq!(s.undo(), None);
    }

    #[test]
    fn merged_edits_are_one_step_with_fresh_revisions() {
        let mut s = store();
        s.edit_merged("Move", "drag", |d| d.items.push(1));
        let r1 = s.revision();
        s.mark_saved();
        s.edit_merged("Move", "drag", |d| d.items[0] = 2);
        assert_ne!(s.revision(), r1, "merged content is new content");
        assert!(s.is_dirty());
        s.edit_merged("Move", "drag", |d| d.items[0] = 3);
        assert_eq!(s.history().depth(), 1);
        s.undo();
        assert!(s.doc().items.is_empty());
        s.redo();
        assert_eq!(s.doc().items, [3]);
        s.edit_merged("Move", "drag", |d| d.items[0] = 4);
        assert_eq!(s.history().depth(), 2, "undo/redo ended the gesture");
        s.edit("Other", |_| ());
        s.edit_merged("Move", "drag", |d| d.items[0] = 5);
        assert_eq!(s.history().depth(), 4);
        // A failed merged edit keeps the gesture going.
        let _: Result<(), ()> = s.try_edit_with("Move", Some("drag"), |_, _| Err(()));
        s.edit_merged("Move", "drag", |d| d.items[0] = 6);
        assert_eq!(s.history().depth(), 4);
    }

    #[test]
    fn extra_state_travels_with_steps() {
        let mut s: Store<Doc, Vec<usize>> = Store::new(Doc::default());
        s.extra_mut().push(0); // selection before
        s.try_edit_with::<_, ()>("Add", None, |d, sel| {
            d.items.push(7);
            *sel = vec![0];
            Ok(())
        })
        .unwrap();
        s.extra_mut().clear(); // user deselects (not a step)
        s.undo();
        assert_eq!(s.extra(), &vec![0], "selection before the step");
        s.redo();
        assert!(s.extra().is_empty(), "selection when undo was pressed");
    }

    #[test]
    fn replace_clears_history_and_is_clean() {
        let mut s = store();
        s.edit("a", |d| d.items.push(1));
        s.replace(Doc { items: vec![9] }, ());
        assert!(!s.can_undo() && !s.can_redo() && !s.is_dirty());
        assert_eq!(s.doc().items, [9]);
    }

    #[test]
    fn snapshots_are_published() {
        let mut s = store();
        let cell = s.subscribe_snapshots();
        assert!(Arc::ptr_eq(&cell, &s.subscribe_snapshots()));
        s.edit("a", |d| d.items.push(1));
        assert_eq!(cell.load().doc.items, [1]);
        assert_eq!(cell.revision(), s.revision());
        s.undo();
        assert!(cell.load().doc.items.is_empty());
        s.redo();
        s.replace(Doc::default(), ());
        assert_eq!(cell.revision(), s.revision());
    }

    #[test]
    fn branches_through_the_store() {
        let mut s = store();
        s.history_mut().set_branching(true);
        s.edit("a", |d| d.items.push(1));
        s.undo();
        s.edit("b", |d| d.items.push(2));
        s.undo();
        assert_eq!(s.history().branches().len(), 1);
        assert_eq!(s.redo_branch(0).as_deref(), Some("a"));
        assert_eq!(s.doc().items, [1]);
        assert_eq!(s.redo_branch(0), None, "not at the fork any more");
    }

    #[test]
    fn mark_saved_at_an_older_revision() {
        let mut s = store();
        s.edit("a", |d| d.items.push(1));
        let saving = s.revision();
        s.edit("b", |d| d.items.push(2));
        s.mark_saved_at(saving);
        assert!(s.is_dirty());
        s.undo();
        assert!(!s.is_dirty());
        assert_eq!(s.saved_revision(), saving);
    }

    #[test]
    fn commit_is_one_step_without_copying() {
        let mut s = store();
        let mut work = s.snapshot();
        Arc::make_mut(&mut work).items.push(1);
        Arc::make_mut(&mut work).items.push(2);
        let built = Arc::clone(&work);
        s.commit("Add Two", None, work);
        assert!(Arc::ptr_eq(s.doc(), &built), "the committed Arc is kept as is");
        assert_eq!(s.history().depth(), 1);
        assert!(s.is_dirty());
        let same = s.snapshot();
        let r = s.revision();
        s.commit("Nothing", None, same);
        assert_eq!((s.revision(), s.history().depth()), (r, 1), "committing the current state is a no-op");
        assert_eq!(s.undo().as_deref(), Some("Add Two"));
        assert!(s.doc().items.is_empty());
        assert!(!s.is_dirty());
    }

    fn labels(s: &Store<Doc>) -> (Vec<&str>, Vec<&str>) {
        (s.history().undo_labels().collect(), s.history().redo_labels().collect())
    }

    #[test]
    fn transaction_folds_edits_into_one_step() {
        let mut s = store();
        s.edit("a", |d| d.items.push(1));
        s.edit("b", |d| d.items.push(2));
        s.undo(); // redo line: b
        let r: Result<usize, ()> = s.transaction("Assemble", |s| {
            for i in 10..15 {
                s.edit("part", |d| d.items.push(i));
            }
            s.edit_merged("drag", "k", |d| d.items.push(20));
            s.edit_merged("drag", "k", |d| d.items.push(21));
            assert_eq!(s.history().depth(), 6, "the scratch history holds only this transaction's steps");
            Ok(s.doc().items.len())
        });
        assert_eq!(r, Ok(8));
        assert_eq!(labels(&s), (vec!["a", "Assemble"], vec![]), "one step, redo dropped");
        assert_eq!(s.history().merge_key(), None);
        assert_eq!(s.undo().as_deref(), Some("Assemble"));
        assert_eq!(s.doc().items, [1]);
        assert_eq!(s.redo().as_deref(), Some("Assemble"));
        assert_eq!(s.doc().items, [1, 10, 11, 12, 13, 14, 20, 21]);
    }

    #[test]
    fn failed_transaction_puts_everything_back() {
        let mut s: Store<Doc, u8> = Store::new(Doc::default());
        s.edit("a", |d| d.items.push(1));
        s.mark_saved();
        s.edit_merged("Move", "drag", |d| d.items.push(2));
        s.undo(); // redo: Move
        s.edit_merged("Move", "drag", |d| d.items.push(3)); // drops redo; gesture "drag" open
        s.edit("b", |d| d.items.push(4));
        s.undo(); // redo: b, at [1, 3]
        s.mark_saved();
        s.history_mut().set_merge_key(Some("drag"));
        *s.extra_mut() = 7;
        let cell = s.subscribe_snapshots();
        let (doc, rev, hist) = (s.snapshot(), s.revision(), format!("{:?}", s.history()));
        let r: Result<(), &str> = s.transaction("T", |s| {
            s.edit("x", |d| d.items.clear());
            s.mark_saved();
            *s.extra_mut() = 9;
            s.undo();
            s.edit("y", |d| d.items.push(5));
            Err("no")
        });
        assert_eq!(r, Err("no"));
        assert!(Arc::ptr_eq(s.doc(), &doc));
        assert_eq!((s.revision(), *s.extra()), (rev, 7));
        assert!(!s.is_dirty(), "clean before, clean after");
        assert_eq!(format!("{:?}", s.history()), hist, "history, redo line and merge key as before");
        assert!(s.history().can_merge("Move", "drag"));
        assert_eq!(cell.revision(), rev, "the rollback was published");
        assert_eq!(s.redo().as_deref(), Some("b"));

        // A dirty store stays dirty.
        s.edit("c", |d| d.items.push(6));
        let rev = s.revision();
        let _: Result<(), ()> = s.transaction("T", |s| {
            s.mark_saved();
            Err(())
        });
        assert!(s.is_dirty());
        assert_eq!(s.revision(), rev);
    }

    #[test]
    fn panicking_transaction_puts_everything_back() {
        let mut s = store();
        s.edit("a", |d| d.items.push(1));
        s.edit("b", |d| d.items.push(2));
        s.undo();
        let (rev, hist) = (s.revision(), format!("{:?}", s.history()));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), ()> = s.transaction("T", |s| {
                s.edit("x", |d| d.items.push(9));
                s.edit("y", |d| d.items.push(9));
                panic!("bug in a command")
            });
        }));
        assert!(r.is_err());
        assert_eq!((s.doc().items.as_slice(), s.revision()), ([1].as_slice(), rev));
        assert_eq!(format!("{:?}", s.history()), hist);
        assert!(s.is_dirty());
    }

    #[test]
    fn guard_commits_or_rolls_back_on_drop() {
        let mut s = store();
        {
            let mut tx = s.begin_transaction("Keep");
            tx.edit("x", |d| d.items.push(1));
            tx.edit("y", |d| d.items.push(2));
            assert!(tx.changed());
            tx.commit();
        }
        assert_eq!(labels(&s).0, ["Keep"]);
        let rev = s.revision();
        {
            let mut tx = s.begin_transaction("Drop");
            tx.edit("x", |d| d.items.clear());
        }
        assert_eq!((s.doc().items.as_slice(), s.revision()), ([1, 2].as_slice(), rev));
        let mut tx = s.begin_transaction("Explicit");
        tx.edit("x", |d| d.items.clear());
        tx.rollback();
        assert_eq!(s.doc().items, [1, 2]);
        assert_eq!(labels(&s).0, ["Keep"]);
    }

    #[test]
    fn empty_transaction_records_nothing() {
        let mut s = store();
        s.edit_merged("Move", "drag", |d| d.items.push(1));
        s.edit("b", |d| d.items.push(2));
        s.undo();
        s.history_mut().set_merge_key(Some("drag"));
        let (rev, hist, dirty) = (s.revision(), format!("{:?}", s.history()), s.is_dirty());
        assert_eq!(s.transaction::<_, ()>("Nothing", |_| Ok(3)), Ok(3));
        let _: Result<(), ()> = s.transaction("Failed edit", |s| {
            let _ = s.try_edit("x", |_| Err::<(), ()>(()));
            Ok(())
        });
        // An edit undone inside the transaction is back at the start too.
        let _: Result<(), ()> = s.transaction("Edit and undo", |s| {
            s.edit("x", |d| d.items.push(9));
            s.undo();
            Ok(())
        });
        assert_eq!(s.revision(), rev);
        assert_eq!(format!("{:?}", s.history()), hist, "redo line and gesture kept");
        assert!(s.history().can_merge("Move", "drag"));
        assert_eq!(s.is_dirty(), dirty);
    }

    #[test]
    fn nested_transactions_join_the_outer_one() {
        let mut s = store();
        s.edit("a", |d| d.items.push(1));
        let r: Result<(), ()> = s.transaction("Outer", |s| {
            s.edit("x", |d| d.items.push(2));
            let inner: Result<(), ()> = s.transaction("Inner ok", |s| {
                s.edit("y", |d| d.items.push(3));
                s.edit("z", |d| d.items.push(4));
                Ok(())
            });
            assert_eq!(inner, Ok(()));
            assert_eq!(s.history().undo_labels().collect::<Vec<_>>(), ["x", "Inner ok"]);
            let failed: Result<(), ()> = s.transaction("Inner failed", |s| {
                s.edit("w", |d| d.items.clear());
                Err(())
            });
            assert_eq!(failed, Err(()));
            assert_eq!(s.doc().items, [1, 2, 3, 4], "only the inner edits were rolled back");
            Ok(())
        });
        assert_eq!(r, Ok(()));
        assert_eq!(labels(&s).0, ["a", "Outer"]);
        s.undo();
        assert_eq!(s.doc().items, [1]);

        // An outer failure rolls back a committed inner transaction too.
        let rev = s.revision();
        let _: Result<(), ()> = s.transaction("Outer", |s| {
            let _: Result<(), ()> = s.transaction("Inner", |s| {
                s.edit("y", |d| d.items.push(3));
                Ok(())
            });
            Err(())
        });
        assert_eq!((s.doc().items.as_slice(), s.revision()), ([1].as_slice(), rev));
        assert_eq!(labels(&s), (vec!["a"], vec!["Outer"]));
    }

    #[test]
    fn transaction_and_merge_keys() {
        let mut s = store();
        s.edit_merged("Move", "drag", |d| d.items.push(1));
        let _: Result<(), ()> = s.transaction("Move", |s| {
            // Inside, a gesture merges with itself only, never with a step before the transaction.
            s.edit_merged("Move", "drag", |d| d.items.push(2));
            s.edit_merged("Move", "drag", |d| d.items.push(3));
            assert_eq!(s.history().depth(), 1);
            Ok(())
        });
        assert_eq!(labels(&s).0, ["Move", "Move"]);
        assert_eq!(s.history().merge_key(), None, "a committed transaction ends the gesture");
        s.edit_merged("Move", "drag", |d| d.items.push(4));
        assert_eq!(s.history().depth(), 3);
    }

    #[test]
    fn transaction_respects_limits_once() {
        let mut s = store();
        s.history_mut().set_max_steps(3);
        for i in 0..3 {
            s.edit(&format!("e{i}"), |d| d.items.push(i));
        }
        let _: Result<(), ()> = s.transaction("Many", |s| {
            for i in 0..10 {
                s.edit("part", |d| d.items.push(i));
            }
            Ok(())
        });
        assert_eq!(labels(&s).0, ["e1", "e2", "Many"], "only the folded step counted");
        let _: Result<(), ()> = s.transaction("Fails", |s| {
            for i in 0..10 {
                s.edit("part", |d| d.items.push(i));
            }
            Err(())
        });
        assert_eq!(labels(&s).0, ["e1", "e2", "Many"], "no older step pushed out by a rollback");
        assert_eq!(s.history().max_steps(), 3);
        s.history_mut().set_max_steps(0);
        let _: Result<(), ()> = s.transaction("NoUndo", |s| {
            s.edit("x", |d| d.items.push(1));
            Ok(())
        });
        assert!(!s.can_undo());
        assert!(s.is_dirty());
    }

    /// An app-like owner: commands take the whole host, not just the store.
    struct Host {
        store: Store<Doc, u8>,
        log: Vec<&'static str>,
    }

    impl Host {
        fn new() -> Host {
            Host { store: Store::new(Doc::default()), log: Vec::new() }
        }
        /// A command that needs `&mut Host`.
        fn push(&mut self, label: &'static str, v: i32) {
            self.log.push(label);
            self.store.edit(label, |d| d.items.push(v));
        }
        fn tx<T, E>(&mut self, label: &str, f: impl FnOnce(&mut Host) -> Result<T, E>) -> Result<T, E> {
            Store::transaction_on(self, |h| &mut h.store, label, f)
        }
    }

    fn host_labels(h: &Host) -> (Vec<&str>, Vec<&str>) {
        (h.store.history().undo_labels().collect(), h.store.history().redo_labels().collect())
    }

    #[test]
    fn host_transaction_folds_commands_into_one_step() {
        let mut h = Host::new();
        h.push("a", 1);
        h.push("b", 2);
        h.store.undo(); // redo line: b
        let r: Result<usize, ()> = h.tx("Assemble", |h| {
            for i in 10..13 {
                h.push("part", i);
            }
            h.store.edit_merged("drag", "k", |d| d.items.push(20));
            h.store.edit_merged("drag", "k", |d| d.items.push(21));
            assert_eq!(h.store.history().depth(), 4, "the scratch history holds only this transaction's steps");
            Ok(h.store.doc().items.len())
        });
        assert_eq!(r, Ok(6));
        assert_eq!(host_labels(&h), (vec!["a", "Assemble"], vec![]), "one step, redo dropped");
        assert_eq!(h.store.history().merge_key(), None);
        assert_eq!(h.log, ["a", "b", "part", "part", "part"]);
        assert_eq!(h.store.undo().as_deref(), Some("Assemble"));
        assert_eq!(h.store.doc().items, [1]);
        assert_eq!(h.store.redo().as_deref(), Some("Assemble"));
        assert_eq!(h.store.doc().items, [1, 10, 11, 12, 20, 21]);
    }

    #[test]
    fn failed_host_transaction_puts_everything_back() {
        let mut h = Host::new();
        h.push("a", 1);
        h.store.edit_merged("Move", "drag", |d| d.items.push(2));
        h.push("b", 3);
        h.store.undo(); // redo: b
        h.store.mark_saved();
        h.store.history_mut().set_merge_key(Some("drag"));
        *h.store.extra_mut() = 7;
        let cell = h.store.subscribe_snapshots();
        let (doc, rev, hist) = (h.store.snapshot(), h.store.revision(), format!("{:?}", h.store.history()));
        let r: Result<(), &str> = h.tx("T", |h| {
            h.push("x", 9);
            h.store.mark_saved();
            *h.store.extra_mut() = 9;
            h.store.undo();
            h.push("y", 5);
            Err("no")
        });
        assert_eq!(r, Err("no"));
        assert!(Arc::ptr_eq(h.store.doc(), &doc));
        assert_eq!((h.store.revision(), *h.store.extra()), (rev, 7));
        assert!(!h.store.is_dirty(), "clean before, clean after");
        assert_eq!(format!("{:?}", h.store.history()), hist, "history, redo line and merge key as before");
        assert!(h.store.history().can_merge("Move", "drag"));
        assert_eq!(cell.revision(), rev, "the rollback was published");
        assert_eq!(h.log, ["a", "b", "x", "y"], "the rest of the host is the host's business");
        assert_eq!(h.store.redo().as_deref(), Some("b"));
    }

    #[test]
    fn panicking_host_transaction_puts_everything_back() {
        let mut h = Host::new();
        h.push("a", 1);
        h.push("b", 2);
        h.store.undo();
        let (rev, hist) = (h.store.revision(), format!("{:?}", h.store.history()));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), ()> = h.tx("T", |h| {
                h.push("x", 9);
                h.push("y", 9);
                panic!("bug in a command")
            });
        }));
        assert!(r.is_err(), "the panic continues");
        assert_eq!((h.store.doc().items.as_slice(), h.store.revision()), ([1].as_slice(), rev));
        assert_eq!(format!("{:?}", h.store.history()), hist);
        assert!(h.store.is_dirty());
    }

    #[test]
    fn empty_host_transaction_records_nothing() {
        let mut h = Host::new();
        h.store.edit_merged("Move", "drag", |d| d.items.push(1));
        h.push("b", 2);
        h.store.undo();
        h.store.history_mut().set_merge_key(Some("drag"));
        let (rev, hist, dirty) = (h.store.revision(), format!("{:?}", h.store.history()), h.store.is_dirty());
        assert_eq!(h.tx::<_, ()>("Nothing", |_| Ok(3)), Ok(3));
        let _: Result<(), ()> = h.tx("Edit and undo", |h| {
            h.push("x", 9);
            h.store.undo();
            Ok(())
        });
        assert_eq!(h.store.revision(), rev);
        assert_eq!(format!("{:?}", h.store.history()), hist, "redo line and gesture kept");
        assert!(h.store.history().can_merge("Move", "drag"));
        assert_eq!(h.store.is_dirty(), dirty);
    }

    #[test]
    fn host_transactions_nest_with_each_other_and_store_transactions() {
        let mut h = Host::new();
        h.push("a", 1);
        let r: Result<(), ()> = h.tx("Outer", |h| {
            h.push("x", 2);
            let inner: Result<(), ()> = h.tx("Inner ok", |h| {
                h.push("y", 3);
                h.push("z", 4);
                Ok(())
            });
            assert_eq!(inner, Ok(()));
            let store_inner: Result<(), ()> = h.store.transaction("Store inner", |s| {
                s.edit("w", |d| d.items.push(5));
                Ok(())
            });
            assert_eq!(store_inner, Ok(()));
            assert_eq!(h.store.history().undo_labels().collect::<Vec<_>>(), ["x", "Inner ok", "Store inner"]);
            let failed: Result<(), ()> = h.tx("Inner failed", |h| {
                h.store.edit("v", |d| d.items.clear());
                Err(())
            });
            assert_eq!(failed, Err(()));
            assert_eq!(h.store.doc().items, [1, 2, 3, 4, 5], "only the inner edits were rolled back");
            Ok(())
        });
        assert_eq!(r, Ok(()));
        assert_eq!(host_labels(&h).0, ["a", "Outer"]);
        h.store.undo();
        assert_eq!(h.store.doc().items, [1]);

        // An outer failure rolls back a committed inner transaction too.
        let rev = h.store.revision();
        let _: Result<(), ()> = h.tx("Outer", |h| {
            let _: Result<(), ()> = h.tx("Inner", |h| {
                h.push("y", 3);
                Ok(())
            });
            Err(())
        });
        assert_eq!((h.store.doc().items.as_slice(), h.store.revision()), ([1].as_slice(), rev));
        assert_eq!(host_labels(&h), (vec!["a"], vec!["Outer"]));
    }

    #[test]
    fn host_transaction_respects_limits_once() {
        let mut h = Host::new();
        h.store.history_mut().set_max_steps(2);
        h.push("e0", 0);
        h.push("e1", 1);
        let _: Result<(), ()> = h.tx("Many", |h| {
            for i in 0..10 {
                h.push("part", i);
            }
            Ok(())
        });
        assert_eq!(host_labels(&h).0, ["e1", "Many"]);
        let _: Result<(), ()> = h.tx("Fails", |h| {
            for i in 0..10 {
                h.push("part", i);
            }
            Err(())
        });
        assert_eq!(host_labels(&h).0, ["e1", "Many"], "no older step pushed out by a rollback");
        assert_eq!(h.store.history().max_steps(), 2);
    }
}
