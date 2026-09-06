//! Where small persistent files live. Code that keeps settings, keymaps or a session sentinel sees
//! only these traits; the app picks the backend: a folder on disk ([`DirConfig`], [`StdFiles`]),
//! memory for tests and headless runs ([`MemoryConfig`], [`MemFiles`]), or browser storage (OPFS)
//! on the web, added later as another implementation.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use crate::atomic::atomic_write;

/// Named text blobs (`settings.json`, `keymap.json`, `session.lock`). Names are relative,
/// `/`-separated, and never climb out of the store (`..`, absolute paths and empty components are
/// refused).
pub trait ConfigStore: Send + Sync {
    /// The blob, or `None` when it doesn't exist or can't be read.
    fn read(&self, name: &str) -> Option<String>;
    /// Replace the blob atomically.
    fn write(&self, name: &str, data: &str) -> io::Result<()>;
    /// Remove the blob; removing a missing blob is not an error.
    fn remove(&self, name: &str) -> io::Result<()>;
    /// Names of the blobs directly in `dir` (`""` for the top level), sorted.
    fn list(&self, dir: &str) -> Vec<String>;
}

/// The file operations autosave and recovery need, by path.
pub trait FileOps: Send + Sync {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Write `data` to `path` atomically, creating its folder.
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()>;
    /// Remove a file; removing a missing file is not an error.
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// Files directly in `dir`: (file name, modified time), sorted by name.
    fn list(&self, dir: &Path) -> Vec<(String, SystemTime)>;
    fn is_file(&self, path: &Path) -> bool;
}

/// Check a [`ConfigStore`] name: relative, no `..`, no empty or `.` components.
pub fn check_name(name: &str) -> io::Result<()> {
    let bad = || io::Error::new(io::ErrorKind::InvalidInput, format!("invalid config name `{name}`"));
    if name.is_empty() || name.contains('\\') || name.contains('\0') {
        return Err(bad());
    }
    for part in name.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(bad());
        }
    }
    if Path::new(name).components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(bad());
    }
    Ok(())
}

fn not_found(e: io::Error) -> io::Result<()> {
    if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) }
}

// ------------------------------------------------------------------ disk

/// Blobs as files in one directory, written atomically.
#[derive(Clone, Debug)]
pub struct DirConfig {
    pub dir: PathBuf,
}

impl DirConfig {
    pub fn new(dir: impl Into<PathBuf>) -> DirConfig {
        DirConfig { dir: dir.into() }
    }
}

impl ConfigStore for DirConfig {
    fn read(&self, name: &str) -> Option<String> {
        check_name(name).ok()?;
        std::fs::read_to_string(self.dir.join(name)).ok()
    }
    fn write(&self, name: &str, data: &str) -> io::Result<()> {
        check_name(name)?;
        StdFiles.write(&self.dir.join(name), data.as_bytes())
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        check_name(name)?;
        StdFiles.remove(&self.dir.join(name))
    }
    fn list(&self, dir: &str) -> Vec<String> {
        let path = if dir.is_empty() {
            self.dir.clone()
        } else {
            if check_name(dir).is_err() {
                return vec![];
            }
            self.dir.join(dir)
        };
        StdFiles.list(&path).into_iter().map(|(n, _)| n).collect()
    }
}

/// [`FileOps`] on the file system.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdFiles;

impl FileOps for StdFiles {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        if let Some(d) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(d)?;
        }
        atomic_write(path, data)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path).or_else(not_found)
    }
    fn list(&self, dir: &Path) -> Vec<(String, SystemTime)> {
        let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
        let mut v: Vec<(String, SystemTime)> = rd
            .flatten()
            .filter_map(|e| {
                let m = e.metadata().ok().filter(|m| m.is_file())?;
                let name = e.file_name().into_string().ok()?;
                Some((name, m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
            })
            .filter(|(n, _)| !crate::atomic::is_temp_name(n))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
}

// ------------------------------------------------------------------ memory

/// Blobs in memory (tests, headless sessions).
#[derive(Debug, Default)]
pub struct MemoryConfig {
    files: Mutex<BTreeMap<String, String>>,
}

impl MemoryConfig {
    pub fn new() -> MemoryConfig {
        MemoryConfig::default()
    }
}

impl ConfigStore for MemoryConfig {
    fn read(&self, name: &str) -> Option<String> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner).get(name).cloned()
    }
    fn write(&self, name: &str, data: &str) -> io::Result<()> {
        check_name(name)?;
        self.files.lock().unwrap_or_else(PoisonError::into_inner).insert(name.to_string(), data.to_string());
        Ok(())
    }
    fn remove(&self, name: &str) -> io::Result<()> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner).remove(name);
        Ok(())
    }
    fn list(&self, dir: &str) -> Vec<String> {
        let pre = if dir.is_empty() { String::new() } else { format!("{}/", dir.trim_end_matches('/')) };
        let m = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        m.keys().filter_map(|k| k.strip_prefix(&pre)).filter(|n| !n.is_empty() && !n.contains('/')).map(str::to_string).collect()
    }
}

