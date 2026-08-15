//! Random ids and the install record.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tracer_proto::UnixMs;

use super::files::{now_ms, read_json, write_json};

pub(super) const INSTALL_FILE: &str = "install.json";

/// Most old install ids kept waiting for a forget request to get through.
const MAX_PENDING_FORGET: usize = 16;

static FALLBACK_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Content of `install.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Identity {
    pub install_id: String,
    #[serde(default)]
    pub created: UnixMs,
    /// Earlier install ids the server has not yet confirmed it deleted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_forget: Vec<String>,
}

impl Identity {
    /// Reads the install record without creating one. `None` when there is no usable record.
    pub(super) fn load(dir: &Path) -> Option<Self> {
        read_json::<Self>(&dir.join(INSTALL_FILE)).filter(|identity| is_id(&identity.install_id))
    }

    /// Reads the install record, creating and saving a new one when there is none.
    pub(super) fn load_or_create(dir: &Path) -> Self {
        Self::load(dir).unwrap_or_else(|| {
            let identity = Self { install_id: new_id(), created: now_ms(), pending_forget: Vec::new() };
            identity.save(dir);
            identity
        })
    }

    pub(super) fn save(&self, dir: &Path) -> bool {
        write_json(&dir.join(INSTALL_FILE), self)
    }

    /// Replaces the install id with a fresh one and returns the old one. With `remember`, the old id is
    /// kept until a forget request for it has been delivered.
    pub(super) fn rotate(&mut self, remember: bool) -> String {
        let old = std::mem::replace(&mut self.install_id, new_id());
        self.created = now_ms();
        if remember && is_id(&old) && !self.pending_forget.contains(&old) {
            if self.pending_forget.len() >= MAX_PENDING_FORGET {
                self.pending_forget.remove(0);
            }
            self.pending_forget.push(old.clone());
        }
        old
    }
}

/// True for 32 lower-case hex characters.
pub(super) fn is_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A new install, session or item id: 16 random bytes as 32 lower-case hex characters.
pub(super) fn new_id() -> String {
    hex(&random_bytes())
}

/// A random number for retry jitter.
pub(super) fn random_u64() -> u64 {
    let [a, b, c, d, e, f, g, h, ..] = random_bytes();
    u64::from_le_bytes([a, b, c, d, e, f, g, h])
}

fn random_bytes() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    if !fill_random(&mut bytes) {
        bytes = fallback_bytes();
    }
    bytes
}

#[cfg(not(target_arch = "wasm32"))]
fn fill_random(bytes: &mut [u8; 16]) -> bool {
    getrandom::fill(bytes).is_ok()
}

#[cfg(target_arch = "wasm32")]
fn fill_random(_bytes: &mut [u8; 16]) -> bool {
    false
}

/// Used when the system random source fails: time, process id and a counter, mixed. Not unpredictable,
/// but unique enough to tell items apart.
fn fallback_bytes() -> [u8; 16] {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_nanos()).unwrap_or(0);
    let counter = FALLBACK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let seed = (nanos as u64) ^ ((nanos >> 64) as u64) ^ (u64::from(std::process::id()) << 32) ^ counter.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let high = mix(seed);
    let low = mix(high ^ counter.wrapping_add(0x2545_f491_4f6c_dd1d));
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(high.to_le_bytes().into_iter().chain(low.to_le_bytes())) {
        *slot = byte;
    }
    bytes
}

/// The SplitMix64 finaliser.
fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            text.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::files::testing::TempDir;
    use super::*;

    #[test]
    fn ids_are_32_hex_and_distinct() {
        let ids: HashSet<String> = (0..500).map(|_| new_id()).collect();
        assert_eq!(ids.len(), 500);
        assert!(ids.iter().all(|id| is_id(id)));
    }

    #[test]
    fn fallback_ids_are_distinct() {
        let ids: HashSet<String> = (0..500).map(|_| hex(&fallback_bytes())).collect();
        assert_eq!(ids.len(), 500);
        assert!(ids.iter().all(|id| is_id(id)));
    }

    #[test]
    fn install_record_is_created_once_and_reused() {
        let dir = TempDir::new("identity");
        assert!(Identity::load(dir.path()).is_none());
        let first = Identity::load_or_create(dir.path());
        let second = Identity::load_or_create(dir.path());
        assert_eq!(first, second);
        assert!(is_id(&first.install_id));
    }

    #[test]
    fn damaged_install_record_is_replaced() {
        let dir = TempDir::new("identity");
        std::fs::write(dir.path().join(INSTALL_FILE), br#"{"install_id":"nope"}"#).unwrap();
        assert!(is_id(&Identity::load_or_create(dir.path()).install_id));
    }

    #[test]
    fn rotate_remembers_the_old_id_within_a_bound() {
        let mut identity = Identity { install_id: new_id(), created: 1, pending_forget: Vec::new() };
        let first = identity.install_id.clone();
        assert_eq!(identity.rotate(true), first);
        assert_ne!(identity.install_id, first);
        assert_eq!(identity.pending_forget, vec![first]);
        identity.rotate(false);
        assert_eq!(identity.pending_forget.len(), 1);
        for _ in 0..40 {
            identity.rotate(true);
        }
        assert_eq!(identity.pending_forget.len(), MAX_PENDING_FORGET);
    }
}
