//! `cargo xtask deny-panics`: the two holes the workspace clippy lints leave in "never crash"
//! (AGENTS.md §0). Clippy enforces the lints only in crates that inherit them, and an
//! `#[allow(clippy::unwrap_used)]` silences them; so this checks that
//! 1. every workspace member has `[lints] workspace = true`, and
//! 2. no non-test source under `crates/` or `examples/` allows or expects a never-crash lint away.
//!
//! Test code is exempt: `tests/`, `benches/`, files that are `#![cfg(test)]` or named `tests.rs`, and
//! everything after the first `#[cfg(test)]` in a file (test modules go last).
//!
//! The one exception is the OS FFI crate (docs/adr/0001-native-ffi.md): it keeps its own `[lints]`
//! table, which must still deny every never-crash lint, deny (not allow) `unsafe_code` and deny
//! undocumented unsafe blocks; and only the FFI modules the ADR lists may allow `unsafe_code`.

use std::path::{Path, PathBuf};

/// The lints a crate may not switch off outside tests.
const GUARDED: &[&str] = &[
    "clippy::unwrap_used",
    "clippy::expect_used",
    "clippy::panic",
    "clippy::unimplemented",
    "clippy::todo",
    "clippy::unreachable",
    "clippy::all",
    "clippy::restriction",
    "unsafe_code",
];

/// The FFI crate of ADR 0001 and the files that may `#[allow(unsafe_code)]`. A new FFI module is
/// an ADR change: add it there and here.
const FFI_CRATE: &str = "crates/native";
const FFI_MODULES: &[&str] = &[
    "crates/native/src/power.rs",
    "crates/native/src/open_files.rs",
    "crates/native/src/progress.rs",
    "crates/native/src/system.rs",
    "crates/native/src/menu/native.rs",
    "crates/native/src/terminate.rs",
    "crates/native/src/memory.rs",
    "crates/native/src/display.rs",
];

/// What the FFI crate's own `[lints]` tables must contain instead of the workspace lints.
const FFI_REQUIRED: &[&str] = &[
    "unsafe_code=\"deny\"",
    "unwrap_used=\"deny\"",
    "expect_used=\"deny\"",
    "panic=\"deny\"",
    "unimplemented=\"deny\"",
    "todo=\"deny\"",
    "unreachable=\"deny\"",
    "undocumented_unsafe_blocks=\"deny\"",
];

/// The `FFI_REQUIRED` lines missing from a manifest's `[lints.*]` tables.
pub fn missing_ffi_lints(manifest: &str) -> Vec<&'static str> {
    let mut in_lints = false;
    let mut lines = Vec::new();
    for line in manifest.lines().map(|l| l.replace(' ', "")) {
        if line.starts_with('[') {
            in_lints = line.starts_with("[lints");
        } else if in_lints {
            lines.push(line);
        }
    }
    FFI_REQUIRED.iter().filter(|r| !lines.iter().any(|l| l == *r)).copied().collect()
}

/// True when the manifest has a `[lints]` table with `workspace = true`.
pub fn inherits_lints(manifest: &str) -> bool {
    let mut in_lints = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_lints = line == "[lints]";
        } else if in_lints && line.replace(' ', "") == "workspace=true" {
            return true;
        }
    }
    manifest.lines().any(|l| l.replace(' ', "") == "lints.workspace=true")
}

/// `(line, lint)` for each attribute in non-test code that allows or expects a guarded lint.
pub fn scan_source(src: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (n, raw) in src.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or_default();
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.starts_with("#![cfg(test)]") && found.is_empty() && n < 20 {
            return Vec::new();
        }
        if compact.starts_with("#[cfg(test)]") {
            break;
        }
        let Some(start) = ["#[allow(", "#![allow(", "#[expect(", "#![expect(", "#[cfg_attr("].iter().find_map(|p| compact.find(p)) else { continue };
        let attr = compact.get(start..).unwrap_or_default();
        if !(attr.contains("allow(") || attr.contains("expect(")) {
            continue;
        }
        for lint in GUARDED {
            if attr.split([',', '(', ')']).any(|t| t == *lint) {
                found.push((n + 1, (*lint).to_owned()));
            }
        }
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") && path.file_name().is_some_and(|n| n != "tests.rs") {
            out.push(path);
        }
    }
}

