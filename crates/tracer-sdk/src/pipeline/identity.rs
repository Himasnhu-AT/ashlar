//! Random ids and the install record.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracer_proto::UnixMs;

use super::files::{now_ms, read_json, write_json};

pub(super) const INSTALL_FILE: &str = "install.json";

/// Locked for the moment it takes to change `install.json`, so two processes do not undo each other.
const INSTALL_LOCK_FILE: &str = "install.lock";

/// How often to look again at a file another process is in the middle of creating or changing.
const BUSY_ATTEMPTS: u32 = 200;

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

    /// Reads the install record, creating one when there is none. Processes that start at the same time
    /// on a fresh directory all end up with the same record: only one of them can create the file, and
    /// the others read what it wrote.
    pub(super) fn load_or_create(dir: &Path) -> Self {
        let path = dir.join(INSTALL_FILE);
        for _ in 0..BUSY_ATTEMPTS {
            if let Some(identity) = Self::load(dir) {
                return identity;
            }
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    let identity = Self { install_id: new_id(), created: now_ms(), pending_forget: Vec::new() };
                    let written = serde_json::to_vec(&identity).is_ok_and(|json| file.write_all(&json).is_ok());
                    if written {
                        return identity;
                    }
                    break;
                }
                // Another process is writing it right now, or it is damaged; look again shortly.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => std::thread::sleep(Duration::from_millis(2)),
                Err(_) => break,
            }
        }
        // A record that stays unreadable is replaced.
        let identity = Self { install_id: new_id(), created: now_ms(), pending_forget: Vec::new() };
        identity.save(dir);
        identity
    }

    /// Changes the record on disk: reads it, applies `change`, saves it, and returns the result. A lock
    /// file keeps two processes from doing this at the same moment. `current` is the starting point when
    /// the file cannot be read.
    pub(super) fn update(dir: &Path, current: &Self, change: impl FnOnce(&mut Self)) -> Self {
        let _lock = lock_install(dir);
        let mut identity = Self::load(dir).unwrap_or_else(|| current.clone());
        change(&mut identity);
        identity.save(dir);
        identity
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

/// Takes the lock on `install.lock`, waiting a short while for another process to let go. `None` when it
/// cannot be had, in which case the caller goes ahead without it: a rare lost update is better than a
/// host thread that hangs.
fn lock_install(dir: &Path) -> Option<File> {
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(INSTALL_LOCK_FILE)).ok()?;
    for _ in 0..BUSY_ATTEMPTS {
        match file.try_lock() {
            Ok(()) => return Some(file),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
            Err(TryLockError::Error(_)) => return None,
        }
    }
    None
}

/// An id that is always the same for the same inputs. Used for the items that report an adopted session's
/// unclean end, so that if they are ever produced twice the server sees one item.
pub(super) fn derived_id(session_id: &str, purpose: &str) -> String {
    let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in session_id.bytes().chain([0]).chain(purpose.bytes()) {
        seed = (seed ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    let high = mix(seed);
    let low = mix(high ^ seed.rotate_left(32));
    hex(&high.to_le_bytes().into_iter().chain(low.to_le_bytes()).collect::<Vec<u8>>())
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
    fn processes_starting_together_agree_on_the_install_id() {
        let dir = TempDir::new("identity");
        let path = dir.path().to_owned();
        let threads: Vec<_> = (0..16)
            .map(|_| {
                std::thread::spawn({
                    let path = path.clone();
                    move || Identity::load_or_create(&path).install_id
                })
            })
            .collect();
        let ids: HashSet<String> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
        assert_eq!(ids.len(), 1, "{ids:?}");
        assert_eq!(Identity::load(dir.path()).map(|identity| identity.install_id), ids.into_iter().next());
    }

    #[test]
    fn updates_start_from_the_file_not_from_a_stale_copy() {
        let dir = TempDir::new("identity");
        let stale = Identity::load_or_create(dir.path());
        let rotated = Identity::update(dir.path(), &stale, |identity| {
            identity.rotate(true);
        });
        assert_ne!(rotated.install_id, stale.install_id);
        // Another process, still holding the old copy, removes a pending id: the rotation survives.
        let updated = Identity::update(dir.path(), &stale, |identity| identity.pending_forget.clear());
        assert_eq!(updated.install_id, rotated.install_id);
        assert!(updated.pending_forget.is_empty());
        assert_eq!(Identity::load(dir.path()), Some(updated));
    }

    #[test]
    fn derived_ids_are_stable_and_distinct() {
        let end = derived_id("0123456789abcdef0123456789abcdef", "end");
        assert!(is_id(&end));
        assert_eq!(end, derived_id("0123456789abcdef0123456789abcdef", "end"));
        assert_ne!(end, derived_id("0123456789abcdef0123456789abcdef", "crash"));
        assert_ne!(end, derived_id("1123456789abcdef0123456789abcdef", "end"));
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
