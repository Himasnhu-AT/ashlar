//! A most-recently-used list of files (File ▸ Open Recent).

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

/// Entries kept unless set with [`RecentFiles::new`].
pub const DEFAULT_CAP: usize = 10;

/// Most recent first, no duplicates, at most `cap` entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFiles {
    paths: Vec<PathBuf>,
    cap: usize,
}

impl Default for RecentFiles {
    fn default() -> Self {
        RecentFiles::new(DEFAULT_CAP)
    }
}

impl RecentFiles {
    /// An empty list keeping at most `cap` entries (at least 1).
    pub fn new(cap: usize) -> RecentFiles {
        RecentFiles { paths: Vec::new(), cap: cap.max(1) }
    }

    /// Move `path` to the front (adding it if new), dropping the oldest beyond the cap. Paths
    /// that differ only by `.` components or trailing separators are the same entry.
    pub fn push(&mut self, path: impl AsRef<Path>) {
        let p = normalize(path.as_ref());
        if p.as_os_str().is_empty() {
            return;
        }
        self.paths.retain(|q| *q != p);
        self.paths.insert(0, p);
        self.paths.truncate(self.cap);
    }

    /// Remove `path`; true when it was listed.
    pub fn remove(&mut self, path: impl AsRef<Path>) -> bool {
        let p = normalize(path.as_ref());
        let before = self.paths.len();
        self.paths.retain(|q| *q != p);
        self.paths.len() != before
    }

    /// Drop the entries `exists` says are gone (pass `Path::exists`, or a `FileOps` check).
    /// Returns how many were dropped.
    pub fn prune(&mut self, exists: impl Fn(&Path) -> bool) -> usize {
        let before = self.paths.len();
        self.paths.retain(|p| exists(p));
        before - self.paths.len()
    }

    pub fn clear(&mut self) {
        self.paths.clear();
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Change the cap, dropping the oldest entries beyond it.
    pub fn set_cap(&mut self, cap: usize) {
        self.cap = cap.max(1);
        self.paths.truncate(self.cap);
    }

    /// Read the list stored under `key` (a list of path strings), keeping this list's cap. Bad
    /// entries are skipped; a missing or malformed value gives an empty list.
    pub fn load(settings: &Settings, key: &str, cap: usize) -> RecentFiles {
        let mut r = RecentFiles::new(cap);
        let items: Vec<serde_json::Value> = settings.get(key).unwrap_or_default();
        for s in items.iter().rev().filter_map(|v| v.as_str()) {
            r.push(s);
        }
        r
    }

    /// Store the list under `key` as a list of path strings.
    pub fn store(&self, settings: &mut Settings, key: &str) {
        let v: Vec<String> = self.paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        settings.set_value(key, serde_json::Value::from(v));
    }
}

/// Lexical clean-up only (no file system access, so symlinks are not resolved): drops `.`
/// components and trailing separators.
fn normalize(p: &Path) -> PathBuf {
    p.components().filter(|c| !matches!(c, Component::CurDir)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{ConfigStore, MemoryConfig};
    use std::sync::Arc;

    #[test]
    fn mru_order_dedupe_and_cap() {
        let mut r = RecentFiles::new(3);
        r.push("/a");
        r.push("/b");
        r.push("/c");
        r.push("/a/"); // same as /a
        assert_eq!(r.paths(), [PathBuf::from("/a"), "/c".into(), "/b".into()]);
        r.push("/d");
        assert_eq!(r.paths(), [PathBuf::from("/d"), "/a".into(), "/c".into()]);
        r.push("/./c");
        assert_eq!(r.paths()[0], PathBuf::from("/c"));
        r.push("");
        assert_eq!(r.paths().len(), 3);
        assert!(r.remove("/a"));
        assert!(!r.remove("/a"));
        r.set_cap(1);
        assert_eq!(r.paths(), [PathBuf::from("/c")]);
        assert_eq!(RecentFiles::new(0).cap(), 1);
    }

    #[test]
    fn prune_missing() {
        let mut r = RecentFiles::new(5);
        for p in ["/keep1", "/gone", "/keep2"] {
            r.push(p);
        }
        assert_eq!(r.prune(|p| p.to_string_lossy().contains("keep")), 1);
        assert_eq!(r.paths(), [PathBuf::from("/keep2"), "/keep1".into()]);
    }

    #[test]
    fn round_trip_through_settings_and_hostile_values() {
        let store: Arc<dyn ConfigStore> = Arc::new(MemoryConfig::new());
        let mut s = Settings::load(store, "s.json", &[]);
        let mut r = RecentFiles::new(3);
        r.push("/x");
        r.push("/y");
        r.store(&mut s, "recent");
        assert_eq!(RecentFiles::load(&s, "recent", 3), r);
        s.set_value("recent", serde_json::json!(["/1", 5, null, "/2", "/3", "/4", "/1"]));
        let back = RecentFiles::load(&s, "recent", 3);
        assert_eq!(back.paths(), [PathBuf::from("/1"), "/2".into(), "/3".into()]);
        s.set_value("recent", serde_json::json!({"not": "a list"}));
        assert!(RecentFiles::load(&s, "recent", 3).paths().is_empty());
    }
}
