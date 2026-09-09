//! Counters and gauges that are safe on a realtime thread: recording is one atomic store, with no lock,
//! no allocation and no system call. Register them up front, off the realtime thread.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// Most counters plus gauges one process can register. Further registrations get a working handle that
/// is never reported.
const MAX_REGISTERED: usize = 256;

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

/// Every counter and gauge registered by name. Registration locks and allocates; recording does neither.
#[derive(Default)]
pub(crate) struct Registry {
    inner: Mutex<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    /// Name, handle, and the value read at the end of the previous window.
    counters: Vec<(&'static str, Counter, u64)>,
    gauges: Vec<(&'static str, Gauge)>,
}

impl Registry {
    /// The counter registered under `name`, created on first use. The same name always gives the same counter.
    pub(crate) fn counter(&self, name: &'static str) -> Counter {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, counter, _)) = inner.counters.iter().find(|(existing, _, _)| *existing == name) {
            return counter.clone();
        }
        let counter = Counter::detached();
        if inner.counters.len() + inner.gauges.len() < MAX_REGISTERED {
            inner.counters.push((name, counter.clone(), 0));
        }
        counter
    }

    /// The gauge registered under `name`, created on first use.
    pub(crate) fn gauge(&self, name: &'static str) -> Gauge {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((_, gauge)) = inner.gauges.iter().find(|(existing, _)| *existing == name) {
            return gauge.clone();
        }
        let gauge = Gauge::detached();
        if inner.counters.len() + inner.gauges.len() < MAX_REGISTERED {
            inner.gauges.push((name, gauge.clone()));
        }
        gauge
    }

    /// Reads what happened since the last call: counters as increases (zero increases are left out) and
    /// gauges that were set, which then read as unset until set again.
    pub(crate) fn drain(&self) -> (BTreeMap<String, u64>, BTreeMap<String, f64>) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let mut counters = BTreeMap::new();
        for (name, counter, last) in &mut inner.counters {
            let now = counter.0.load(Ordering::Relaxed);
            let delta = now.saturating_sub(*last);
            *last = now;
            if delta > 0 {
                counters.insert((*name).to_owned(), delta);
            }
        }
        let mut gauges = BTreeMap::new();
        for (name, gauge) in &inner.gauges {
            let value = f64::from_bits(gauge.0.swap(f64::NAN.to_bits(), Ordering::Relaxed));
            if value.is_finite() {
                gauges.insert((*name).to_owned(), value);
            }
        }
        (counters, gauges)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_name_same_handle() {
        let registry = Registry::default();
        let a = registry.counter("x");
        let b = registry.counter("x");
        a.inc();
        b.add(2);
        assert_eq!(registry.drain().0.get("x"), Some(&3));
        let g1 = registry.gauge("g");
        let g2 = registry.gauge("g");
        g1.set(1.0);
        g2.set(2.5);
        assert_eq!(registry.drain().1.get("g"), Some(&2.5));
    }

    #[test]
    fn counters_drain_as_increases() {
        let registry = Registry::default();
        let counter = registry.counter("n");
        counter.add(5);
        assert_eq!(registry.drain().0.get("n"), Some(&5));
        assert!(registry.drain().0.is_empty());
        counter.add(2);
        assert_eq!(registry.drain().0.get("n"), Some(&2));
    }

    #[test]
    fn gauges_report_only_when_set() {
        let registry = Registry::default();
        let gauge = registry.gauge("level");
        assert!(registry.drain().1.is_empty());
        gauge.set(0.75);
        assert_eq!(registry.drain().1.get("level"), Some(&0.75));
        assert!(registry.drain().1.is_empty());
        gauge.set(f64::INFINITY);
        assert!(registry.drain().1.is_empty());
    }

    #[test]
    fn counter_and_gauge_may_share_a_name() {
        let registry = Registry::default();
        registry.counter("same").inc();
        registry.gauge("same").set(1.0);
        let (counters, gauges) = registry.drain();
        assert_eq!(counters.len(), 1);
        assert_eq!(gauges.len(), 1);
    }

    #[test]
    fn registration_is_bounded_but_handles_still_work() {
        let registry = Registry::default();
        for index in 0..MAX_REGISTERED + 10 {
            let name: &'static str = Box::leak(format!("c{index}").into_boxed_str());
            registry.counter(name).inc();
        }
        assert_eq!(registry.drain().0.len(), MAX_REGISTERED);
    }

    #[test]
    fn recording_through_a_clone_is_one_shared_atomic() {
        let counter = Counter::detached();
        let clone = counter.clone();
        let strong_before = Arc::strong_count(&counter.0);
        for _ in 0..1000 {
            clone.inc();
        }
        assert_eq!(counter.0.load(Ordering::Relaxed), 1000);
        assert_eq!(Arc::strong_count(&counter.0), strong_before);
    }
}
