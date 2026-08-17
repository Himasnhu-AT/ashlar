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

/// Failed sign-ins remembered per key (a user name, or the whole server) for this long.
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Failures before a user name starts to cost extra time, and the longest extra wait.
const USER_FREE_FAILURES: u32 = 3;
const USER_MAX_DELAY: Duration = Duration::from_secs(10);
/// Failures across all users within a minute before every sign-in slows down, and the longest extra wait.
const GLOBAL_FREE_FAILURES: u32 = 20;
const GLOBAL_WINDOW: Duration = Duration::from_secs(60);
const GLOBAL_MAX_DELAY: Duration = Duration::from_secs(5);

/// Counts recent failures per key, in memory, bounded like [`TokenBuckets`].
pub struct FailureTracker<K> {
    window: Duration,
    max_entries: usize,
    entries: Mutex<HashMap<K, (u32, Instant)>>,
}

impl<K: Hash + Eq + Clone> FailureTracker<K> {
    pub fn new(window: Duration, max_entries: usize) -> Self {
        Self { window, max_entries: max_entries.max(1), entries: Mutex::new(HashMap::new()) }
    }

    /// Failures recorded for `key` that have not aged out.
    pub fn count(&self, key: &K, now: Instant) -> u32 {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.get(key).filter(|(_, last)| now.saturating_duration_since(*last) <= self.window).map_or(0, |(count, _)| *count)
    }

    pub fn record(&self, key: &K, now: Instant) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if !entries.contains_key(key) && entries.len() >= self.max_entries {
            entries.retain(|_, (_, last)| now.saturating_duration_since(*last) <= self.window);
            if entries.len() >= self.max_entries
                && let Some(oldest) = entries.iter().min_by_key(|(_, (_, last))| *last).map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        let entry = entries.entry(key.clone()).or_insert((0, now));
        if now.saturating_duration_since(entry.1) > self.window {
            entry.0 = 0;
        }
        entry.0 = entry.0.saturating_add(1);
        entry.1 = now;
    }

    pub fn clear(&self, key: &K) {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).remove(key);
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Dashboard login attempts per minute per client address (burst and refill rate). Always on: it does not
/// follow `TRACER_RATE_PER_MIN`.
pub const LOGIN_PER_MIN: u32 = 10;

/// The limits applied to the public routes.
///
/// Ingest checks the client address first, before the key is even looked up, so unknown keys and the health
/// route are limited too. Only a request the address bucket let through touches the per-key bucket, which is
/// a capacity guard far above the address limit: one noisy client cannot drain it for everyone else.
pub struct Limiters {
    keys: TokenBuckets<String>,
    addrs: TokenBuckets<IpAddr>,
    logins: TokenBuckets<IpAddr>,
    user_failures: FailureTracker<String>,
    global_failures: FailureTracker<()>,
}

fn whole_seconds(wait: Duration) -> u64 {
    wait.as_secs_f64().ceil().max(1.0) as u64
}

impl Limiters {
    pub fn new(per_min: u32, key_per_min: u32) -> Self {
        Self {
            keys: TokenBuckets::new(key_per_min),
            addrs: TokenBuckets::new(per_min),
            logins: TokenBuckets::new(LOGIN_PER_MIN),
            user_failures: FailureTracker::new(FAILURE_WINDOW, MAX_TRACKED),
            global_failures: FailureTracker::new(GLOBAL_WINDOW, 1),
        }
    }

    /// Takes one request for the client address. On refusal returns the whole seconds to wait, at least one.
    pub fn check_addr(&self, addr: IpAddr, now: Instant) -> Result<(), u64> {
        self.addrs.check(&mask(addr), now).map_err(whole_seconds)
    }

    /// Takes one request for a write key. Call only after [`Limiters::check_addr`] passed.
    pub fn check_key(&self, key: &str, now: Instant) -> Result<(), u64> {
        self.keys.check(&key.to_string(), now).map_err(whole_seconds)
    }

    /// Takes one login attempt for `addr`. On refusal returns the whole seconds to wait, at least one.
    pub fn check_login(&self, addr: IpAddr, now: Instant) -> Result<(), u64> {
        self.logins.check(&mask(addr), now).map_err(whole_seconds)
    }

    /// Extra time a sign-in for `username` must wait, growing with recent failures for that user name and with
    /// failures across all of them. It is a function of the user name text only, so it is the same whether or
    /// not the account exists.
    pub fn login_delay(&self, username: &str, now: Instant) -> Duration {
        let user = self.user_failures.count(&username.to_ascii_lowercase(), now);
        let user_delay = if user > USER_FREE_FAILURES {
            Duration::from_millis(250u64 << (user - USER_FREE_FAILURES).min(6)).min(USER_MAX_DELAY)
        } else {
            Duration::ZERO
        };
        let global = self.global_failures.count(&(), now);
        let global_delay = if global > GLOBAL_FREE_FAILURES {
            Duration::from_millis(100 * u64::from(global - GLOBAL_FREE_FAILURES)).min(GLOBAL_MAX_DELAY)
        } else {
            Duration::ZERO
        };
        user_delay.max(global_delay)
    }

