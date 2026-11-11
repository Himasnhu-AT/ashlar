//! `cargo xtask release <x.y.z> [--date YYYY-MM-DD]`: steps 2 and 3 of docs/releasing.md.
//!
//! Bumps the workspace version and every internal crate's version in `[workspace.dependencies]`
//! together, renames CHANGELOG.md's "Unreleased" to `## [x.y.z] - <date>` under a fresh empty
//! "Unreleased", updates the compare links, and runs `cargo check` to refresh Cargo.lock. The date
//! defaults to today (UTC). It never commits, tags or publishes: the steps after it are printed.

use std::path::Path;

const REPO: &str = "https://github.com/Himasnhu-AT/ashlar";
const USAGE: &str = "usage: cargo xtask release <x.y.z> [--date YYYY-MM-DD]";

pub fn release(root: &Path, args: &[String]) -> Result<(), String> {
    let mut version = None;
    let mut date = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--date" => date = Some(it.next().ok_or(USAGE)?.clone()),
            v if version.is_none() && !v.starts_with('-') => version = Some(v.to_string()),
            other => return Err(format!("unexpected argument: {other}\n{USAGE}")),
        }
    }
    let version = version.ok_or(USAGE)?;
    if !is_version(&version) {
        return Err(format!("not a version (x.y.z): {version}"));
    }
    let date = date.unwrap_or_else(today);
    if !is_date(&date) {
        return Err(format!("not a date (YYYY-MM-DD): {date}"));
    }

    let manifest_path = root.join("Cargo.toml");
    let changelog_path = root.join("CHANGELOG.md");
    let manifest = read(&manifest_path)?;
    let changelog = read(&changelog_path)?;
    let (manifest, old, crates) = bump_manifest(&manifest, &version)?;
    let changelog = bump_changelog(&changelog, &old, &version, &date)?;
    write(&manifest_path, &manifest)?;
    write(&changelog_path, &changelog)?;
    println!("Cargo.toml: {old} -> {version} (workspace and {crates} internal crates)");
    println!("CHANGELOG.md: [Unreleased] -> [{version}] - {date}");

    crate::run(&mut crate::cmd(&["check", "--workspace", "--quiet"]))?;
    println!(
        "\nNext (docs/releasing.md):\n  cargo xtask ci\n  cargo publish --workspace --locked --dry-run\n  \
         commit, push, wait for CI\n  git tag -a v{version} -m \"ashlar {version}\" && git push origin v{version}"
    );
    Ok(())
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn write(p: &Path, s: &str) -> Result<(), String> {
    std::fs::write(p, s).map_err(|e| format!("{}: {e}", p.display()))
}

fn is_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

fn is_date(d: &str) -> bool {
    let b = d.as_bytes();
    b.len() == 10 && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() })
}

/// Today's UTC date as YYYY-MM-DD (days since 1970 to a civil date, Howard Hinnant's algorithm).
fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    civil_date(i64::try_from(secs / 86_400).unwrap_or(0))
}

fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The value of `version = "…"` on a manifest line, if it has one.
fn version_on(line: &str) -> Option<&str> {
    let rest = &line[line.find("version = \"")? + "version = \"".len()..];
    Some(&rest[..rest.find('"')?])
}

/// Bump `[workspace.package] version` and each `path = "crates/…"` entry of
/// `[workspace.dependencies]`, which must all carry the same version. Returns the new text, the old
/// version and how many internal crates were bumped.
fn bump_manifest(text: &str, new: &str) -> Result<(String, String, usize), String> {
    let mut section = "";
    let mut old: Option<String> = None;
    let mut crates = 0;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = if trimmed == "[workspace.package]" {
                "package"
            } else if trimmed == "[workspace.dependencies]" {
                "deps"
            } else {
                ""
            };
        }
        let ours = (section == "package" && trimmed.starts_with("version")) || (section == "deps" && trimmed.contains("path = \"crates/"));
        match version_on(line).filter(|_| ours) {
            Some(v) => {
                match &old {
                    None => old = Some(v.to_string()),
                    Some(o) if o != v => return Err(format!("Cargo.toml: versions differ ({o} and {v}): {trimmed}")),
                    Some(_) => {}
                }
                if section == "deps" {
                    crates += 1;
                }
                out.push_str(&line.replacen(&format!("version = \"{v}\""), &format!("version = \"{new}\""), 1));
            }
            None => out.push_str(line),
        }
    }
    let old = old.ok_or("Cargo.toml: no [workspace.package] version")?;
    if old == new {
        return Err(format!("the workspace is already at {new}"));
    }
    Ok((out, old, crates))
}

