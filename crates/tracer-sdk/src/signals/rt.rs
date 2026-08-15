//! Counters and gauges that are safe on a realtime thread: recording is one atomic store, with no lock,
//! no allocation and no system call. Register them up front, off the realtime thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A monotonically increasing count, reported per performance window.
#[derive(Debug, Clone)]
pub struct Counter(pub(crate) Arc<AtomicU64>);

impl Counter {
    pub(crate) fn detached() -> Self {
        Self(Arc::new(AtomicU64::new(0)))
    }

    #[inline]
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    #[inline]
    pub fn inc(&self) {
        self.add(1);
    }
}

/// A last-value-wins measurement, reported per performance window.
#[derive(Debug, Clone)]
pub struct Gauge(pub(crate) Arc<AtomicU64>);

impl Gauge {
    pub(crate) fn detached() -> Self {
        Self(Arc::new(AtomicU64::new(f64::NAN.to_bits())))
    }

    #[inline]
    pub fn set(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }
}
