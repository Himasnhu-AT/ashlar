//! Inspect mode: a capped local mirror of every item accepted for sending, for the user to read.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::files::write_atomic;

pub(super) const OUTBOX_FILE: &str = "outbox.jsonl";

/// Largest size of the mirror. When it is reached the older half is discarded.
const MAX_OUTBOX_BYTES: u64 = 1024 * 1024;

pub(super) struct Outbox {
    path: PathBuf,
    bytes: u64,
    max_bytes: u64,
}

impl Outbox {
    pub(super) fn open(dir: &Path) -> Self {
        Self::with_cap(dir, MAX_OUTBOX_BYTES)
    }

    fn with_cap(dir: &Path, max_bytes: u64) -> Self {
        let path = dir.join(OUTBOX_FILE);
        let bytes = std::fs::metadata(&path).map(|metadata| metadata.len()).unwrap_or(0);
        Self { path, bytes, max_bytes }
    }

    /// Appends one item as a line of JSON. Failures are ignored: the mirror is a convenience.
    pub(super) fn push(&mut self, json: &str) {
        let line_bytes = json.len() as u64 + 1;
        if line_bytes > self.max_bytes {
            return;
        }
        if self.bytes + line_bytes > self.max_bytes {
            self.trim(self.max_bytes / 2);
        }
        let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.path) else { return };
        let mut line = Vec::with_capacity(json.len() + 1);
        line.extend_from_slice(json.as_bytes());
        line.push(b'\n');
        if file.write_all(&line).is_ok() {
            self.bytes += line_bytes;
        }
    }

    /// Keeps the newest whole lines that fit in `keep` bytes.
    fn trim(&mut self, keep: u64) {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut start = text.len();
        for line in text.split_inclusive('\n').rev() {
            if (text.len() - start + line.len()) as u64 > keep {
                break;
            }
            start -= line.len();
        }
        let tail = text.get(start..).unwrap_or_default();
        if write_atomic(&self.path, tail.as_bytes()).is_ok() {
            self.bytes = tail.len() as u64;
        }
    }

    pub(super) fn wipe(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::super::files::testing::TempDir;
    use super::*;

    #[test]
    fn mirror_stays_under_its_cap_and_keeps_the_newest_lines() {
        let dir = TempDir::new("outbox");
        let mut outbox = Outbox::with_cap(dir.path(), 1000);
        for number in 0..500 {
            outbox.push(&format!(r#"{{"n":{number:04}}}"#));
            assert!(std::fs::metadata(dir.path().join(OUTBOX_FILE)).unwrap().len() <= 1000);
        }
        let text = std::fs::read_to_string(dir.path().join(OUTBOX_FILE)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.last(), Some(&r#"{"n":0499}"#));
        assert!(lines.len() > 30 && lines.len() < 100);
        assert!(lines.iter().all(|line| line.starts_with('{') && line.ends_with('}')));
    }

    #[test]
    fn reopening_continues_the_same_file() {
        let dir = TempDir::new("outbox");
        Outbox::open(dir.path()).push("{}");
        let mut outbox = Outbox::open(dir.path());
        assert_eq!(outbox.bytes, 3);
        outbox.push("{}");
        assert_eq!(std::fs::read_to_string(dir.path().join(OUTBOX_FILE)).unwrap(), "{}\n{}\n");
        outbox.wipe();
        assert!(!dir.path().join(OUTBOX_FILE).exists());
    }
}