fn bump_changelog(text: &str, old: &str, new: &str, date: &str) -> Result<String, String> {
    const HEAD: &str = "## [Unreleased]\n";
    if text.contains(&format!("## [{new}]")) {
        return Err(format!("CHANGELOG.md already has a [{new}] section"));
    }
    let at = text.find(HEAD).ok_or("CHANGELOG.md: no \"## [Unreleased]\" heading")?;
    let body_start = at + HEAD.len();
    let next = text[body_start..].find("\n## [").map_or(text.len(), |i| body_start + i);
    if text[body_start..next].trim().is_empty() {
        return Err("CHANGELOG.md: \"Unreleased\" is empty, nothing to release".into());
    }
    let mut out = format!("{}## [Unreleased]\n\n## [{new}] - {date}\n{}", &text[..at], &text[body_start..]);

    let old_link = format!("[Unreleased]: {REPO}/compare/v{old}...HEAD");
    if !out.contains(&old_link) {
        return Err(format!("CHANGELOG.md: no link line `{old_link}`"));
    }
    let new_links = format!("[Unreleased]: {REPO}/compare/v{new}...HEAD\n[{new}]: {REPO}/compare/v{old}...v{new}");
    out = out.replacen(&old_link, &new_links, 1);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = "[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
        [workspace.dependencies]\nashlar = { path = \"crates/ashlar\", version = \"0.1.0\", package = \"ashlar-rs\" }\n\
        ashlar-ui = { path = \"crates/ui\", version = \"0.1.0\" }\nserde = { version = \"0.1.0\" }\n";

    #[test]
    fn manifest_bumps_workspace_and_internal_crates_only() {
        let (out, old, n) = bump_manifest(MANIFEST, "0.2.0").unwrap_or_default();
        assert_eq!((old.as_str(), n), ("0.1.0", 2));
        assert_eq!(out.matches("\"0.2.0\"").count(), 3);
        assert!(out.contains("serde = { version = \"0.1.0\" }"));
        assert!(bump_manifest(MANIFEST, "0.1.0").is_err());
        assert!(bump_manifest(&MANIFEST.replacen("crates/ui\", version = \"0.1.0", "crates/ui\", version = \"0.0.9", 1), "0.2.0").is_err());
    }

    #[test]
    fn changelog_moves_unreleased_and_links() {
        let log = format!(
            "# Changelog\n\n## [Unreleased]\n\n### Added\n- x\n\n## [0.1.0] - 2026-10-27\n- y\n\n\
             [Unreleased]: {REPO}/compare/v0.1.0...HEAD\n[0.1.0]: {REPO}/releases/tag/v0.1.0\n"
        );
        let out = bump_changelog(&log, "0.1.0", "0.2.0", "2026-11-11").unwrap_or_default();
        assert!(out.contains("## [Unreleased]\n\n## [0.2.0] - 2026-11-11\n\n### Added\n- x"));
        assert!(out.contains(&format!("[Unreleased]: {REPO}/compare/v0.2.0...HEAD\n[0.2.0]: {REPO}/compare/v0.1.0...v0.2.0\n[0.1.0]")));
        assert!(bump_changelog(&out, "0.1.0", "0.2.0", "2026-11-11").is_err());
        let empty = log.replace("### Added\n- x\n", "");
        assert!(bump_changelog(&empty, "0.1.0", "0.2.0", "2026-11-11").is_err());
    }

    #[test]
    fn checks_and_dates() {
        assert!(is_version("0.2.0") && !is_version("0.2") && !is_version("v0.2.0") && !is_version("0.2.x"));
        assert!(is_date("2026-11-11") && !is_date("2026-1-11") && !is_date("20261111"));
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(20_403), "2025-11-11");
        assert_eq!(civil_date(11_016), "2000-02-29");
    }
}
