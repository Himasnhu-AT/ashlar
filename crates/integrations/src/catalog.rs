//! The catalog an app fetches (`{"integrations": [...]}`), and the manifest inside a bundle
//! (`integration.json`).
//!
//! The catalog says where a build is, how big it is, its sha256 and how to start it. The manifest,
//! which comes inside the zip the catalog's sha256 vouches for, adds what the catalog doesn't carry:
//! files to download before the first start (models, kept across versions in the integration's data
//! folder), extra environment variables, timeouts, tools to hide. In arguments and environment values
//! `${data}` stands for the data folder and `${bundle}` for the unpacked version folder.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Largest catalog answer read.
pub const MAX_CATALOG: usize = 1024 * 1024;
/// Largest manifest read.
pub const MAX_MANIFEST: usize = 1024 * 1024;
/// Largest zip accepted (4 GiB).
pub const MAX_ZIP: u64 = 4 * 1024 * 1024 * 1024;
/// Largest single asset (16 GiB).
pub const MAX_ASSET: u64 = 16 * 1024 * 1024 * 1024;
/// Most assets in a manifest.
pub const MAX_ASSETS: usize = 256;

/// One build in the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    /// Whole numbers: a higher one is newer.
    pub version: u32,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub arch: String,
    /// Where the zip is. Empty when the catalog doesn't publish links: a [`crate::Links`] then asks for
    /// one at install time (a link signed for the signed-in account).
    #[serde(default)]
    pub url: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub mirror: Option<String>,
    /// The executable, relative to where the zip is unpacked (`<id>/<file>`).
    pub entry: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub min_app_version: String,
    #[serde(default)]
    pub licence: String,
    #[serde(default)]
    pub homepage: Option<String>,
    /// Bytes downloaded on first use (models), when known.
    #[serde(default)]
    pub models_size: Option<u64>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// Why a catalog or manifest was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Invalid(pub String);

/// `[a-z0-9-]`, starts with a letter, 1–40 bytes: safe as a folder name and in tool names (no `__`).
pub fn id_ok(id: &str) -> bool {
    (1..=40).contains(&id.len())
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.contains("--")
}

/// `https://`, or `http://` to the loopback (tests, a local server).
pub fn url_ok(url: &str) -> bool {
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    if url.strip_prefix("https://").is_some_and(|rest| !rest.is_empty()) {
        return true;
    }
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let host = rest.split('/').next().unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

fn sha_ok(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A relative path with only normal components (no `..`, no root, no `.`), at most 512 bytes.
pub fn rel_path_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 512
        && !p.contains('\\')
        && p.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && Path::new(p).components().all(|c| matches!(c, Component::Normal(_)))
}

impl Entry {
    /// Refuse what the manager can't install safely.
    pub fn check(&self) -> Result<(), Invalid> {
        let bad = |what: &str| Err(Invalid(format!("integration {:?}: {what}", self.id.chars().take(40).collect::<String>())));
        if !id_ok(&self.id) {
            return bad("bad id");
        }
        if self.version == 0 {
            return bad("version 0");
        }
        if (!self.url.is_empty() && !url_ok(&self.url)) || self.mirror.as_deref().is_some_and(|m| !url_ok(m)) {
            return bad("download URLs must be https://");
        }
        if !sha_ok(&self.sha256) {
            return bad("sha256 isn't 64 lower-case hex digits");
        }
        if self.size == 0 || self.size > MAX_ZIP {
            return bad("size out of range");
        }
        if !rel_path_ok(&self.entry) || !self.entry.starts_with(&format!("{}/", self.id)) {
            return bad("entry must be a path inside the bundle's <id>/ folder");
        }
        if self.args.len() > 32 || self.args.iter().any(|a| a.len() > 1024) {
            return bad("too many or too long arguments");
        }
        Ok(())
    }

    /// Runs on this platform and architecture (`universal` runs on any).
    pub fn runs_on(&self, platform: &str, arch: &str) -> bool {
        (self.platform.is_empty() || self.platform == platform) && (self.arch.is_empty() || self.arch == arch || self.arch == "universal")
    }
}

/// The catalog: every valid entry for this platform, the highest version of each id. Invalid entries are
/// logged and skipped, so one bad row doesn't hide the others.
pub fn parse_catalog(body: &[u8], platform: &str, arch: &str) -> Result<Vec<Entry>, Invalid> {
    if body.len() > MAX_CATALOG {
        return Err(Invalid("the catalog is too large".into()));
    }
    let v: Value = serde_json::from_slice(body).map_err(|e| Invalid(format!("the catalog isn't JSON: {e}")))?;
    let Some(rows) = v.get("integrations").and_then(Value::as_array) else {
        return Err(Invalid("the catalog has no integrations list".into()));
    };
    let mut out: Vec<Entry> = Vec::new();
    for row in rows {
        let entry = match serde_json::from_value::<Entry>(row.clone()) {
            Ok(e) => e,
            Err(e) => {
                log::warn!(target: "integrations", "catalog entry skipped: {e}");
                continue;
            }
        };
        if let Err(e) = entry.check() {
            log::warn!(target: "integrations", "catalog entry skipped: {e}");
            continue;
        }
        if !entry.runs_on(platform, arch) {
            continue;
        }
        match out.iter_mut().find(|e| e.id == entry.id) {
            Some(existing) if existing.version >= entry.version => {}
            Some(existing) => *existing = entry,
            None => out.push(entry),
        }
    }
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

/// One file to download into the data folder before the first start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// Relative to the data folder.
    pub path: String,
    pub size: u64,
    pub sha256: String,
    /// Tried in order.
    pub urls: Vec<String>,
}

/// `integration.json` at the bundle's root folder (`<id>/integration.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub id: String,
    /// The bundle's own version, for people ("0.4.0-1").
    #[serde(default)]
    pub version: String,
    /// The team id its code is signed with, when it says (must match the app's expectation).
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub startup_timeout_secs: Option<u64>,
    #[serde(default)]
    pub call_timeout_secs: Option<u64>,
    #[serde(default)]
    pub hidden_tools: Vec<String>,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

/// Environment variables a manifest may not set: they would change how the child loads code.
const RESERVED_ENV: &[&str] = &["PATH", "HOME", "TMPDIR", "DYLD_", "LD_", "PYTHON"];

impl Manifest {
    pub fn parse(body: &[u8]) -> Result<Manifest, Invalid> {
        if body.len() > MAX_MANIFEST {
            return Err(Invalid("integration.json is too large".into()));
        }
        let m: Manifest = serde_json::from_slice(body).map_err(|e| Invalid(format!("integration.json: {e}")))?;
        if m.assets.len() > MAX_ASSETS {
            return Err(Invalid("integration.json lists too many assets".into()));
        }
        for a in &m.assets {
            if !rel_path_ok(&a.path) || !sha_ok(&a.sha256) || a.size > MAX_ASSET || a.urls.is_empty() || a.urls.iter().any(|u| !url_ok(u)) {
                return Err(Invalid(format!("integration.json: bad asset {:?}", a.path.chars().take(80).collect::<String>())));
            }
        }
        for k in m.env.keys() {
            if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') || RESERVED_ENV.iter().any(|r| k.starts_with(r)) {
                return Err(Invalid(format!("integration.json: environment variable {k:?} isn't allowed")));
            }
        }
        Ok(m)
    }

    /// Total bytes of the assets.
    pub fn assets_size(&self) -> u64 {
        self.assets.iter().fold(0u64, |n, a| n.saturating_add(a.size))
    }
}

