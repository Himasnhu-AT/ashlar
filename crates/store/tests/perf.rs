#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Performance checks (ignored by default). Run in release:
//!
//! ```sh
//! cargo test -p ashlar-store --release --test perf -- --ignored --nocapture --test-threads=1
//! ```
//!
//! A 100k-element document stored as `Arc`'d chunks is compared with the same data in one flat
//! `Vec`: with chunks an edit copies the chunk it touches plus the chunk table, so its cost does
//! not grow with the document.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use ashlar_store::{SnapshotCell, Store, Tracked};

const N: usize = 100_000;
const CHUNK: usize = 1024;

/// A document whose elements live in shared chunks: cloning it copies `N / CHUNK` pointers.
#[derive(Clone)]
struct Chunked {
    chunks: Vec<Tracked<Arc<Vec<u64>>>>,
}

impl Chunked {
    fn new() -> Chunked {
        Chunked { chunks: (0..N.div_ceil(CHUNK)).map(|c| Tracked::new(Arc::new((0..CHUNK).map(|i| (c * CHUNK + i) as u64).collect()))).collect() }
    }
    fn set(&mut self, i: usize, v: u64) {
        if let Some(c) = self.chunks.get_mut(i / CHUNK) {
            c.make_mut()[i % CHUNK] = v;
        }
    }
}

#[derive(Clone)]
struct Flat {
    items: Vec<u64>,
}

fn time<F: FnMut(usize)>(iters: usize, mut f: F) -> f64 {
    let t = Instant::now();
    for i in 0..iters {
        f(i);
    }
    t.elapsed().as_secs_f64() * 1e9 / iters as f64
}

#[test]
#[ignore]
fn perf_edit_100k_structural_sharing() {
    let iters = 2000;
    let mut chunked: Store<Chunked> = Store::new(Chunked::new());
    chunked.history_mut().set_max_steps(100);
    let ns_chunked = time(iters, |i| chunked.edit("set", |d| d.set((i * 7919) % N, i as u64)));

    let mut flat: Store<Flat> = Store::new(Flat { items: (0..N as u64).collect() });
    flat.history_mut().set_max_steps(100);
    let ns_flat = time(iters, |i| flat.edit("set", |d| d.items[(i * 7919) % N] = i as u64));

    // Only the touched chunk changed revision: views keyed by chunk revision redraw one chunk.
    let before: Vec<_> = chunked.doc().chunks.iter().map(|c| c.revision()).collect();
    chunked.edit("one", |d| d.set(5, 1));
    let changed = chunked.doc().chunks.iter().zip(&before).filter(|(c, r)| c.revision() != **r).count();

    println!(
        "edit, 100k elements: chunked {:.0} ns/edit, flat {:.0} ns/edit ({:.1}x); chunks changed by one edit: {changed}",
        ns_chunked,
        ns_flat,
        ns_flat / ns_chunked
    );
    println!("bench: edit_100k_chunked {ns_chunked:.0} ns");
    println!("bench: edit_100k_flat {ns_flat:.0} ns");
    assert_eq!(changed, 1);
    assert!(ns_chunked < ns_flat, "structural sharing must beat a full copy");
}

#[test]
#[ignore]
fn perf_undo_redo() {
    let mut s: Store<Chunked> = Store::new(Chunked::new());
    s.history_mut().set_max_steps(1000);
    for i in 0..1000 {
        s.edit("set", |d| d.set(i * 97 % N, 1));
    }
    let iters = 100_000;
    let ns = time(iters, |i| {
        if (i / 1000) % 2 == 0 {
            black_box(s.undo());
        } else {
            black_box(s.redo());
        }
    });
    println!("undo/redo (1000-step history, 100k-element doc): {ns:.0} ns/op");
    println!("bench: undo_redo {ns:.0} ns");
}

#[test]
#[ignore]
fn perf_snapshot_publish_and_read() {
    let doc = Arc::new(Chunked::new());
    let mut s: Store<Chunked> = Store::new(Chunked::new());
    let cell = s.subscribe_snapshots();
    let iters = 1_000_000;
    let c2 = SnapshotCell::new(doc.clone(), s.revision());
    let ns_publish = time(iters, |i| c2.publish(doc.clone(), ashlar_store::Revision(i as u64)));
    let ns_read = time(iters, |_| {
        black_box(cell.read(|p| p.revision));
    });
    let ns_load = time(iters, |_| {
        black_box(cell.load());
    });
    let ns_edit_published = time(2000, |i| s.edit("set", |d| d.set(i, 2)));
    println!(
        "snapshot cell: publish {ns_publish:.0} ns, read in place {ns_read:.1} ns, load (Arc) {ns_load:.1} ns; edit with a subscriber {ns_edit_published:.0} ns"
    );
    println!("bench: snapshot_publish {ns_publish:.0} ns");
    println!("bench: snapshot_read {ns_read:.1} ns");
    println!("bench: snapshot_load {ns_load:.1} ns");
    println!("bench: edit_with_subscriber {ns_edit_published:.0} ns");
}

#[test]
#[ignore]
fn perf_undo_redo_10k_steps() {
    let mut s: Store<Vec<u64>> = Store::new(Vec::new());
    s.history_mut().set_max_steps(10_000);
    for i in 0..10_000u64 {
        s.edit("push", |d| d.push(i));
    }
    let ops = 10_000;
    let ns = time(ops * 2, |i| {
        if i < ops {
            black_box(s.undo());
        } else {
            black_box(s.redo());
        }
    });
    assert_eq!(s.doc().len(), 10_000);
    println!("undo/redo walking a 10k-step history: {ns:.0} ns/op");
    println!("bench: undo_redo_10k_steps {ns:.0} ns");
}
