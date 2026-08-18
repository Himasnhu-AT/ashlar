//! What the end user allows, and the process-wide kill switches.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use serde::{Deserialize, Serialize};
use tracer_proto::ConsentCategory;

use super::files::{lock, read_json, write_json};

pub(super) const CONSENT_FILE: &str = "consent.json";

/// Environment variables that switch the SDK off for the whole process.
const KILL_SWITCHES: [&str; 2] = ["DO_NOT_TRACK", "TRACER_DISABLED"];

/// Content of `consent.json`. A missing field means allowed: the model is opt-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ConsentFile {
    #[serde(default = "allowed")]
    crash: bool,
    #[serde(default = "allowed")]
    performance: bool,
    #[serde(default = "allowed")]
    usage: bool,
    #[serde(default)]
    notice_shown: bool,
}

fn allowed() -> bool {
    true
}

/// The live consent state. Reads are one atomic load.
///
/// The file is shared by every process of the host that uses the same data directory, so the worker
/// reads it again on each tick ([`Consent::refresh`]) and a choice made in one process reaches the others.
pub(super) struct Consent {
    bits: AtomicU8,
    notice_shown: AtomicBool,
    path: PathBuf,
    /// Held while the file is read into memory or changed and saved, so that a refresh cannot put back
    /// what a change on another thread has just replaced.
    file: Mutex<()>,
    /// A choice made in this process has not reached the file yet, because saving failed. Until it has,
    /// memory is the authority: the file is not read, and every refresh tries the save again.
    unsaved: AtomicBool,
    /// The file was written and has not been flushed to the disk yet.
    unsynced: AtomicBool,
    /// The file has existed at some point in this run, so if it disappears it is put back.
    persisted: AtomicBool,
}

impl Consent {
    /// Loads the saved choices, defaulting to everything allowed. Writes nothing.
    pub(super) fn load(dir: &Path) -> Self {
        let path = dir.join(CONSENT_FILE);
        let path_exists = path.is_file();
        let file = read_json::<ConsentFile>(&path).unwrap_or(ConsentFile { crash: true, performance: true, usage: true, notice_shown: false });
        Self {
            bits: AtomicU8::new(file.bits()),
            notice_shown: AtomicBool::new(file.notice_shown),
            path,
            file: Mutex::new(()),
            unsaved: AtomicBool::new(false),
            unsynced: AtomicBool::new(false),
            persisted: AtomicBool::new(path_exists),
        }
    }

    /// Takes over what the file says now, if it can be read. Returns the categories that were allowed
    /// and no longer are.
    pub(super) fn refresh(&self) -> Vec<ConsentCategory> {
        let _file = lock(&self.file);
        self.read_file()
    }

    /// Reads the file into memory, unless memory holds a choice the file does not have yet; then the
    /// save is tried again instead. A file that could not be written must never undo an opt-out.
    fn read_file(&self) -> Vec<ConsentCategory> {
        if self.unsaved.load(Ordering::Relaxed) {
            self.save();
            return Vec::new();
        }
        let Some(file) = read_json::<ConsentFile>(&self.path) else {
            // The file this process has read or written before is gone or unreadable. Left like that,
            // the next launch would start from the defaults and forget an opt-out; write it again.
            if self.persisted.load(Ordering::Relaxed) {
                self.save();
            }
            return Vec::new();
        };
        self.persisted.store(true, Ordering::Relaxed);
        let before = self.bits.swap(file.bits(), Ordering::Relaxed);
        if file.notice_shown {
            self.notice_shown.store(true, Ordering::Relaxed);
        }
        CATEGORIES.into_iter().filter(|category| before & bit(*category) != 0 && file.bits() & bit(*category) == 0).collect()
    }

    pub(super) fn get(&self, category: ConsentCategory) -> bool {
        self.bits.load(Ordering::Relaxed) & bit(category) != 0
    }

    /// True when at least one category is allowed.
    pub(super) fn any(&self) -> bool {
        self.bits.load(Ordering::Relaxed) != 0
    }

    /// Changes one category for this process only, writing nothing. For a process the kill switch has
    /// switched off, which must leave no trace on disk.
    pub(super) fn set_in_memory(&self, category: ConsentCategory, allowed: bool) {
        if allowed {
            self.bits.fetch_or(bit(category), Ordering::Relaxed);
        } else {
            self.bits.fetch_and(!bit(category), Ordering::Relaxed);
        }
    }

    /// Changes one category and saves the file.
    pub(super) fn set(&self, category: ConsentCategory, allowed: bool) {
        let _file = lock(&self.file);
        // Start from the file, so that a choice another process made for another category is kept.
        self.read_file();
        if allowed {
            self.bits.fetch_or(bit(category), Ordering::Relaxed);
        } else {
            self.bits.fetch_and(!bit(category), Ordering::Relaxed);
        }
        self.save();
    }

    pub(super) fn notice_shown(&self) -> bool {
        self.notice_shown.load(Ordering::Relaxed)
    }

    pub(super) fn set_notice_shown(&self) {
        let _file = lock(&self.file);
        self.read_file();
        self.notice_shown.store(true, Ordering::Relaxed);
        self.save();
    }