fn members(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for group in ["crates", "examples"] {
        let Ok(entries) = std::fs::read_dir(root.join(group)) else { continue };
        out.extend(entries.flatten().map(|e| e.path()).filter(|p| p.join("Cargo.toml").is_file()));
    }
    out.push(root.join("xtask"));
    out.sort();
    out
}

pub fn check(root: &Path) -> Result<(), String> {
    let mut errors = Vec::new();
    let mut scanned = 0;
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    for member in members(root) {
        let manifest = std::fs::read_to_string(member.join("Cargo.toml")).unwrap_or_default();
        let ffi = rel(&member).replace('\\', "/") == FFI_CRATE;
        if ffi {
            for lint in missing_ffi_lints(&manifest) {
                errors.push(format!("{}/Cargo.toml: the FFI crate's [lints] must have `{lint}` (docs/adr/0001-native-ffi.md)", rel(&member)));
            }
        } else if !inherits_lints(&manifest) {
            errors.push(format!("{}/Cargo.toml: add `[lints] workspace = true` (the never-crash lints)", rel(&member)));
        }
        if member.ends_with("xtask") {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&member.join("src"), &mut files);
        for file in files {
            scanned += 1;
            let src = std::fs::read_to_string(&file).unwrap_or_default();
            let ffi_module = FFI_MODULES.contains(&rel(&file).replace('\\', "/").as_str());
            for (line, lint) in scan_source(&src) {
                if ffi && ffi_module && lint == "unsafe_code" {
                    continue;
                }
                errors.push(format!("{}:{line}: allows `{lint}` outside tests (AGENTS.md §0: handle the error instead)", rel(&file)));
            }
        }
    }
    if errors.is_empty() {
        println!("deny-panics: ok ({scanned} files)");
        Ok(())
    } else {
        Err(format!("deny-panics: {} problem(s)\n{}", errors.len(), errors.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lints_table_is_detected() {
        assert!(inherits_lints("[package]\nname = \"a\"\n\n[lints]\nworkspace = true\n"));
        assert!(inherits_lints("[package]\nlints.workspace = true\n"));
        assert!(!inherits_lints("[package]\nname = \"a\"\n[dependencies]\nworkspace = true\n"));
    }

    #[test]
    fn ffi_crate_lints() {
        let ok = "[lints.rust]\nunsafe_code = \"deny\"\n[lints.clippy]\nunwrap_used = \"deny\"\nexpect_used = \"deny\"\npanic = \"deny\"\nunimplemented = \"deny\"\ntodo = \"deny\"\nunreachable = \"deny\"\nundocumented_unsafe_blocks = \"deny\"\n";
        assert!(missing_ffi_lints(ok).is_empty());
        assert_eq!(missing_ffi_lints(&ok.replace("unsafe_code = \"deny\"", "unsafe_code = \"allow\"")), ["unsafe_code=\"deny\""]);
        assert_eq!(missing_ffi_lints("[dependencies]\npanic = \"deny\"\n").len(), FFI_REQUIRED.len());
    }

    #[test]
    fn allows_outside_tests_are_found() {
        let src = "#![allow(clippy::unwrap_used, dead_code)]\nfn a() {}\n#[ expect( clippy::panic ) ]\nfn b() {}\n#[allow(dead_code)]\nfn c() {}\n";
        assert_eq!(scan_source(src), [(1, "clippy::unwrap_used".to_owned()), (3, "clippy::panic".to_owned())]);
        let cfg_attr = "#[cfg_attr(feature = \"x\", allow(clippy::todo))]\nfn d() {}\n";
        assert_eq!(scan_source(cfg_attr), [(1, "clippy::todo".to_owned())]);
    }

    #[test]
    fn test_code_and_comments_are_exempt() {
        assert!(scan_source("fn a() {}\n#[cfg(test)]\nmod tests {\n    #![allow(clippy::unwrap_used)]\n}\n").is_empty());
        assert!(scan_source("#![cfg(test)]\n#![allow(clippy::unwrap_used)]\n").is_empty());
        assert!(scan_source("// #[allow(clippy::unwrap_used)]\nfn a() {}\n").is_empty());
        assert!(scan_source("#[allow(clippy::unwrap_used_extra)]\n").is_empty());
    }
}
