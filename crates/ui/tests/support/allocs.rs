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
//! The counters are process-wide: allocations on other threads during the closure count too, so
//! run perf tests with `--test-threads=1`.

use std::hint::black_box;

use stats_alloc::{INSTRUMENTED_SYSTEM, Region};

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
