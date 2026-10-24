//! What is installed, on disk.
//!
//! ```text
//! <root>/state.json                 enabled flags, current and previous version of each integration
//! <root>/catalog.json               the last catalog fetched (shown while offline)
//! <root>/downloads/<id>-<v>.zip     a zip being downloaded (.part) or not yet unpacked
//! <root>/<id>/<v>/<id>/...          an unpacked version (the zip holds one <id>/ folder)
//! <root>/<id>/data/                 the integration's data folder (its models), kept across versions
//! ```
//!
//! Installing unpacks into `<v>.tmp`, checks the layout, the entry and the code signature, then
//! renames it into place, so a half-installed version is never run. The current version and one
//! previous version are kept; older ones are deleted.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::catalog::{Entry, Manifest, rel_path_ok};
use crate::download::{self, DownloadError, Http, Progress};
use crate::verify::Verify;

/// One installed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub version: u32,
    /// The catalog entry it was installed from (how to start it).
    pub entry: Entry,
    /// The bundle's own version string, from its manifest.
    #[serde(default)]
    pub display_version: String,
}

/// What the state file keeps per integration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Saved {
    pub enabled: bool,
    pub current: Option<Installed>,
    pub previous: Option<Installed>,
    /// The digest of the asset list that was last downloaded completely.
    #[serde(default)]
    pub assets_ready: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    integrations: BTreeMap<String, Saved>,
}

/// Why an install step failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Download(#[from] DownloadError),
    #[error("the download isn't a valid bundle: {0}")]
    Bundle(String),
    #[error("the bundle's code signature check failed: {0}")]
    Signature(String),
    #[error("{0}")]
    Disk(String),
}

/// The integrations folder and its state file.
#[derive(Debug)]
pub struct Store {
    root: PathBuf,
    state: StateFile,
}

fn disk(path: &Path, e: impl std::fmt::Display) -> InstallError {
    InstallError::Disk(format!("{}: {e}", path.display()))
}

impl Store {
    /// Open (or start) the store at `root`. A damaged state file is set aside and starts empty.
    pub fn open(root: &Path) -> Store {
        let state = match fs::read(root.join("state.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!(target: "integrations", "state.json unreadable ({e}); starting empty");
                let _ = fs::rename(root.join("state.json"), root.join("state.json.bad"));
                StateFile::default()
            }),
            Err(_) => StateFile::default(),
        };
        Store { root: root.to_path_buf(), state }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn save(&self) -> Result<(), InstallError> {
        let bytes = serde_json::to_vec_pretty(&self.state).map_err(|e| InstallError::Disk(e.to_string()))?;
        fs::create_dir_all(&self.root).map_err(|e| disk(&self.root, e))?;
        ashlar_fs::atomic_write(&self.root.join("state.json"), &bytes).map_err(|e| disk(&self.root, e))
    }

    pub fn get(&self, id: &str) -> Option<&Saved> {
        self.state.integrations.get(id)
    }

    pub fn ids(&self) -> Vec<String> {
        self.state.integrations.keys().cloned().collect()
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), InstallError> {
        let Some(saved) = self.state.integrations.get_mut(id) else {
            return Err(InstallError::Disk(format!("{id} isn't installed")));
        };
        saved.enabled = enabled;
        self.save()
    }

    pub fn set_assets_ready(&mut self, id: &str, digest: Option<String>) -> Result<(), InstallError> {
        if let Some(saved) = self.state.integrations.get_mut(id) {
            saved.assets_ready = digest;
        }
        self.save()
    }

    /// `<root>/<id>`.
    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// `<root>/<id>/data`, created on demand.
    pub fn data_dir(&self, id: &str) -> PathBuf {
        self.dir(id).join("data")
    }

    /// `<root>/<id>/<version>`.
    pub fn version_dir(&self, id: &str, version: u32) -> PathBuf {
        self.dir(id).join(version.to_string())
    }

    /// The executable of an installed version.
    pub fn entry_path(&self, installed: &Installed) -> PathBuf {
        self.version_dir(&installed.entry.id, installed.version).join(&installed.entry.entry)
    }

    /// The manifest of an installed version (empty when the bundle has none).
    pub fn manifest(&self, installed: &Installed) -> Result<Manifest, InstallError> {
        read_manifest(&self.version_dir(&installed.entry.id, installed.version), &installed.entry.id)
    }

