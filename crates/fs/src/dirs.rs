//! Per-app directories (config, data, cache, logs) in the places each OS expects.
//!
//! | | macOS | Windows | Linux |
//! |---|---|---|---|
//! | config | `~/Library/Application Support/<id>` | `%APPDATA%\<org>\<app>\config` | `$XDG_CONFIG_HOME/<app>` |
//! | data | `~/Library/Application Support/<id>` | `%APPDATA%\<org>\<app>\data` | `$XDG_DATA_HOME/<app>` |
//! | cache | `~/Library/Caches/<id>` | `%LOCALAPPDATA%\<org>\<app>\cache` | `$XDG_CACHE_HOME/<app>` |
//! | logs | `~/Library/Logs/<id>` | `%LOCALAPPDATA%\<org>\<app>\data\logs` | `$XDG_STATE_HOME/<app>/logs` |
//!
//! `<id>` is `qualifier.org.app` as the `directories` crate builds it. Setting the
//! [`ROOT_ENV`] environment variable puts all four under one folder instead (tests, portable
//! installs, CI).
//!
//! Apps that already keep their data in the conventional *flat* per-app folder (no
//! organisation, no `data` subfolder) find it with [`AppDirs::data_only`]:
//!
//! | macOS | Windows | Linux / BSD |
//! |---|---|---|
//! | `~/Library/Application Support/<App>` | `%APPDATA%\<App>` | `$XDG_DATA_HOME/<app>`, else `~/.local/share/<app>` |
//!
//! (`<app>` is the name in lower case.) The app's own override variable wins there;
//! [`ROOT_ENV`] does not move it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Environment variable that overrides every app directory: `<root>/{config,data,cache,logs}`.
pub const ROOT_ENV: &str = "ASHLAR_DIRS_ROOT";

/// The directories of one app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDirs {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
}

impl AppDirs {
    /// The platform directories for an app (`qualifier` is a reverse-domain prefix such as `com`,
    /// `org` the organisation, `app` the application name). [`ROOT_ENV`] wins when set and not
    /// empty. `None` when the platform has no home directory (the browser, some sandboxes).
    pub fn new(qualifier: &str, org: &str, app: &str) -> Option<AppDirs> {
        match std::env::var_os(ROOT_ENV) {
            Some(root) if !root.is_empty() => Some(AppDirs::under(root)),
            _ => platform(qualifier, org, app),
        }
    }

    /// All four directories under `root` (`root/config`, `root/data`, …).
    pub fn under(root: impl AsRef<Path>) -> AppDirs {
        let root = root.as_ref();
        AppDirs { config: root.join("config"), data: root.join("data"), cache: root.join("cache"), logs: root.join("logs") }
    }

    /// The flat per-app data directory (see the module docs): `override_env` (e.g.
    /// `"ACME_DATA_DIR"`) wins when set and not empty; [`ROOT_ENV`] is ignored. `None` without a
    /// home directory. Not created.
    pub fn data_only(app: &str, override_env: &str) -> Option<PathBuf> {
        data_dir_for(std::env::consts::OS, app, override_env, |k| std::env::var_os(k))
    }

    /// Create the four directories (and their parents) if they don't exist.
    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [&self.config, &self.data, &self.cache, &self.logs] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }
}

