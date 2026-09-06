//! The control key: a random secret the app writes to a file only its user can read, and that
//! every request must carry. One rule for the app (which writes it) and the clients (which read
//! it): `<dir>/control-key.<instance>`, `<dir>` being a folder the app chooses (its settings
//! folder), `<instance>` the Flatpak instance id or `local-<port>`.

use std::io::Write;
use std::path::{Path, PathBuf};

/// A launcher (`ashlar-debug open`, a test harness) that starts an app with `--control <port>` sets
/// this to the folder the app must write its key to, so it can read the key back without knowing
/// the app's settings folder.
pub const KEY_DIR_ENV: &str = "ASHLAR_CONTROL_KEY_DIR";

/// The key folder a launcher asked for ([`KEY_DIR_ENV`]), if any.
pub fn key_dir_from_env() -> Option<PathBuf> {
    std::env::var_os(KEY_DIR_ENV).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// The `instance-id=` value of a `/.flatpak-info` file, only when it is `[A-Za-z0-9_-]+` (it
/// becomes part of a file name).
pub fn parse_instance_id(info: &str) -> Option<String> {
    let v = info.lines().find_map(|l| l.strip_prefix("instance-id="))?.trim();
    (!v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')).then(|| v.to_string())
}

/// The Flatpak instance id, or `None` outside Flatpak.
pub fn instance_id() -> Option<String> {
    parse_instance_id(&std::fs::read_to_string("/.flatpak-info").ok()?)
}

/// `<dir>/control-key.<instance>`.
pub fn key_path(dir: &Path, port: u16) -> PathBuf {
    key_path_for(dir, instance_id().as_deref(), port)
}

pub(crate) fn key_path_for(dir: &Path, instance: Option<&str>, port: u16) -> PathBuf {
    match instance {
        Some(id) => dir.join(format!("control-key.{id}")),
        None => dir.join(format!("control-key.local-{port}")),
    }
}

/// A new key: 32 random bytes from the operating system, as 64 hex digits.
pub fn random_key() -> Result<String, String> {
    let mut b = [0u8; 32];
    fill_random(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

#[cfg(not(target_arch = "wasm32"))]
fn fill_random(b: &mut [u8]) -> Result<(), String> {
    getrandom::fill(b).map_err(|e| e.to_string())
}

#[cfg(target_arch = "wasm32")]
fn fill_random(_: &mut [u8]) -> Result<(), String> {
    Err("no control port on the web".into())
}

/// Write the key file: a stale one (from a run that did not exit cleanly) is replaced; on Unix the
/// new file is created with mode 0600 (never readable by other users, not even for a moment).
pub fn write_key_file(p: &Path, key: &str) -> Result<(), String> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    let _ = std::fs::remove_file(p);
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    let mut f = open.open(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
    f.write_all(key.as_bytes()).map_err(|e| format!("cannot write {}: {e}", p.display()))
}

/// The key in a key file (trimmed). The error says where it looked.
pub fn read_key_file(p: &Path) -> Result<String, String> {
    // A key is 64 bytes; never read a huge file someone put there.
    let text = std::fs::read(p).map_err(|e| format!("no control key at {} ({e}); is the app running with its control port on?", p.display()))?;
    let text = text.get(..4096).unwrap_or(&text);
    let k = String::from_utf8_lossy(text).trim().to_string();
    if k.is_empty() {
        return Err(format!("the control key file {} is empty", p.display()));
    }
    Ok(k)
}

/// Compare a given key with the app's in time that does not depend on where they differ. An empty
/// key never matches.
pub fn keys_match(given: &str, key: &str) -> bool {
    let (a, b) = (given.as_bytes(), key.as_bytes());
    !b.is_empty() && a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A written key file, removed when dropped (the app closes its control port).
#[derive(Debug)]
pub struct KeyFile(PathBuf);

impl KeyFile {
    pub(crate) fn new(p: PathBuf) -> Self {
        Self(p)
    }

    /// Write `key` to `path` (as [`write_key_file`] does) and keep the file until this is dropped:
    /// for an app that serves the protocol itself instead of through [`crate::Server`]
    /// ([`key_path`] gives the path clients look at).
    pub fn create(path: impl Into<PathBuf>, key: &str) -> Result<KeyFile, String> {
        let path = path.into();
        write_key_file(&path, key)?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn instance_id_only_safe_values() {
        assert_eq!(parse_instance_id("[Instance]\ninstance-id=abc_123-X\n").as_deref(), Some("abc_123-X"));
        assert_eq!(parse_instance_id("instance-id= 42 \n").as_deref(), Some("42"));
        for bad in ["instance-id=../etc\n", "instance-id=a/b\n", "instance-id=a b\n", "instance-id=..\n", "instance-id=\n", "app=x\n"] {
            assert_eq!(parse_instance_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn key_file_per_instance() {
        let dir = Path::new("/settings/app");
        assert_eq!(key_path_for(dir, Some("1234"), 7981), dir.join("control-key.1234"));
        assert_eq!(key_path_for(dir, None, 7981), dir.join("control-key.local-7981"));
        assert_eq!(key_path_for(dir, None, 7982), dir.join("control-key.local-7982"));
    }

    #[test]
    fn keys_compare_whole() {
        assert!(keys_match(KEY, KEY));
        assert!(!keys_match(KEY, &KEY[..63]));
        assert!(!keys_match(&KEY[..63], KEY));
        assert!(!keys_match(KEY, &KEY.replace('f', "e")));
        assert!(!keys_match("", ""), "an empty key never matches");
    }

    #[test]
    fn random_keys_are_256_bits() {
        let (a, b) = (random_key().unwrap(), random_key().unwrap());
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn a_stale_key_file_is_replaced_with_mode_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("control-key.test");
        std::fs::write(&p, "old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_key_file(&p, KEY).unwrap();
        assert_eq!(read_key_file(&p).unwrap(), KEY);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        drop(KeyFile::new(p.clone()));
        assert!(!p.exists());
        assert!(read_key_file(&p).unwrap_err().contains("no control key"));
        // an app serving the protocol itself owns its key file the same way
        let own = KeyFile::create(p.clone(), KEY).unwrap();
        assert_eq!((own.path(), read_key_file(&p).unwrap().as_str()), (p.as_path(), KEY));
        drop(own);
        assert!(!p.exists());
    }
}
