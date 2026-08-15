//! Small helpers for the SDK's own files: atomic writes, lenient reads, clocks and locks.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tracer_proto::UnixMs;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Wall-clock time in Unix milliseconds. Only for timestamps; durations use `Instant`.
pub(super) fn now_ms() -> UnixMs {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)).unwrap_or(0)
}

/// Writes to a temporary name in the same folder, then renames over `path`.
pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().map(|name| name.to_os_string()).unwrap_or_default();
    name.push(format!(".{}.{unique}.tmp", std::process::id()));
    let temp = path.with_file_name(name);
    std::fs::write(&temp, bytes)?;
    let renamed = std::fs::rename(&temp, path);
    if renamed.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    renamed
}

pub(super) fn write_json<T: Serialize>(path: &Path, value: &T) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| write_atomic(path, &bytes).is_ok())
}

/// Reads and parses a file. A missing or malformed file is `None`, never an error.
pub(super) fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Locks a mutex, carrying on with the data when another thread panicked while holding it.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Like [`lock`], but gives up after a short while instead of waiting forever. For the panic path, where
/// the panicking thread may itself hold the lock.
pub(super) fn lock_soft<T>(mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    for _ in 0..200 {
        match mutex.try_lock() {
            Ok(guard) => return Some(guard),
            Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    None
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// A folder under the system temporary directory, removed on drop.
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new(label: &str) -> Self {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("tracer-sdk-{label}-{}-{unique}-{}", std::process::id(), super::now_ms()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::TempDir;
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp_file() {
        let dir = TempDir::new("files");
        let path = dir.path().join("value.json");
        assert!(write_json(&path, &1u32));
        assert!(write_json(&path, &2u32));
        assert_eq!(read_json::<u32>(&path), Some(2));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn malformed_file_reads_as_absent() {
        let dir = TempDir::new("files");
        let path = dir.path().join("value.json");
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(read_json::<u32>(&path), None);
        assert_eq!(read_json::<u32>(&dir.path().join("missing.json")), None);
    }

    #[test]
    fn soft_lock_survives_poison_and_gives_up_when_held() {
        let mutex = std::sync::Arc::new(Mutex::new(5));
        let held = lock(&mutex);
        assert!(lock_soft(&mutex).is_none());
        drop(held);
        let poisoner = mutex.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poisoner.lock().unwrap();
            panic!("poison the lock");
        })
        .join();
        assert_eq!(lock_soft(&mutex).map(|guard| *guard), Some(5));
        assert_eq!(*lock(&mutex), 5);
    }
}
