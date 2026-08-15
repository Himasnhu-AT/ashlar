//! What the end user allows, and the process-wide kill switches.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use serde::{Deserialize, Serialize};
use tracer_proto::ConsentCategory;

use super::files::{read_json, write_json};

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
pub(super) struct Consent {
    bits: AtomicU8,
    notice_shown: AtomicBool,
    path: PathBuf,
}

impl Consent {
    /// Loads the saved choices, defaulting to everything allowed. Writes nothing.
    pub(super) fn load(dir: &Path) -> Self {
        let path = dir.join(CONSENT_FILE);
        let file = read_json::<ConsentFile>(&path).unwrap_or(ConsentFile { crash: true, performance: true, usage: true, notice_shown: false });
        let mut bits = 0;
        for (category, on) in
            [(ConsentCategory::Crash, file.crash), (ConsentCategory::Performance, file.performance), (ConsentCategory::Usage, file.usage)]
        {
            if on {
                bits |= bit(category);
            }
        }
        Self { bits: AtomicU8::new(bits), notice_shown: AtomicBool::new(file.notice_shown), path }
    }

    pub(super) fn get(&self, category: ConsentCategory) -> bool {
        self.bits.load(Ordering::Relaxed) & bit(category) != 0
    }

    /// True when at least one category is allowed.
    pub(super) fn any(&self) -> bool {
        self.bits.load(Ordering::Relaxed) != 0
    }

    /// Changes one category and saves the file.
    pub(super) fn set(&self, category: ConsentCategory, allowed: bool) {
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
        self.notice_shown.store(true, Ordering::Relaxed);
        self.save();
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = ConsentFile {
            crash: self.get(ConsentCategory::Crash),
            performance: self.get(ConsentCategory::Performance),
            usage: self.get(ConsentCategory::Usage),
            notice_shown: self.notice_shown(),
        };
        write_json(&self.path, &file);
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
    fn malformed_file_means_defaults() {
        let dir = TempDir::new("consent");
        std::fs::write(dir.path().join(CONSENT_FILE), b"[1,2").unwrap();
        assert!(Consent::load(dir.path()).get(ConsentCategory::Usage));
    }
}
