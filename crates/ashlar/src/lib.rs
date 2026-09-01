//! Build fast desktop and web apps in Rust.
//!
//! One dependency for documents with undo and autosave, commands that drive menus, shortcuts, a
//! palette, a CLI and an MCP server, background jobs, native OS features, egui components, and a
//! runtime with crash safety, agent control and telemetry. Start with [`prelude`] and
//! [`app::App`]; the `notes` example is a complete app.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub use ashlar_app as app;
pub use ashlar_cli as cli;
pub use ashlar_command as command;
pub use ashlar_control as control;
pub use ashlar_fs as fs;
pub use ashlar_jobs as jobs;
pub use ashlar_mcp as mcp;
pub use ashlar_native as native;
pub use ashlar_store as store;
pub use ashlar_ui as ui;
pub use egui;
pub use serde_json;

/// What most apps need, in one `use ashlar::prelude::*;`.
pub mod prelude {
    pub use ashlar_app::{App, Cx, Document, Driver, Effect, Headless, Session, cli_main};
    pub use ashlar_command::{CommandError, CommandInfo, CommandSpec, Dispatch, Param, Params, always, cmd, query};
    pub use ashlar_jobs::{Jobs, apply};
    pub use ashlar_store::{Format, Revision};
    pub use ashlar_ui::toasts::ToastLevel;
    pub use egui;
    pub use serde_json::{Value, json};
}

/// The repository README's example, compiled as a doc test.
#[cfg(doctest)]
#[doc = include_str!("../../../README.md")]
pub struct ReadmeDoctests;
