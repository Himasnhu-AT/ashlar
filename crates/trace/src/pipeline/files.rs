//! Small helpers for the SDK's own files: atomic writes, lenient reads, clocks and locks.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ashlar_trace_proto::UnixMs;
use serde::Serialize;
use serde::de::DeserializeOwned;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Wall-clock time in Unix milliseconds. Only for timestamps; durations use `Instant`.
pub(super) fn now_ms() -> UnixMs {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)).unwrap_or(0);
    #[cfg(test)]
    let now = now.saturating_add_signed(testing::CLOCK_SKEW_MS.with(std::cell::Cell::get));
    now
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

/// Waits for a file, and the folder entry that names it, to be on the disk. Best effort.
pub(super) fn sync_file(path: &Path) {
    if let Ok(file) = std::fs::File::open(path) {
        let _ = file.sync_all();
    }
    #[cfg(unix)]
    if let Some(Ok(folder)) = path.parent().map(std::fs::File::open) {
        let _ = folder.sync_all();
    }
}

pub(super) fn write_json<T: Serialize>(path: &Path, value: &T) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| write_atomic(path, &bytes).is_ok())
}

/// Largest state file that is read. The SDK's own are a few hundred bytes; anything bigger is not one.
const MAX_STATE_FILE_BYTES: u64 = 1024 * 1024;

/// Reads a file the SDK wrote itself: at most `limit` bytes of a regular file. A symbolic link is not
/// followed, so nothing planted in the SDK's folder can make it read, and then send, a file from
/// somewhere else; and a file grown huge cannot make it allocate without bound. `Ok(None)` means the file
/// is there but is not one of ours: a link, a folder, or too big.
pub(super) fn read_own_file(path: &Path, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;

    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    // The size is checked again while reading, in case the file grows between the two.
    std::fs::File::open(path)?.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= limit).then_some(bytes))
}

/// Reads and parses a file. A missing or malformed file is `None`, never an error.
pub(super) fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let bytes = read_own_file(path, MAX_STATE_FILE_BYTES).ok()??;
    serde_json::from_slice(&bytes).ok()
}

/// Longest single wait on a condition variable. Waits are taken in steps of at most this, so that no
/// platform is ever handed a timeout it cannot represent.
const WAIT_STEP: Duration = Duration::from_secs(60 * 60);

/// A point in time `wait` from now, or `None` when that is further away than the clock can express,
/// which callers treat as "no deadline". `Instant + Duration` panics in that case.
pub(super) fn deadline_in(wait: Duration) -> Option<std::time::Instant> {
    std::time::Instant::now().checked_add(wait)
}

/// How long to wait next for something due by `deadline`: `None` when the deadline has passed.
pub(super) fn wait_step(deadline: Option<std::time::Instant>) -> Option<Duration> {
    match deadline {
        None => Some(WAIT_STEP),
        Some(deadline) => {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            (!left.is_zero()).then_some(left.min(WAIT_STEP))
        }
    }
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

    thread_local! {
        /// Added to the wall clock as this thread sees it, to stage the user or the network changing
        /// the system time. Monotonic time is not affected, as it is not in reality.
        pub(crate) static CLOCK_SKEW_MS: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
    }

    /// A folder under the system temporary directory, removed on drop.
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn new(label: &str) -> Self {
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("ashlar-trace-{label}-{}-{unique}-{}", std::process::id(), super::now_ms()));
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
    fn deadlines_too_far_away_to_express_mean_no_deadline() {
        assert_eq!(deadline_in(Duration::MAX), None);
        assert_eq!(wait_step(None), Some(WAIT_STEP));
        assert_eq!(wait_step(deadline_in(Duration::ZERO)), None);
        let soon = wait_step(deadline_in(Duration::from_secs(5))).unwrap();
        assert!(soon <= Duration::from_secs(5) && soon > Duration::from_secs(4));
        assert_eq!(wait_step(deadline_in(Duration::from_secs(1_000_000))), Some(WAIT_STEP));
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