    fn save(&self) {
        let saved = self.write_file();
        self.unsaved.store(!saved, Ordering::Relaxed);
        if saved {
            self.unsynced.store(true, Ordering::Relaxed);
            self.persisted.store(true, Ordering::Relaxed);
        }
    }

    /// Makes the last save durable. Called by the worker, so that no host thread waits for the disk.
    pub(super) fn sync(&self) {
        if self.unsynced.swap(false, Ordering::Relaxed) {
            super::files::sync_file(&self.path);
        }
    }

    fn write_file(&self) -> bool {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = ConsentFile {
            crash: self.get(ConsentCategory::Crash),
            performance: self.get(ConsentCategory::Performance),
            usage: self.get(ConsentCategory::Usage),
            notice_shown: self.notice_shown(),
        };
        write_json(&self.path, &file)
    }
}

const CATEGORIES: [ConsentCategory; 3] = [ConsentCategory::Crash, ConsentCategory::Performance, ConsentCategory::Usage];

impl ConsentFile {
    fn bits(&self) -> u8 {
        let allowed = [(ConsentCategory::Crash, self.crash), (ConsentCategory::Performance, self.performance), (ConsentCategory::Usage, self.usage)];
        allowed.into_iter().filter(|(_, on)| *on).fold(0, |bits, (category, _)| bits | bit(category))
    }
}

fn bit(category: ConsentCategory) -> u8 {
    match category {
        ConsentCategory::Crash => 1,
        ConsentCategory::Performance => 2,
        ConsentCategory::Usage => 4,
    }
}

/// True when `DO_NOT_TRACK` or `TRACER_DISABLED` is set to anything other than empty, `0` or `false`.
pub(super) fn kill_switch() -> bool {
    KILL_SWITCHES.iter().any(|name| std::env::var_os(name).is_some_and(|value| switch_on(&value.to_string_lossy())))
}

fn switch_on(value: &str) -> bool {
    let value = value.trim();
    !(value.is_empty() || value == "0" || value.eq_ignore_ascii_case("false"))
}

#[cfg(test)]
mod tests {
    use super::super::files::testing::TempDir;
    use super::*;

    #[test]
    fn kill_switch_values() {
        for off in ["", "0", "false", "FALSE", " 0 "] {
            assert!(!switch_on(off), "{off:?}");
        }
        for on in ["1", "true", "yes", "on", "2"] {
            assert!(switch_on(on), "{on}");
        }
    }

    #[test]
    fn defaults_to_allowed_and_writes_nothing_on_load() {
        let dir = TempDir::new("consent");
        let consent = Consent::load(dir.path());
        assert!(consent.get(ConsentCategory::Crash) && consent.get(ConsentCategory::Performance) && consent.get(ConsentCategory::Usage));
        assert!(!consent.notice_shown());
        assert!(!dir.path().join(CONSENT_FILE).exists());
    }

    #[test]
    fn choices_are_saved_and_reloaded() {
        let dir = TempDir::new("consent");
        let consent = Consent::load(dir.path());
        consent.set(ConsentCategory::Usage, false);
        consent.set_notice_shown();
        let reloaded = Consent::load(dir.path());
        assert!(reloaded.get(ConsentCategory::Crash));
        assert!(!reloaded.get(ConsentCategory::Usage));
        assert!(reloaded.notice_shown() && reloaded.any());
        let text = std::fs::read_to_string(dir.path().join(CONSENT_FILE)).unwrap();
        assert_eq!(text, r#"{"crash":true,"performance":true,"usage":false,"notice_shown":true}"#);
    }

    #[test]
    fn a_change_by_another_process_is_picked_up_and_merged() {
        let dir = TempDir::new("consent");
        let here = Consent::load(dir.path());
        let there = Consent::load(dir.path());
        assert!(here.refresh().is_empty(), "no file yet, nothing changes");
        there.set(ConsentCategory::Usage, false);
        there.set_notice_shown();
        assert!(here.get(ConsentCategory::Usage));
        assert_eq!(here.refresh(), vec![ConsentCategory::Usage]);
        assert!(!here.get(ConsentCategory::Usage) && here.notice_shown());
        assert!(here.refresh().is_empty(), "reported once");
        // A change here keeps the other process's choice instead of writing over it.
        there.set(ConsentCategory::Crash, false);
        here.set(ConsentCategory::Performance, false);
        assert!(there.refresh().contains(&ConsentCategory::Performance));
        for consent in [&here, &there] {
            assert!(!consent.get(ConsentCategory::Crash) && !consent.get(ConsentCategory::Performance) && !consent.get(ConsentCategory::Usage));
            assert!(!consent.any());
        }
        there.set(ConsentCategory::Usage, true);
        assert!(here.refresh().is_empty());
        assert!(here.get(ConsentCategory::Usage));
        // A file that cannot be read changes nothing.
        std::fs::write(dir.path().join(CONSENT_FILE), b"{").unwrap();
        assert!(here.refresh().is_empty());
        assert!(here.get(ConsentCategory::Usage) && !here.get(ConsentCategory::Crash));
    }

    #[test]
    fn malformed_file_means_defaults() {
        let dir = TempDir::new("consent");
        std::fs::write(dir.path().join(CONSENT_FILE), b"[1,2").unwrap();
        assert!(Consent::load(dir.path()).get(ConsentCategory::Usage));
    }
}
