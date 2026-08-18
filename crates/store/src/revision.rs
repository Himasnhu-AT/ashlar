//! Revision numbers, revision-tracked values and a small revision-keyed cache.
//!
//! Every [`Revision`] comes from one process-wide counter, so a revision number names one exact
//! content: a cache keyed by it never serves stale data, even across undo (which restores the old
//! revision together with the old content).

use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// A content version. Fresh ones come from [`Revision::next`] and are never reused in a process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Revision(pub u64);

impl Revision {
    /// A revision no content has (a cache's "nothing yet").
    pub const NONE: Revision = Revision(0);

    /// A new, never used revision.
    pub fn next() -> Revision {
        Revision(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// A value with its own revision, bumped by every mutable access. Put these on the parts of a
/// document that views cache (a track, a page, a mesh): the view keys its cache by the part's
/// revision and rebuilds only what changed.
///
/// Cloning keeps the revision (same content); equality compares values only; serializes as the
/// bare value and gets a fresh revision when deserialized.
#[derive(Clone, Debug, Default)]
pub struct Tracked<T> {
    value: T,
    rev: Revision,
}

/// Another name for [`Tracked`].
pub type Versioned<T> = Tracked<T>;

impl<T> Tracked<T> {
    pub fn new(value: T) -> Tracked<T> {
        Tracked { value, rev: Revision::next() }
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    /// Mutable access; bumps the revision.
    pub fn get_mut(&mut self) -> &mut T {
        self.rev = Revision::next();
        &mut self.value
    }

    /// Replace the value; bumps the revision.
    pub fn set(&mut self, value: T) {
        self.value = value;
        self.rev = Revision::next();
    }

    pub fn revision(&self) -> Revision {
        self.rev
    }

    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<T: Clone> Tracked<Arc<T>> {
    /// Copy-on-write access to a shared chunk: clones the chunk only if another snapshot still
    /// holds it, and bumps the revision.
    pub fn make_mut(&mut self) -> &mut T {
        self.rev = Revision::next();
        Arc::make_mut(&mut self.value)
    }
}

impl<T> Deref for Tracked<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: PartialEq> PartialEq for Tracked<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Serialize> Serialize for Tracked<T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(s)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Tracked<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Tracked::new)
    }
}

/// A small cache of values computed from keyed inputs (typically `(id, Revision)`), keeping the
/// `cap` most recently used. Lookups are a linear scan: meant for a handful of entries per view
/// (laid-out text for the visible paragraphs, the waveform of the visible clips).
#[derive(Clone, Debug)]
pub struct Memo<K, V> {
    slots: Vec<(K, V, u64)>,
    cap: usize,
    clock: u64,
}

impl<K: PartialEq, V> Memo<K, V> {
    /// A cache of at most `cap` entries (at least 1).
    pub fn new(cap: usize) -> Memo<K, V> {
        Memo { slots: Vec::new(), cap: cap.max(1), clock: 0 }
    }

    /// The cached value for `key`.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.clock += 1;
        let clock = self.clock;
        let slot = self.slots.iter_mut().find(|s| s.0 == *key)?;
        slot.2 = clock;
        Some(&slot.1)
    }

    /// The cached value for `key`, computing and storing it on a miss (evicting the least
    /// recently used entry when full).
    pub fn get_or_insert_with(&mut self, key: K, compute: impl FnOnce() -> V) -> &V {
        self.clock += 1;
        let clock = self.clock;
        let i = match self.slots.iter().position(|s| s.0 == key) {
            Some(i) => i,
            None if self.slots.len() < self.cap => {
                self.slots.push((key, compute(), clock));
                self.slots.len() - 1
            }
            None => {
                let lru = self.slots.iter().enumerate().min_by_key(|(_, s)| s.2).map(|(i, _)| i).unwrap_or(0);
                self.slots[lru] = (key, compute(), clock);
                lru
            }
        };
        let slot = &mut self.slots[i];
        slot.2 = clock;
        &slot.1
    }

    /// Drop every entry the predicate rejects (e.g. ids that no longer exist).
    pub fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.slots.retain(|s| keep(&s.0));
    }

    pub fn clear(&mut self) {
        self.slots.clear();
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_are_unique() {
        let a = Revision::next();
        let b = Revision::next();
        assert!(b > a);
        assert_ne!(a, Revision::NONE);
    }

    #[test]
    fn tracked_bumps_on_mutation_only() {
        let mut t = Tracked::new(vec![1]);
        let r0 = t.revision();
        assert_eq!(t.len(), 1, "deref reads");
        assert_eq!(t.revision(), r0);
        let c = t.clone();
        assert_eq!(c.revision(), r0, "clone keeps the revision");
        t.get_mut().push(2);
        assert_ne!(t.revision(), r0);
        assert_eq!(c.revision(), r0);
        let r1 = t.revision();
        t.set(vec![]);
        assert_ne!(t.revision(), r1);
        assert_eq!(Tracked::new(1), Tracked::new(1), "equality ignores revisions");
    }

    #[test]
    fn tracked_arc_copy_on_write() {
        let mut t = Tracked::new(Arc::new(vec![0u8; 4]));
        let snapshot = t.clone();
        t.make_mut()[0] = 9;
        assert_eq!(snapshot[0], 0, "the snapshot is untouched");
        assert_eq!(t[0], 9);
        assert!(!Arc::ptr_eq(&t, &snapshot));
    }

    #[test]
    fn tracked_serde_is_transparent() {
        let t = Tracked::new(5);
        assert_eq!(serde_json::to_string(&t).unwrap(), "5");
        let back: Tracked<i32> = serde_json::from_str("5").unwrap();
        assert_eq!(*back, 5);
        assert_ne!(back.revision(), t.revision());
    }

    #[test]
    fn memo_hits_misses_and_evicts_lru() {
        let mut m: Memo<(u32, Revision), String> = Memo::new(2);
        let r = Revision::next();
        let mut computed = 0;
        for _ in 0..3 {
            m.get_or_insert_with((1, r), || {
                computed += 1;
                "one".into()
            });
        }
        assert_eq!(computed, 1);
        m.get_or_insert_with((2, r), || "two".into());
        assert!(m.get(&(1, r)).is_some()); // 1 is now more recent than 2
        m.get_or_insert_with((3, r), || "three".into());
        assert_eq!(m.len(), 2);
        assert!(m.get(&(2, r)).is_none(), "least recently used evicted");
        assert!(m.get(&(1, r)).is_some());
        let r2 = Revision::next();
        assert_eq!(m.get_or_insert_with((1, r2), || "one v2".into()), "one v2", "a new revision misses");
        m.retain(|k| k.1 == r2);
        assert_eq!(m.len(), 1);
        m.clear();
        assert!(m.is_empty());
        assert_eq!(Memo::<u8, u8>::new(0).cap, 1);
    }
}