/// `${data}` and `${bundle}` replaced.
pub fn expand(s: &str, data: &Path, bundle: &Path) -> String {
    s.replace("${data}", &data.to_string_lossy()).replace("${bundle}", &bundle.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn entry_json(id: &str, version: u32) -> Value {
        serde_json::json!({
            "id": id, "app": "app", "title": id, "summary": "s", "version": version, "platform": "macos", "arch": "arm64",
            "url": "https://downloads.example.com/x.zip", "sha256": "a".repeat(64), "size": 10, "mirror": null,
            "entry": format!("{id}/{id}"), "args": ["--home", "${data}", "mcp"], "min_app_version": "0.1.0",
            "licence": "MIT", "homepage": null, "models_size": 5, "notes": null, "published_at": 1
        })
    }

    #[test]
    fn catalogs_keep_the_newest_valid_build_for_this_machine() {
        let mut bad = entry_json("bad", 1);
        bad["url"] = "http://evil.example.com/x.zip".into();
        let mut intel = entry_json("intel", 1);
        intel["arch"] = "x86_64".into();
        let mut universal = entry_json("uni", 1);
        universal["arch"] = "universal".into();
        let mut escape = entry_json("escape", 1);
        escape["entry"] = "escape/../../bin/sh".into();
        let mut unlinked = entry_json("nolink", 1);
        unlinked.as_object_mut().unwrap().remove("url");
        let body = serde_json::json!({"integrations": [entry_json("b", 1), entry_json("b", 3), entry_json("b", 2), bad, intel, universal, escape, unlinked, {"id": 5}]});
        let list = parse_catalog(body.to_string().as_bytes(), "macos", "arm64").unwrap();
        let ids: Vec<_> = list.iter().map(|e| (e.id.as_str(), e.version)).collect();
        assert_eq!(ids, [("b", 3), ("nolink", 1), ("uni", 1)]);
        assert!(parse_catalog(b"[]", "macos", "arm64").is_err());
    }

    #[test]
    fn ids_and_urls() {
        assert!(id_ok("listen") && id_ok("a-1"));
        assert!(!id_ok("A") && !id_ok("1a") && !id_ok("a__b") && !id_ok("a--b") && !id_ok("a/b") && !id_ok(""));
        assert!(url_ok("https://x.example.com/a") && url_ok("http://127.0.0.1:9/a") && url_ok("http://localhost/a"));
        assert!(!url_ok("http://example.com/a") && !url_ok("file:///etc/passwd") && !url_ok("https://a b"));
        assert!(rel_path_ok("hf/models--a/snapshots/1/config.json"));
        assert!(!rel_path_ok("../x") && !rel_path_ok("/x") && !rel_path_ok("a/./b") && !rel_path_ok(""));
    }

    #[test]
    fn manifests_are_checked() {
        let ok = serde_json::json!({
            "id": "e", "version": "0.4.0-1", "env": {"TOOL_THREADS": "4"}, "hidden_tools": ["upload"],
            "assets": [{"path": "hf/a/model.bin", "size": 3, "sha256": "b".repeat(64), "urls": ["https://x.example.com/a"]}]
        });
        let m = Manifest::parse(ok.to_string().as_bytes()).unwrap();
        assert_eq!(m.assets_size(), 3);
        for bad in [
            serde_json::json!({"env": {"DYLD_INSERT_LIBRARIES": "x"}}),
            serde_json::json!({"env": {"PATH": "/tmp"}}),
            serde_json::json!({"assets": [{"path": "../x", "size": 1, "sha256": "b".repeat(64), "urls": ["https://x.example.com"]}]}),
            serde_json::json!({"assets": [{"path": "x", "size": 1, "sha256": "b".repeat(64), "urls": []}]}),
        ] {
            assert!(Manifest::parse(bad.to_string().as_bytes()).is_err(), "{bad}");
        }
        assert_eq!(expand("--home=${data}", Path::new("/d"), Path::new("/b")), "--home=/d");
    }
}
