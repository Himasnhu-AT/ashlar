//! Heap allocations counted around a closure, for perf tests (IMP-1.2).
//!
//! Include it with `#[path]` and install the counting allocator in that test binary only (each
//! file in `tests/` is its own binary, so other tests keep the plain system allocator):
//!
//! ```ignore
//! #[path = "support/allocs.rs"]
//! mod allocs;
//!
//! #[global_allocator]
//! static GLOBAL: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;
//! ```
//!
//! The counters are process-wide: an allocation on any thread during the closure counts. Counting
//! per thread would need a wrapper allocator, which needs `unsafe`, so instead every test in such a
//! binary holds [`serial`] for its whole body: the tests run one at a time even under the default
//! parallel test threads, and [`count`] panics when its thread does not hold the lock. A test that
//! does not count still takes it, or its allocations land in another test's count. The test harness
//! itself may still allocate between tests (printing a result, starting the next thread), which is
//! outside any test body and so outside any measured region.

use std::cell::Cell;
use std::hint::black_box;
use std::sync::{Mutex, MutexGuard, PoisonError};

use stats_alloc::{INSTRUMENTED_SYSTEM, Region};

static SERIAL: Mutex<()> = Mutex::new(());

thread_local! {
    static HELD: Cell<bool> = const { Cell::new(false) };
}

/// Holds the process-wide test lock until dropped. See the module docs.
pub struct Serial(#[allow(dead_code)] MutexGuard<'static, ()>);

impl Drop for Serial {
    fn drop(&mut self) {
        HELD.with(|held| held.set(false));
    }
}

/// Take the lock that keeps tests in this binary from running at the same time. Hold it for the
/// whole test: `let _serial = allocs::serial();`. A test that panicked while holding it does not
/// block the others.
#[must_use = "the lock is released when the guard drops"]
pub fn serial() -> Serial {
    let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    HELD.with(|held| held.set(true));
    Serial(guard)
}

/// What a closure allocated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Allocs {
    /// Calls to `alloc` and `realloc`.
    pub count: usize,
    /// Bytes handed out by `alloc`, plus the growth of `realloc`s.
    pub bytes: usize,
}

/// Run `f` and count the heap allocations made meanwhile.
///
/// Panics if the counting allocator is not installed, so a forgotten `#[global_allocator]` can't
/// pass a gate with zeros.
pub fn count<R>(f: impl FnOnce() -> R) -> (R, Allocs) {
    assert!(HELD.with(Cell::get), "allocs::count needs `let _serial = allocs::serial();` at the top of the test");
    assert_installed();
    let region = Region::new(&INSTRUMENTED_SYSTEM);
    let r = f();
    let s = region.change();
    let grown = usize::try_from(s.bytes_reallocated).unwrap_or(0);
    (r, Allocs { count: s.allocations + s.reallocations, bytes: s.bytes_allocated + grown })
}

fn assert_installed() {
    let region = Region::new(&INSTRUMENTED_SYSTEM);
    black_box(Vec::<u8>::with_capacity(black_box(16)));
    assert!(
        region.change().allocations > 0,
        "allocs::count needs `#[global_allocator] static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;` in the test binary"
    );
}
