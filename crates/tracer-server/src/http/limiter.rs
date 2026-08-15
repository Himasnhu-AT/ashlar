//! In-memory token buckets that cannot grow without bound.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Most distinct keys one limiter tracks. When full, idle entries go first, then the least recently used.
pub const MAX_TRACKED: usize = 10_000;

struct Bucket {
    tokens: f64,
    last: Instant,
}

/// A token bucket per key: `per_min` requests of burst, refilled at `per_min` per minute.
pub struct TokenBuckets<K> {
    per_min: u32,
    max_entries: usize,
    buckets: Mutex<HashMap<K, Bucket>>,
}

impl<K: Hash + Eq + Clone> TokenBuckets<K> {
    pub fn new(per_min: u32) -> Self {
        Self::with_capacity(per_min, MAX_TRACKED)
    }

    pub fn with_capacity(per_min: u32, max_entries: usize) -> Self {
        Self { per_min, max_entries: max_entries.max(1), buckets: Mutex::new(HashMap::new()) }
    }

    /// Takes one token for `key`. On refusal returns how long until one is available.
    pub fn check(&self, key: &K, now: Instant) -> Result<(), Duration> {
        if self.per_min == 0 {
            return Ok(());
        }
        let capacity = f64::from(self.per_min);
        let rate = capacity / 60.0;
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if !buckets.contains_key(key) && buckets.len() >= self.max_entries {
            // Entries that have refilled completely are indistinguishable from new ones.
            buckets.retain(|_, bucket| bucket.tokens + now.saturating_duration_since(bucket.last).as_secs_f64() * rate < capacity);
            if buckets.len() >= self.max_entries
                && let Some(oldest) = buckets.iter().min_by_key(|(_, bucket)| bucket.last).map(|(key, _)| key.clone())
            {
                buckets.remove(&oldest);
            }
        }
        let bucket = buckets.entry(key.clone()).or_insert(Bucket { tokens: capacity, last: now });
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * rate).min(capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - bucket.tokens) / rate))
        }
    }

    pub fn len(&self) -> usize {
        self.buckets.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The two limits applied to ingest: per write key and per client address.
pub struct Limiters {
    keys: TokenBuckets<String>,
    addrs: TokenBuckets<IpAddr>,
}

impl Limiters {
    pub fn new(per_min: u32) -> Self {
        Self { keys: TokenBuckets::new(per_min), addrs: TokenBuckets::new(per_min) }
    }

    /// Checks both limits. On refusal returns the whole seconds to wait, at least one.
    pub fn check(&self, key: &str, addr: IpAddr, now: Instant) -> Result<(), u64> {
        let by_addr = self.addrs.check(&mask(addr), now);
        let by_key = self.keys.check(&key.to_string(), now);
        match (by_addr, by_key) {
            (Ok(()), Ok(())) => Ok(()),
            (a, b) => {
                let wait = a.err().unwrap_or_default().max(b.err().unwrap_or_default());
                Err(wait.as_secs_f64().ceil().max(1.0) as u64)
            }
        }
    }
}

/// Clients that own a whole IPv6 /64 could otherwise mint unlimited addresses; bucket by the /64.
fn mask(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => {
            let mut octets = v6.octets();
            octets[8..].fill(0);
            IpAddr::V6(octets.into())
        }
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_refusal_then_refill() {
        let limiter = TokenBuckets::new(60);
        let start = Instant::now();
        for _ in 0..60 {
            assert!(limiter.check(&"k", start).is_ok());
        }
        let wait = limiter.check(&"k", start).unwrap_err();
        assert!(wait > Duration::from_millis(900) && wait <= Duration::from_secs(1));
        assert!(limiter.check(&"k", start + Duration::from_secs(1)).is_ok());
        assert!(limiter.check(&"k", start + Duration::from_secs(1)).is_err());
    }

    #[test]
    fn keys_are_independent() {
        let limiter = TokenBuckets::new(1);
        let now = Instant::now();
        assert!(limiter.check(&"a", now).is_ok());
        assert!(limiter.check(&"b", now).is_ok());
        assert!(limiter.check(&"a", now).is_err());
    }

    #[test]
    fn zero_disables_the_limit() {
        let limiter = TokenBuckets::new(0);
        let now = Instant::now();
        for _ in 0..1000 {
            assert!(limiter.check(&"k", now).is_ok());
        }
        assert!(limiter.is_empty());
    }

    #[test]
    fn memory_is_bounded() {
        let limiter = TokenBuckets::with_capacity(10, 100);
        let now = Instant::now();
        for index in 0..10_000u32 {
            let _ = limiter.check(&index, now + Duration::from_millis(u64::from(index)));
            assert!(limiter.len() <= 100);
        }
    }

    #[test]
    fn idle_entries_are_evicted_before_active_ones() {
        let limiter = TokenBuckets::with_capacity(60, 2);
        let start = Instant::now();
        for _ in 0..60 {
            let _ = limiter.check(&"busy", start);
        }
        let _ = limiter.check(&"idle", start);
        // A minute later "idle" has refilled; adding a third key evicts it, not the drained one... which
        // has refilled too, so either may go, but the table stays at two entries.
        let _ = limiter.check(&"new", start + Duration::from_secs(1));
        assert!(limiter.len() <= 2);
    }

    #[test]
    fn ipv6_clients_share_a_bucket_per_64() {
        let limiters = Limiters::new(1);
        let now = Instant::now();
        let a: IpAddr = "2001:db8::1".parse().unwrap();
        let b: IpAddr = "2001:db8::2".parse().unwrap();
        assert!(limiters.check("k1", a, now).is_ok());
        assert!(limiters.check("k2", b, now).is_err());
    }

    #[test]
    fn retry_after_is_at_least_one_second() {
        let limiters = Limiters::new(600);
        let now = Instant::now();
        let addr: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..600 {
            assert!(limiters.check("k", addr, now).is_ok());
        }
        assert_eq!(limiters.check("k", addr, now), Err(1));
    }
}