    /// Record a version unpacked by [`prepare`]: it becomes current, the old current becomes previous,
    /// and any older version folder is deleted. New installs start enabled.
    pub fn commit(&mut self, installed: Installed) -> Result<(), InstallError> {
        let id = installed.entry.id.clone();
        let saved = self.state.integrations.entry(id.clone()).or_insert_with(|| Saved { enabled: true, ..Saved::default() });
        if saved.current.as_ref().is_some_and(|c| c.version == installed.version) {
            saved.current = Some(installed);
        } else {
            saved.previous = saved.current.take();
            saved.current = Some(installed);
        }
        let keep: Vec<u32> = [saved.current.as_ref(), saved.previous.as_ref()].into_iter().flatten().map(|i| i.version).collect();
        self.save()?;
        self.prune(&id, &keep);
        Ok(())
    }

    /// Make the previous version current again (and forget the one that was current).
    pub fn rollback(&mut self, id: &str) -> Result<u32, InstallError> {
        let Some(saved) = self.state.integrations.get_mut(id) else {
            return Err(InstallError::Disk(format!("{id} isn't installed")));
        };
        let Some(previous) = saved.previous.take() else {
            return Err(InstallError::Disk(format!("{id} has no previous version")));
        };
        let version = previous.version;
        saved.current = Some(previous);
        self.save()?;
        self.prune(id, &[version]);
        Ok(version)
    }

    /// Delete everything of `id`: its versions, its data (models) and its state.
    pub fn remove(&mut self, id: &str) -> Result<(), InstallError> {
        let dir = self.dir(id);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| disk(&dir, e))?;
        }
        if let Ok(list) = fs::read_dir(self.root.join("downloads")) {
            for f in list.flatten() {
                if f.file_name().to_string_lossy().starts_with(&format!("{id}-")) {
                    let _ = fs::remove_file(f.path());
                }
            }
        }
        self.state.integrations.remove(id);
        self.save()
    }

    /// Delete version folders of `id` other than `keep` (and leftover `.tmp` folders).
    fn prune(&self, id: &str, keep: &[u32]) {
        let Ok(list) = fs::read_dir(self.dir(id)) else { return };
        for f in list.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if name == "data" {
                continue;
            }
            let wanted = name.parse::<u32>().is_ok_and(|v| keep.contains(&v));
            if !wanted
                && f.path().is_dir()
                && let Err(e) = fs::remove_dir_all(f.path())
            {
                log::warn!(target: "integrations", "couldn't delete {}: {e}", f.path().display());
            }
        }
    }
}

/// The manifest in an unpacked version folder, checked; a bundle without one gets an empty manifest.
pub fn read_manifest(version_dir: &Path, id: &str) -> Result<Manifest, InstallError> {
    let path = version_dir.join(id).join("integration.json");
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Manifest::default()),
        Err(e) => return Err(disk(&path, e)),
    };
    let m = Manifest::parse(&bytes).map_err(|e| InstallError::Bundle(e.0))?;
    if !m.id.is_empty() && m.id != id {
        return Err(InstallError::Bundle(format!("its manifest is for {:?}, not {id:?}", m.id.chars().take(40).collect::<String>())));
    }
    Ok(m)
}

