#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `Store` must be a drop-in for the linear undo history apps hand-write today: two
//! `Vec<(label, Arc<Doc>)>` stacks, a step limit and a merge key, with these rules (copied from
//! such an app, verbatim in behaviour):
//!
//! - edit: push (label, old) on undo, drop the oldest beyond the limit, clear redo and the key;
//!   a failed edit changes nothing;
//! - edit_merged: merge when the key matches the last merged key *and* the last undo label is the
//!   same label; otherwise edit and remember the key;
//! - undo / redo: move between the stacks, clear the key.
//!
//! A reference model of those rules runs side by side with `Store` over random operation
//! sequences, and every observable (document, labels on both stacks, results) must match.

use std::sync::Arc;

use ashlar_store::Store;

#[derive(Default)]
struct Reference {
    project: Arc<Vec<i32>>,
    undo: Vec<(String, Arc<Vec<i32>>)>,
    redo: Vec<(String, Arc<Vec<i32>>)>,
    limit: usize,
    merge_key: Option<String>,
}

impl Reference {
    fn edit(&mut self, label: &str, f: impl FnOnce(&mut Vec<i32>) -> Result<(), ()>) -> Result<(), ()> {
        let mut p = (*self.project).clone();
        f(&mut p)?;
        let old = std::mem::replace(&mut self.project, Arc::new(p));
        self.undo.push((label.to_string(), old));
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.merge_key = None;
        Ok(())
    }

    fn edit_merged(&mut self, label: &str, key: &str, f: impl FnOnce(&mut Vec<i32>) -> Result<(), ()>) -> Result<(), ()> {
        let merge = self.merge_key.as_deref() == Some(key) && self.undo.last().is_some_and(|u| u.0 == label);
        if !merge {
            self.edit(label, f)?;
            self.merge_key = Some(key.to_string());
            return Ok(());
        }
        let mut p = (*self.project).clone();
        f(&mut p)?;
        self.project = Arc::new(p);
        Ok(())
    }

    fn undo(&mut self) -> Option<String> {
        let (label, prev) = self.undo.pop()?;
        self.merge_key = None;
        let cur = std::mem::replace(&mut self.project, prev);
        self.redo.push((label.clone(), cur));
        Some(label)
    }

    fn redo(&mut self) -> Option<String> {
        let (label, next) = self.redo.pop()?;
        self.merge_key = None;
        let cur = std::mem::replace(&mut self.project, next);
        self.undo.push((label.clone(), cur));
        Some(label)
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

fn check(r: &Reference, s: &Store<Vec<i32>>, step: usize) {
    assert_eq!(**s.doc(), *r.project, "document at step {step}");
    let undo: Vec<&str> = r.undo.iter().map(|u| u.0.as_str()).collect();
    assert_eq!(s.history().undo_labels().collect::<Vec<_>>(), undo, "undo labels at step {step}");
    let redo: Vec<&str> = r.redo.iter().rev().map(|u| u.0.as_str()).collect();
    assert_eq!(s.history().redo_labels().collect::<Vec<_>>(), redo, "redo labels at step {step}");
    for (a, b) in r.undo.iter().zip(s.history().undo_entries()) {
        assert_eq!(*a.1, *b.doc, "undo states at step {step}");
    }
    for (a, b) in r.redo.iter().zip(s.history().redo_entries()) {
        assert_eq!(*a.1, *b.doc, "redo states at step {step}");
    }
    assert_eq!(s.can_undo(), !r.undo.is_empty());
    assert_eq!(s.can_redo(), !r.redo.is_empty());
}

#[test]
fn store_matches_the_linear_history_rules() {
    const LABELS: [&str; 3] = ["Move", "Trim", "Gain"];
    const KEYS: [&str; 2] = ["drag-1", "drag-2"];
    for seed in 1..=200u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let limit = [0usize, 1, 2, 3, 5, 200][rng.below(6) as usize];
        let mut r = Reference { limit, ..Reference::default() };
        let mut s: Store<Vec<i32>> = Store::new(Vec::new());
        s.history_mut().set_max_steps(limit);
        for step in 0..300 {
            let label = LABELS[rng.below(3) as usize];
            let key = KEYS[rng.below(2) as usize];
            let value = rng.below(1000) as i32;
            let fail = rng.below(8) == 0;
            let f = move |d: &mut Vec<i32>| {
                d.push(value);
                if fail { Err(()) } else { Ok(()) }
            };
            match rng.below(6) {
                0 | 1 => {
                    let a = r.edit(label, f);
                    let b = s.try_edit(label, f);
                    assert_eq!(a, b);
                }
                2 | 3 => {
                    let a = r.edit_merged(label, key, f);
                    let b = s.try_edit_with(label, Some(key), |d, _| f(d));
                    assert_eq!(a, b);
                }
                4 => assert_eq!(r.undo(), s.undo(), "undo result at step {step}"),
                _ => assert_eq!(r.redo(), s.redo(), "redo result at step {step}"),
            }
            check(&r, &s, step);
        }
    }
}