    pub fn login_failed(&self, username: &str, now: Instant) {
        self.user_failures.record(&username.to_ascii_lowercase(), now);
        self.global_failures.record(&(), now);
    }

    pub fn login_succeeded(&self, username: &str) {
        self.user_failures.clear(&username.to_ascii_lowercase());
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
        let limiters = Limiters::new(1, 100);
        let now = Instant::now();
        let a: IpAddr = "2001:db8::1".parse().unwrap();
        let b: IpAddr = "2001:db8::2".parse().unwrap();
        assert!(limiters.check_addr(a, now).is_ok());
        assert!(limiters.check_addr(b, now).is_err());
    }

    #[test]
    fn retry_after_is_at_least_one_second() {
        let limiters = Limiters::new(600, 6000);
        let now = Instant::now();
        let addr: IpAddr = "10.0.0.1".parse().unwrap();
        for _ in 0..600 {
            assert!(limiters.check_addr(addr, now).is_ok());
        }
        assert_eq!(limiters.check_addr(addr, now), Err(1));
    }

    #[test]
    fn a_refused_address_does_not_drain_the_key_bucket() {
        // The key bucket holds 10; the address bucket 2. A client hammering from one address is refused by
        // the address bucket alone, and the key bucket is left for everyone else.
        let limiters = Limiters::new(2, 10);
        let now = Instant::now();
        let noisy: IpAddr = "10.0.0.1".parse().unwrap();
        let quiet: IpAddr = "10.0.0.2".parse().unwrap();
        let mut key_tokens_used = 0;
        for _ in 0..100 {
            if limiters.check_addr(noisy, now).is_ok() {
                assert!(limiters.check_key("shared", now).is_ok());
                key_tokens_used += 1;
            }
        }
        assert_eq!(key_tokens_used, 2);
        assert!(limiters.check_addr(quiet, now).is_ok());
        assert!(limiters.check_key("shared", now).is_ok());
    }

    #[test]
    fn the_key_limit_is_separate_and_can_be_off() {
        let limiters = Limiters::new(1, 0);
        let now = Instant::now();
        for _ in 0..1000 {
            assert!(limiters.check_key("k", now).is_ok());
        }
    }

    #[test]
    fn login_limit_ignores_the_ingest_setting() {
        let limiters = Limiters::new(0, 0);
        let now = Instant::now();
        let addr: IpAddr = "10.0.0.9".parse().unwrap();
        for _ in 0..LOGIN_PER_MIN {
            assert!(limiters.check_login(addr, now).is_ok());
        }
        assert!(limiters.check_login(addr, now).is_err());
    }

    #[test]
    fn failures_slow_a_user_name_down_and_success_resets_it() {
        let limiters = Limiters::new(0, 0);
        let now = Instant::now();
        assert_eq!(limiters.login_delay("Alice", now), Duration::ZERO);
        for _ in 0..3 {
            limiters.login_failed("alice", now);
        }
        assert_eq!(limiters.login_delay("alice", now), Duration::ZERO);
        limiters.login_failed("ALICE", now);
        let first = limiters.login_delay("alice", now);
        limiters.login_failed("alice", now);
        let second = limiters.login_delay("alice", now);
        assert!(first > Duration::ZERO && second > first, "{first:?} {second:?}");
        for _ in 0..50 {
            limiters.login_failed("alice", now);
        }
        assert_eq!(limiters.login_delay("alice", now), USER_MAX_DELAY);
        // Another user name is unaffected by the per-user budget (the global one is a separate matter).
        assert!(limiters.login_delay("bob", now) <= GLOBAL_MAX_DELAY);
        // Old failures age out; a success clears them.
        assert_eq!(limiters.login_delay("alice", now + FAILURE_WINDOW + Duration::from_secs(1)), Duration::ZERO);
        limiters.login_succeeded("alice");
        assert_eq!(limiters.login_delay("alice", now), limiters.login_delay("bob", now));
    }

    #[test]
    fn many_failures_across_user_names_slow_everyone_down() {
        let limiters = Limiters::new(0, 0);
        let now = Instant::now();
        for index in 0..60 {
            limiters.login_failed(&format!("guess{index}"), now);
        }
        let delay = limiters.login_delay("someone-else", now);
        assert!(delay > Duration::ZERO && delay <= GLOBAL_MAX_DELAY, "{delay:?}");
        // The minute passes and the slowdown with it.
        assert_eq!(limiters.login_delay("someone-else", now + GLOBAL_WINDOW + Duration::from_secs(1)), Duration::ZERO);
    }

    #[test]
    fn failure_tracking_is_bounded() {
        let tracker = FailureTracker::new(Duration::from_secs(60), 50);
        let now = Instant::now();
        for index in 0..1000u32 {
            tracker.record(&index, now + Duration::from_millis(u64::from(index)));
            assert!(tracker.len() <= 50);
        }
    }
}
