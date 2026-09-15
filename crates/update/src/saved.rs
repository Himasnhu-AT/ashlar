//! The last check's result, kept in a small JSON file so a newer release is shown at launch before
//! the network answers, and "later" survives a restart.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Release;

/// The largest state file read.
const MAX_FILE: u64 = 256 * 1024;

/// What is kept between runs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    /// When the last successful check finished, Unix milliseconds (0: never).
    #[serde(default)]
    pub checked_at: i64,
    /// The newest release that check found.
    #[serde(default)]
    pub latest: Option<Release>,
    /// The version the person chose "later" for: no notice for it (a newer one shows again).
    #[serde(default)]
    pub dismissed: Option<String>,
}

impl Saved {
    /// Reads `path`. A missing, oversized or unreadable file is an empty state, never an error.
    pub fn load(path: &Path) -> Saved {
        let Ok(meta) = std::fs::metadata(path) else {
            return Saved::default();
        };
        if meta.len() > MAX_FILE {
            log::warn!("update state {} is too large; ignored", path.display());
            return Saved::default();
        }
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("update state {} unreadable ({e}); starting afresh", path.display());
                Saved::default()
            }),
            Err(_) => Saved::default(),
        }
    }

    /// Writes `path` atomically, creating its folder. Failures are logged, not returned: a lost
    /// cache only costs one extra check.
    pub fn store(&self, path: &Path) {
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        if let Some(dir) = path.parent()
            && !dir.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            log::warn!("update state folder {}: {e}", dir.display());
            return;
        }
        if let Err(e) = ashlar_fs::atomic_write(path, &bytes) {
            log::warn!("update state {}: {e}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_tolerates_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/update.json");
        assert_eq!(Saved::load(&path), Saved::default());
        let saved = Saved { checked_at: 5, latest: None, dismissed: Some("1.2.3".into()) };
        saved.store(&path);
        assert_eq!(Saved::load(&path), saved);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(Saved::load(&path), Saved::default());
        std::fs::write(&path, vec![b' '; 300 * 1024]).unwrap();
        assert_eq!(Saved::load(&path), Saved::default());
    }
}
