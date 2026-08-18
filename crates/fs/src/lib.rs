//! Files and settings: app dirs, durable atomic writes, keyed settings, recent files.
//!
//! - [`AppDirs`]: where an app keeps its config, data, cache and logs on each OS.
//! - [`atomic_write`]: replace a file so a crash or power cut leaves the old or the new content,
//!   never a torn mix.
//! - [`ConfigStore`] / [`FileOps`]: the storage seen by settings, autosave and recovery, with disk
//!   and in-memory backends (browser storage plugs in here).
//! - [`Settings`]: keyed JSON with typed getters, a schema version and a debounced durable save.
//! - [`RecentFiles`]: a most-recently-used list.
//!
//! Headless and wasm32-clean: on the web the disk backends compile but report errors; use the
//! in-memory ones or a browser-storage backend.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod atomic;
pub mod backend;
pub mod dirs;
pub mod recent;
pub mod settings;

pub use atomic::{atomic_write, atomic_write_with};
pub use backend::{ConfigStore, DirConfig, FileOps, MemFiles, MemoryConfig, StdFiles};
pub use dirs::AppDirs;
pub use recent::RecentFiles;
pub use settings::{LoadState, Settings, SettingsMigration};