/// Download, unpack and check `entry` into `<root>/<id>/<version>`; returns what [`Store::commit`] records.
/// `team_id`, when given, must match the manifest's (a manifest that names another team is refused).
pub fn prepare(
    root: &Path,
    entry: &Entry,
    http: &dyn Http,
    verify: &dyn Verify,
    team_id: Option<&str>,
    progress: Progress<'_>,
    cancel: &AtomicBool,
) -> Result<Installed, InstallError> {
    entry.check().map_err(|e| InstallError::Bundle(e.0))?;
    let downloads = root.join("downloads");
    let zip = downloads.join(format!("{}-{}.zip", entry.id, entry.version));
    let mut urls = vec![entry.url.clone()];
    urls.extend(entry.mirror.clone());
    download::download(http, &urls, &zip, entry.size, &entry.sha256, progress, cancel)?;

    let final_dir = root.join(&entry.id).join(entry.version.to_string());
    let tmp = root.join(&entry.id).join(format!("{}.tmp", entry.version));
    if tmp.exists() {
        fs::remove_dir_all(&tmp).map_err(|e| disk(&tmp, e))?;
    }
    fs::create_dir_all(&tmp).map_err(|e| disk(&tmp, e))?;
    let result = (|| {
        unzip(&zip, &tmp)?;
        check_layout(&tmp, &entry.id)?;
        let exe = fs::canonicalize(tmp.join(&entry.entry)).map_err(|_| InstallError::Bundle(format!("{} is missing", entry.entry)))?;
        let manifest = read_manifest(&tmp, &entry.id)?;
        if let (Some(want), Some(has)) = (team_id, manifest.team_id.as_deref())
            && want != has
        {
            return Err(InstallError::Signature(format!("the bundle says team {has}, expected {want}")));
        }
        verify.verify(&tmp, &exe).map_err(InstallError::Signature)?;
        Ok(manifest)
    })();
    let manifest = match result {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            // A zip that unpacks but fails the checks is no use: download it again next time.
            if !matches!(e, InstallError::Disk(_)) {
                let _ = fs::remove_file(&zip);
            }
            return Err(e);
        }
    };
    if final_dir.exists() {
        fs::remove_dir_all(&final_dir).map_err(|e| disk(&final_dir, e))?;
    }
    fs::rename(&tmp, &final_dir).map_err(|e| disk(&final_dir, e))?;
    let _ = fs::remove_file(&zip);
    Ok(Installed { version: entry.version, entry: entry.clone(), display_version: manifest.version })
}

/// The zip must hold exactly one top-level folder, named after the id (macOS metadata aside).
fn check_layout(dir: &Path, id: &str) -> Result<(), InstallError> {
    let mut names = Vec::new();
    for f in fs::read_dir(dir).map_err(|e| disk(dir, e))?.flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if name == "__MACOSX" || name == ".DS_Store" {
            let _ = if f.path().is_dir() { fs::remove_dir_all(f.path()) } else { fs::remove_file(f.path()) };
            continue;
        }
        names.push(name);
    }
    if names != [id] || !dir.join(id).is_dir() {
        return Err(InstallError::Bundle(format!("expected one folder {id:?} at the top, found {names:?}")));
    }
    Ok(())
}

/// Unpack with `ditto`, which keeps code signatures, permissions and links.
fn unzip(zip: &Path, into: &Path) -> Result<(), InstallError> {
    if !cfg!(target_os = "macos") {
        return Err(InstallError::Bundle("bundles can only be unpacked on macOS".into()));
    }
    let out = Command::new("/usr/bin/ditto")
        .args(["-x", "-k", "--noqtn"])
        .arg(zip)
        .arg(into)
        .output()
        .map_err(|e| InstallError::Disk(format!("couldn't run ditto: {e}")))?;
    if !out.status.success() {
        return Err(InstallError::Bundle(format!("couldn't unpack: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(())
}

/// A digest of an asset list: equal digests mean the same files.
pub fn assets_digest(m: &Manifest) -> String {
    let mut h = Sha256::new();
    for a in &m.assets {
        h.update(a.path.as_bytes());
        h.update([0]);
        h.update(a.sha256.as_bytes());
        h.update([0]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The assets that are missing or have the wrong size in `data`.
pub fn missing_assets<'a>(m: &'a Manifest, data: &Path) -> Vec<&'a crate::catalog::Asset> {
    m.assets.iter().filter(|a| !fs::metadata(data.join(&a.path)).is_ok_and(|md| md.is_file() && md.len() == a.size)).collect()
}

/// Download the missing assets into `data`; `progress` gets (bytes done, bytes total) over all of them.
pub fn fetch_assets(
    m: &Manifest,
    data: &Path,
    http: &dyn Http,
    progress: &(dyn Fn(u64, u64) + Sync),
    cancel: &AtomicBool,
) -> Result<(), InstallError> {
    let total = m.assets_size();
    let missing = missing_assets(m, data);
    let mut done = total.saturating_sub(missing.iter().fold(0u64, |n, a| n.saturating_add(a.size)));
    progress(done, total);
    for a in missing {
        if !rel_path_ok(&a.path) {
            return Err(InstallError::Bundle(format!("bad asset path {:?}", a.path)));
        }
        let dest = data.join(&a.path);
        let base = done;
        download::download(http, &a.urls, &dest, a.size, &a.sha256, &|n| progress(base.saturating_add(n), total), cancel)?;
        done = done.saturating_add(a.size);
    }
    progress(total, total);
    Ok(())
}
