//! Crash-safe file replacement.
//!
//! A save goes to a hidden temp file in the destination's directory (so the rename never crosses
//! file systems), which is flushed to disk and then renamed over the destination. On Unix the
//! directory is flushed afterwards so the rename itself survives a power cut. A crash at any point
//! leaves either the old file or the new one, never a torn mix.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Suffix of every temp file [`atomic_write`] creates (`.<name>.<pid>-<n>.tmp`).
pub const TEMP_SUFFIX: &str = ".tmp";

/// Replace `path` with `data` atomically and durably.
///
/// The parent directory must exist. If anything fails the temp file is removed and `path` is
/// untouched. When `path` is a symlink, the file it points to is replaced (the link is kept). An
/// existing file's permissions are carried over.
pub fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    atomic_write_with(path, |f| f.write_all(data))
}

/// [`atomic_write`] with a writer: `write` gets the temp file and fills it (streams a large
/// document without building it in memory first). An error from `write` aborts the save and
/// leaves `path` untouched.
pub fn atomic_write_with(path: &Path, write: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let target = resolve_symlink(path);
    let tmp = temp_path(&target);
    let r = (|| {
        {
            // closed before the rename (Windows refuses to rename an open file)
            let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            write(&mut f)?;
            f.flush()?;
            if let Ok(m) = fs::metadata(&target) {
                let _ = f.set_permissions(m.permissions());
            }
            f.sync_all()?;
        }
        fs::rename(&tmp, &target)
    })();
    if let Err(e) = r {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    sync_dir(&parent_dir(&target));
    Ok(())
}

/// The temp file a save of `path` goes through: hidden, in the same directory, unique per process
/// and call.
pub fn temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    parent_dir(path).join(format!(".{name}.{}-{n}{TEMP_SUFFIX}", pid()))
}

/// True for names [`temp_path`] produces (leftovers of a crash mid-save can be swept with this).
pub fn is_temp_name(name: &str) -> bool {
    name.starts_with('.') && name.ends_with(TEMP_SUFFIX) && name.len() > 1 + TEMP_SUFFIX.len()
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn resolve_symlink(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

/// Best-effort directory fsync: persists the rename on POSIX. Windows cannot open a directory
/// this way; its `MoveFileEx` rename is journaled by NTFS.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// This process id (0 where processes don't exist, e.g. the browser).
pub fn pid() -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    return std::process::id();
    #[cfg(target_arch = "wasm32")]
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ashlar-atomic-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn names(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn replaces_and_leaves_no_temp_files() {
        let d = tmpdir("replace");
        let p = d.join("a.doc");
        atomic_write(&p, b"one").unwrap();
        atomic_write(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(names(&d), vec!["a.doc"]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn failed_rename_keeps_old_file_and_cleans_temp() {
        let d = tmpdir("fail");
        let p = d.join("a.doc");
        atomic_write(&p, b"good").unwrap();
        // Renaming a file over a non-empty directory fails.
        let dir_target = d.join("sub");
        fs::create_dir_all(dir_target.join("x")).unwrap();
        assert!(atomic_write(&dir_target, b"bad").is_err());
        assert_eq!(fs::read(&p).unwrap(), b"good");
        assert!(!names(&d).iter().any(|n| is_temp_name(n)), "{:?}", names(&d));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn writer_error_mid_write_keeps_original() {
        let d = tmpdir("midwrite");
        let p = d.join("a.doc");
        atomic_write(&p, b"original").unwrap();
        let r = atomic_write_with(&p, |f| {
            f.write_all(b"half of the new conte")?;
            Err(io::Error::other("simulated crash"))
        });
        assert!(r.is_err());
        assert_eq!(fs::read(&p).unwrap(), b"original");
        assert_eq!(names(&d), vec!["a.doc"]);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn missing_directory_is_an_error() {
        let d = tmpdir("missing");
        assert!(atomic_write(&d.join("nope/a.doc"), b"x").is_err());
        assert!(names(&d).is_empty());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn empty_and_large_payloads() {
        let d = tmpdir("sizes");
        let p = d.join("a.bin");
        atomic_write(&p, b"").unwrap();
        assert_eq!(fs::read(&p).unwrap().len(), 0);
        let big = vec![7u8; 8 << 20];
        atomic_write(&p, &big).unwrap();
        assert_eq!(fs::read(&p).unwrap(), big);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn temp_names_are_unique_and_recognised() {
        let a = temp_path(Path::new("x/a.doc"));
        let b = temp_path(Path::new("x/a.doc"));
        assert_ne!(a, b);
        assert!(is_temp_name(&a.file_name().unwrap().to_string_lossy()));
        assert!(!is_temp_name("a.doc"));
        assert!(!is_temp_name(".tmp"));
        assert_eq!(temp_path(Path::new("bare")).parent().unwrap(), Path::new("."));
    }

    #[cfg(unix)]
    #[test]
    fn keeps_permissions_and_follows_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("perm");
        let real = d.join("real.doc");
        atomic_write(&real, b"one").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.join("link.doc");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        atomic_write(&link, b"two").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link survives");
        assert_eq!(fs::read(&real).unwrap(), b"two");
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn concurrent_writers_never_tear() {
        let d = tmpdir("race");
        let p = d.join("a.doc");
        let handles: Vec<_> = (0..8u8)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        atomic_write(&p, &vec![i; 64 * 1024]).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let bytes = fs::read(&p).unwrap();
        assert_eq!(bytes.len(), 64 * 1024);
        assert!(bytes.iter().all(|b| *b == bytes[0]), "one writer's content, whole");
        assert_eq!(names(&d), vec!["a.doc"]);
        fs::remove_dir_all(&d).unwrap();
    }
}
