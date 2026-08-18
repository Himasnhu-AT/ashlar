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

    /// Create the four directories (and their parents) if they don't exist.
    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [&self.config, &self.data, &self.cache, &self.logs] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
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
    fn ensure_creates_all() {
        let root = std::env::temp_dir().join(format!("ashlar-dirs-{}", std::process::id()));
        let d = AppDirs::under(&root);
        d.ensure().unwrap();
        assert!(d.config.is_dir() && d.data.is_dir() && d.cache.is_dir() && d.logs.is_dir());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
