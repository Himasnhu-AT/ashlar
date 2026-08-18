//! Lock-free publishing of the latest document snapshot to other threads.
//!
//! The UI thread owns the [`crate::Store`]; an audio callback, a renderer or a background job reads
//! the newest snapshot from a [`SnapshotCell`] without taking a lock and without ever blocking the
//! writer (it is an `arc-swap` underneath).

use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::revision::Revision;

/// One published state: the document and its revision.
#[derive(Debug)]
pub struct Published<D> {
    pub doc: Arc<D>,
    pub revision: Revision,
}

/// Holds the latest published snapshot.
pub struct SnapshotCell<D> {
    cur: ArcSwap<Published<D>>,
}

impl<D> std::fmt::Debug for SnapshotCell<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotCell").field("revision", &self.revision()).finish()
    }
}

impl<D> SnapshotCell<D> {
    pub fn new(doc: Arc<D>, revision: Revision) -> SnapshotCell<D> {
        SnapshotCell { cur: ArcSwap::from_pointee(Published { doc, revision }) }
    }

    /// Replace the published snapshot (writer side; one small allocation).
    pub fn publish(&self, doc: Arc<D>, revision: Revision) {
        self.cur.store(Arc::new(Published { doc, revision }));
    }

    /// The latest snapshot, to keep (a reference count increment, no lock).
    pub fn load(&self) -> Arc<Published<D>> {
        self.cur.load_full()
    }

    /// Read the latest snapshot in place without touching its reference count (the cheapest read,
    /// for real-time threads). Keep `f` short: the snapshot can't be freed while it runs.
    pub fn read<R>(&self, f: impl FnOnce(&Published<D>) -> R) -> R {
        let g = self.cur.load();
        f(&g)
    }

    /// Revision of the latest snapshot (to skip work when nothing changed).
    pub fn revision(&self) -> Revision {
        self.cur.load().revision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readers_see_latest_and_keep_old() {
        let r0 = Revision::next();
        let cell = SnapshotCell::new(Arc::new(1), r0);
        let old = cell.load();
        let r1 = Revision::next();
        cell.publish(Arc::new(2), r1);
        assert_eq!(*old.doc, 1, "a held snapshot stays valid");
        assert_eq!(cell.read(|p| *p.doc), 2);
        assert_eq!(cell.revision(), r1);
    }

    #[test]
    fn concurrent_readers_never_see_torn_state() {
        let cell = Arc::new(SnapshotCell::new(Arc::new((0u64, 0u64)), Revision::next()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let cell = cell.clone();
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut reads = 0u64;
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        cell.read(|p| assert_eq!(p.doc.0, p.doc.1));
                        reads += 1;
                    }
                    reads
                })
            })
            .collect();
        for i in 1..=20_000u64 {
            cell.publish(Arc::new((i, i)), Revision::next());
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for r in readers {
            assert!(r.join().unwrap() > 0);
        }
        assert_eq!(cell.load().doc.0, 20_000);
    }
}