/// Files in memory, keyed by path (tests; a stand-in for browser storage).
#[derive(Debug, Default)]
pub struct MemFiles {
    files: Mutex<BTreeMap<PathBuf, (Vec<u8>, SystemTime)>>,
}

impl MemFiles {
    pub fn new() -> MemFiles {
        MemFiles::default()
    }
}

impl FileOps for MemFiles {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let m = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        m.get(path).map(|f| f.0.clone()).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, path.display().to_string()))
    }
    fn write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let now = now_or_epoch();
        self.files.lock().unwrap_or_else(PoisonError::into_inner).insert(path.to_path_buf(), (data.to_vec(), now));
        Ok(())
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner).remove(path);
        Ok(())
    }
    fn list(&self, dir: &Path) -> Vec<(String, SystemTime)> {
        let m = self.files.lock().unwrap_or_else(PoisonError::into_inner);
        m.iter().filter(|(p, _)| p.parent() == Some(dir)).filter_map(|(p, f)| Some((p.file_name()?.to_str()?.to_string(), f.1))).collect()
    }
    fn is_file(&self, path: &Path) -> bool {
        self.files.lock().unwrap_or_else(PoisonError::into_inner).contains_key(path)
    }
}

/// `SystemTime::now()`, or the epoch where there is no clock (`wasm32-unknown-unknown` panics on
/// `now()`).
pub(crate) fn now_or_epoch() -> SystemTime {
    #[cfg(not(target_arch = "wasm32"))]
    return SystemTime::now();
    #[cfg(target_arch = "wasm32")]
    SystemTime::UNIX_EPOCH
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> tempfile::TempDir {
        tempfile::Builder::new().prefix(&format!("ashlar-backend-{tag}-")).tempdir().unwrap()
    }

    fn exercise(s: &dyn ConfigStore) {
        assert_eq!(s.read("a.json"), None);
        s.write("a.json", "1").unwrap();
        s.write("sub/b.json", "2").unwrap();
        s.write("sub/c.json", "3").unwrap();
        assert_eq!(s.read("a.json").as_deref(), Some("1"));
        assert_eq!(s.list(""), vec!["a.json"]);
        assert_eq!(s.list("sub"), vec!["b.json", "c.json"]);
        s.remove("sub/b.json").unwrap();
        s.remove("sub/b.json").unwrap();
        assert_eq!(s.list("sub"), vec!["c.json"]);
        for bad in ["", "../x", "/etc/passwd", "a/../../x", "a//b", "./a", "a\\b", "a/."] {
            assert!(s.write(bad, "x").is_err(), "{bad}");
            assert_eq!(s.read(bad), None);
        }
    }

    #[test]
    fn memory_config() {
        exercise(&MemoryConfig::new());
    }

    #[test]
    fn dir_config() {
        let tmp = tmpdir("cfg");
        let d = tmp.path();
        exercise(&DirConfig::new(d));
        assert!(!d.parent().unwrap().join("x").exists());
    }

    fn exercise_files(f: &dyn FileOps, root: &Path) {
        let p = root.join("dir/a.bin");
        assert!(!f.is_file(&p));
        assert!(f.read(&p).is_err());
        f.write(&p, b"xy").unwrap();
        assert!(f.is_file(&p));
        assert_eq!(f.read(&p).unwrap(), b"xy");
        f.write(&root.join("dir/b.bin"), b"z").unwrap();
        let names: Vec<String> = f.list(&root.join("dir")).into_iter().map(|e| e.0).collect();
        assert_eq!(names, vec!["a.bin", "b.bin"]);
        f.remove(&p).unwrap();
        f.remove(&p).unwrap();
        assert!(!f.is_file(&p));
        assert!(f.list(&root.join("missing")).is_empty());
    }

    #[test]
    fn mem_files() {
        exercise_files(&MemFiles::new(), Path::new("/virtual"));
    }

    #[test]
    fn std_files() {
        let tmp = tmpdir("files");
        let d = tmp.path();
        exercise_files(&StdFiles, d);
    }
}
