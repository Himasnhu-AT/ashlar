//! The machine an install looks at: the home folder, the few environment variables agents use to
//! move their config, the program search path and the application folders. Taken from the process
//! by [`Host::current`]; tests build one around a temporary folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Environment variables that relocate an agent's config, read by [`Host::current`].
pub const VARS: &[&str] = &["CLAUDE_CONFIG_DIR", "CODEX_HOME", "XDG_CONFIG_HOME", "APPDATA", "GEMINI_CLI_HOME"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Mac,
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

#[derive(Debug, Clone)]
pub struct Host {
    pub home: PathBuf,
    /// The values of [`VARS`] that are set and not empty.
    pub vars: BTreeMap<String, String>,
    /// Folders searched for programs (`PATH`, plus the usual install folders a desktop app does
    /// not inherit).
    pub path: Vec<PathBuf>,
    /// Folders that hold applications (`/Applications`, `~/Applications`).
    pub app_dirs: Vec<PathBuf>,
    pub os: Os,
}

impl Host {
    /// This process's machine. `None` without a home folder.
    pub fn current() -> Option<Self> {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).filter(|h| !h.is_empty()).map(PathBuf::from)?;
        let vars = VARS.iter().filter_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()).map(|v| (k.to_string(), v))).collect();
        let mut path: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        // a desktop app launched from the Dock or Finder gets a minimal PATH: add the folders
        // command-line tools are usually installed in
        for extra in ["/usr/local/bin", "/opt/homebrew/bin"] {
            path.push(PathBuf::from(extra));
        }
        for extra in [".local/bin", ".npm-global/bin", ".bun/bin", ".volta/bin", ".cargo/bin", ".claude/local"] {
            path.push(home.join(extra));
        }
        dedup(&mut path);
        let app_dirs = vec![PathBuf::from("/Applications"), home.join("Applications")];
        Some(Self { home, vars, path, app_dirs, os: Os::current() })
    }

    /// A host rooted at `home` with nothing else set (tests).
    pub fn at(home: impl Into<PathBuf>, os: Os) -> Self {
        let home = home.into();
        Self { app_dirs: vec![home.join("Applications")], path: vec![home.join("bin")], home, vars: BTreeMap::new(), os }
    }

    pub fn var(&self, k: &str) -> Option<&str> {
        self.vars.get(k).map(String::as_str)
    }

    /// `name` (or `name.exe` on Windows) in one of the [`path`](Self::path) folders.
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        let names: Vec<String> =
            if self.os == Os::Windows { vec![format!("{name}.exe"), format!("{name}.cmd"), name.to_string()] } else { vec![name.to_string()] };
        self.path.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| is_program(p))
    }

    /// `name.app` in one of the application folders.
    pub fn app(&self, name: &str) -> Option<PathBuf> {
        self.app_dirs.iter().map(|d| d.join(format!("{name}.app"))).find(|p| p.is_dir())
    }

    /// The per-user application settings folder: `~/Library/Application Support` (macOS),
    /// `%APPDATA%` (Windows), `$XDG_CONFIG_HOME` or `~/.config` (Linux).
    pub fn config_dir(&self) -> PathBuf {
        match self.os {
            Os::Mac => self.home.join("Library/Application Support"),
            Os::Windows => self.var("APPDATA").map_or_else(|| self.home.join("AppData/Roaming"), PathBuf::from),
            Os::Linux => self.var("XDG_CONFIG_HOME").map_or_else(|| self.home.join(".config"), PathBuf::from),
        }
    }

    /// `rel` under the home folder.
    pub fn home_join(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }
}

fn dedup(v: &mut Vec<PathBuf>) {
    let mut seen = Vec::new();
    v.retain(|p| {
        let keep = !seen.contains(p);
        seen.push(p.clone());
        keep
    });
}

fn is_program(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.is_file() && m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        m.is_file()
    }
}

/// The command an agent should run for the app's command-line tool `name`: the copy installed on
/// the search path (it follows the app when the app moves), else `fallback` (the tool inside the
/// app). `None` when there is neither.
pub fn cli_path(host: &Host, name: &str, fallback: Option<PathBuf>) -> Option<PathBuf> {
    host.which(name).or_else(|| fallback.filter(|p| is_program(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn finds_programs_and_prefers_the_installed_tool() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let host = Host::at(dir.path(), Os::Mac);
        let bundled = dir.path().join("App.app/Contents/Helpers/tool");
        std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        std::fs::write(&bundled, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bundled, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(cli_path(&host, "tool", Some(bundled.clone())), Some(bundled.clone()));
        assert_eq!(cli_path(&host, "tool", Some(dir.path().join("missing"))), None);
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        let linked = dir.path().join("bin/tool");
        std::fs::write(&linked, "#!/bin/sh\n").unwrap();
        // not executable: not a program
        assert_eq!(host.which("tool"), None);
        std::fs::set_permissions(&linked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(cli_path(&host, "tool", Some(bundled)), Some(linked));
        assert_eq!(host.config_dir(), dir.path().join("Library/Application Support"));
    }
}