/// [`AppDirs::data_only`] for the OS named `os` (as in [`std::env::consts::OS`]), reading
/// environment variables through `var`. Empty variables count as unset; a relative
/// `XDG_DATA_HOME` is used as it is.
pub fn data_dir_for(os: &str, app: &str, override_env: &str, var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let env = |k: &str| var(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(d) = env(override_env) {
        return Some(d);
    }
    match os {
        "macos" => env("HOME").map(|h| h.join("Library/Application Support").join(app)),
        "windows" => env("APPDATA").map(|d| d.join(app)),
        _ => {
            let app = app.to_lowercase();
            env("XDG_DATA_HOME").map(|d| d.join(&app)).or_else(|| env("HOME").map(|h| h.join(".local/share").join(&app)))
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn platform(qualifier: &str, org: &str, app: &str) -> Option<AppDirs> {
    let p = directories::ProjectDirs::from(qualifier, org, app)?;
    let logs = logs_dir(&p)?;
    Some(AppDirs { config: p.config_dir().to_path_buf(), data: p.data_dir().to_path_buf(), cache: p.cache_dir().to_path_buf(), logs })
}

#[cfg(target_arch = "wasm32")]
fn platform(_qualifier: &str, _org: &str, _app: &str) -> Option<AppDirs> {
    None
}

#[cfg(target_os = "macos")]
fn logs_dir(p: &directories::ProjectDirs) -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(home.join("Library/Logs").join(p.project_path()))
}

#[cfg(all(not(target_arch = "wasm32"), not(target_os = "macos")))]
fn logs_dir(p: &directories::ProjectDirs) -> Option<PathBuf> {
    Some(p.state_dir().unwrap_or_else(|| p.data_local_dir()).join("logs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_root() {
        let d = AppDirs::under("/r");
        assert_eq!(d.config, Path::new("/r/config"));
        assert_eq!(d.logs, Path::new("/r/logs"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn platform_dirs_are_distinct_and_named_after_the_app() {
        let Some(d) = platform("com", "Example Org", "Demo App") else { return };
        for p in [&d.cache, &d.logs] {
            assert!(p.to_string_lossy().contains("Demo"), "{p:?}");
        }
        assert_ne!(d.cache, d.data);
        assert_ne!(d.logs, d.data);
    }

    #[test]
    fn flat_data_dir_paths_are_pinned() {
        let dir = |os: &str, vars: &[(&str, &str)]| {
            let vars: Vec<(String, String)> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            data_dir_for(os, "Demo", "DEMO_DATA_DIR", |k| vars.iter().find(|(n, _)| n == k).map(|(_, v)| v.into()))
        };
        let home = [("HOME", "/home/u"), ("APPDATA", "C:/Users/u/AppData/Roaming"), ("XDG_DATA_HOME", "/xdg")];
        assert_eq!(dir("macos", &home), Some(PathBuf::from("/home/u/Library/Application Support/Demo")));
        assert_eq!(dir("windows", &home), Some(PathBuf::from("C:/Users/u/AppData/Roaming").join("Demo")));
        assert_eq!(dir("linux", &home), Some(PathBuf::from("/xdg/demo")));
        assert_eq!(dir("freebsd", &[("HOME", "/home/u")]), Some(PathBuf::from("/home/u/.local/share/demo")));
        assert_eq!(dir("linux", &[("HOME", "/home/u"), ("XDG_DATA_HOME", "")]), Some(PathBuf::from("/home/u/.local/share/demo")));
        assert_eq!(dir("linux", &[("XDG_DATA_HOME", "rel")]), Some(PathBuf::from("rel/demo")), "a relative XDG_DATA_HOME is used as is");
        for os in ["macos", "windows", "linux"] {
            assert_eq!(dir(os, &[]), None, "{os}: no home, no data directory");
            let mut with_override = home.to_vec();
            with_override.push(("DEMO_DATA_DIR", "/custom"));
            assert_eq!(dir(os, &with_override), Some(PathBuf::from("/custom")), "{os}");
            with_override.push((ROOT_ENV, "/elsewhere"));
            assert_eq!(dir(os, &with_override), Some(PathBuf::from("/custom")), "{os}: the root override does not move it");
            assert_eq!(dir(os, &[(ROOT_ENV, "/elsewhere")]), None, "{os}");
            assert!(
                dir(os, &[("DEMO_DATA_DIR", ""), ("HOME", "/h"), ("APPDATA", "/h"), ("XDG_DATA_HOME", "/h")]).is_some(),
                "{os}: an empty override is unset"
            );
        }
        assert_eq!(AppDirs::data_only("Demo", "DEMO_DATA_DIR"), data_dir_for(std::env::consts::OS, "Demo", "DEMO_DATA_DIR", |k| std::env::var_os(k)));
    }

    #[test]
    fn ensure_creates_all() {
        let root = tempfile::tempdir().unwrap();
        let d = AppDirs::under(root.path());
        d.ensure().unwrap();
        assert!(d.config.is_dir() && d.data.is_dir() && d.cache.is_dir() && d.logs.is_dir());
    }
}
