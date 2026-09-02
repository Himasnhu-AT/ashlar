#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `Store::transaction` against a reference model of a hand-written "one step" helper: snapshot
//! the document, the stacks, the merge key and the clean state; run the edits; on success put the
//! undo stack back to its old depth and push one step from the old document; on failure or panic
//! put everything back. Random sequences of edits, merged edits, transactions (succeeding,
//! failing, panicking, nested, empty), undo, redo and saves must agree on every observable.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use ashlar_store::Store;

type Doc = Vec<i32>;

#[derive(Clone, Default)]
struct Model {
    doc: Arc<Doc>,
    undo: Vec<(String, Arc<Doc>)>,
    redo: Vec<(String, Arc<Doc>)>,
    merge_key: Option<String>,
    limit: usize,
    /// Content counter: the model's "revision" (bumped by every change, restored by undo).
    rev: u64,
    next: u64,
    saved: u64,
    revs_undo: Vec<u64>,
    revs_redo: Vec<u64>,
}

impl Model {
    fn bump(&mut self) {
        self.next += 1;
        self.rev = self.next;
    }

    fn push(&mut self, label: &str, old: Arc<Doc>, old_rev: u64) {
        self.undo.push((label.to_string(), old));
        self.revs_undo.push(old_rev);
        while self.undo.len() > self.limit {
            self.undo.remove(0);
            self.revs_undo.remove(0);
        }
        self.redo.clear();
        self.revs_redo.clear();
        self.merge_key = None;
    }

    fn edit(&mut self, label: &str, key: Option<&str>, v: i32) {
        let merge = key.is_some() && self.merge_key.as_deref() == key && self.undo.last().is_some_and(|u| u.0 == label);
        let mut d = (*self.doc).clone();
        d.push(v);
        let old = std::mem::replace(&mut self.doc, Arc::new(d));
        if !merge {
            self.push(label, old, self.rev);
            self.merge_key = key.map(str::to_string);
        }
        self.bump();
    }

    fn undo(&mut self) -> Option<String> {
        let (label, prev) = self.undo.pop()?;
        let prev_rev = self.revs_undo.pop().unwrap();
        self.merge_key = None;
        let cur = std::mem::replace(&mut self.doc, prev);
        self.redo.push((label.clone(), cur));
        self.revs_redo.push(std::mem::replace(&mut self.rev, prev_rev));
        Some(label)
    }

    fn redo(&mut self) -> Option<String> {
        let (label, next) = self.redo.pop()?;
        let next_rev = self.revs_redo.pop().unwrap();
        self.merge_key = None;
        let cur = std::mem::replace(&mut self.doc, next);
        self.undo.push((label.clone(), cur));
        self.revs_undo.push(std::mem::replace(&mut self.rev, next_rev));
        Some(label)
    }

    /// The hand-written helper: run `f` with an unlimited stack, then fold or restore.
    fn one_step(&mut self, label: &str, ok: bool, f: impl FnOnce(&mut Model)) {
        let before = self.clone();
        self.limit = usize::MAX;
        f(self);
        self.limit = before.limit;
        if self.rev == before.rev && ok {
            // Nothing changed: drop any inner bookkeeping, keep the stacks as they were.
            let next = self.next;
            let saved = self.saved;
            *self = before;
            self.next = next;
            self.saved = saved;
            return;
        }
        let next = self.next;
        if ok {
            let (doc, rev, saved) = (Arc::clone(&self.doc), self.rev, self.saved);
            *self = before.clone();
            self.next = next;
            self.saved = saved;
            self.push(label, before.doc, before.rev);
            self.doc = doc;
            self.rev = rev;
        } else {
            *self = before;
            self.next = next;
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const LABELS: [&str; 3] = ["Move", "Trim", "Gain"];
const KEYS: [&str; 2] = ["drag-1", "drag-2"];

/// One random operation on the model, the store, or both. A transaction runs its inner
/// sequence on each side separately from the same seed. Commands do not undo, so inside a
/// transaction only edits, saves and nested transactions are drawn.
fn step(rng: &mut Rng, mut m: Option<&mut Model>, mut s: Option<&mut Store<Doc>>, depth: u32) {
    let label = LABELS[rng.below(3) as usize];
    let key = KEYS[rng.below(2) as usize];
    let v = rng.below(1000) as i32;
    let op = match (depth, rng.below(9)) {
        (0, op) => op,
        (1, op) => [0, 1, 2, 5, 6, 7, 0, 2, 6][op as usize],
        (_, op) => [0, 1, 2, 5][(op % 4) as usize],
    };
    match op {
        0 | 1 => {
            if let Some(m) = m.as_deref_mut() {
                m.edit(label, None, v);
            }
            if let Some(s) = s.as_deref_mut() {
                s.edit(label, |d| d.push(v));
            }
        }
        2 => {
            if let Some(m) = m.as_deref_mut() {
                m.edit(label, Some(key), v);
            }
            if let Some(s) = s.as_deref_mut() {
                s.edit_merged(label, key, |d| d.push(v));
            }
        }
        3 => {
            if let Some(m) = m.as_deref_mut() {
                m.undo();
            }
            if let Some(s) = s.as_deref_mut() {
                s.undo();
            }
        }
        4 => {
            if let Some(m) = m.as_deref_mut() {
                m.redo();
            }
            if let Some(s) = s.as_deref_mut() {
                s.redo();
            }
        }
        5 => {
            if let Some(m) = m.as_deref_mut() {
                m.saved = m.rev;
            }
            if let Some(s) = s.as_deref_mut() {
                s.mark_saved();
            }
        }
        _ => {
            let n = rng.below(5);
            let outcome = rng.below(4);
            let seed = rng.next() | 1;
            if let Some(m) = m {
                let mut r = Rng(seed);
                m.one_step(label, outcome >= 2, |m| {
                    for _ in 0..n {
                        step(&mut r, Some(&mut *m), None, depth + 1);
                    }
                });
            }
            if let Some(s) = s {
                let mut r = Rng(seed);
                let res = catch_unwind(AssertUnwindSafe(|| {
                    s.transaction::<(), ()>(label, |s| {
                        for _ in 0..n {
                            step(&mut r, None, Some(&mut *s), depth + 1);
                        }
                        match outcome {
                            0 => Err(()),
                            1 => panic!("bug"),
                            _ => Ok(()),
                        }
                    })
                }));
                assert_eq!(res.is_err(), outcome == 1);
            }
        }
    }
}

fn check(m: &Model, s: &Store<Doc>, at: &str) {
    assert_eq!(**s.doc(), *m.doc, "document {at}");
    let undo: Vec<&str> = m.undo.iter().map(|u| u.0.as_str()).collect();
    assert_eq!(s.history().undo_labels().collect::<Vec<_>>(), undo, "undo labels {at}");
    let redo: Vec<&str> = m.redo.iter().rev().map(|u| u.0.as_str()).collect();
    assert_eq!(s.history().redo_labels().collect::<Vec<_>>(), redo, "redo labels {at}");
    for (a, b) in m.undo.iter().zip(s.history().undo_entries()) {
        assert_eq!(*a.1, *b.doc, "undo states {at}");
    }
    for (a, b) in m.redo.iter().zip(s.history().redo_entries()) {
        assert_eq!(*a.1, *b.doc, "redo states {at}");
    }
    assert_eq!(s.history().merge_key(), m.merge_key.as_deref(), "merge key {at}");
    assert_eq!(s.is_dirty(), m.rev != m.saved, "dirty {at}");
}

#[test]
fn transactions_match_the_one_step_model() {
    std::panic::set_hook(Box::new(|_| {})); // the injected panics are expected
    for seed in 1..=300u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let limit = [0usize, 1, 2, 3, 5, 100][rng.below(6) as usize];
        let mut m = Model { limit, ..Model::default() };
        let mut s: Store<Doc> = Store::new(Vec::new());
        s.history_mut().set_max_steps(limit);
        for i in 0..200 {
            step(&mut rng, Some(&mut m), Some(&mut s), 0);
            check(&m, &s, &format!("seed {seed} step {i}"));
        }
    }
    let _ = std::panic::take_hook();
}
