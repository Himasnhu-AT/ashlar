//! Everything is a command.
//!
//! An app declares its commands once ([`CommandSpec`]) in a [`Registry`]. Menus, shortcuts, the
//! command palette, the CLI, the control port and MCP all reach the app through one chokepoint,
//! [`Registry::execute`], and discover it through [`Dispatch::commands`].
//!
//! **Contract (shared with ashlar-control, ashlar-mcp, ashlar-cli, ashlar-app):** the items marked
//! `CONTRACT` below are depended on by sibling crates being built in parallel. Change them only by
//! adding (new fields behind `#[serde(default)]`, new trait methods with defaults).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// CONTRACT: why a command failed. Hosts wrap their own errors in [`CommandError::Failed`].
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandError {
    #[error("unknown command `{id}`")]
    Unknown { id: String },
    #[error("command `{id}` is not available right now: {reason}")]
    Disabled { id: String, reason: String },
    #[error("invalid parameters for `{id}`: {msg}")]
    BadParams { id: String, msg: String },
    #[error("{msg}")]
    Failed { id: String, msg: String },
    #[error("internal error in `{id}` (see the crash log); the command did not complete")]
    Panicked { id: String },
}

/// CONTRACT: the kind of one typed parameter. Maps 1:1 onto JSON Schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParamKind {
    Bool,
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    Float {
        min: Option<f64>,
        max: Option<f64>,
    },
    String,
    /// One of a fixed set of strings.
    Enum {
        values: Vec<String>,
    },
    /// Free JSON (object, array…) described in `about`.
    Json,
}

/// CONTRACT: one typed parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    pub name: String,
    pub kind: ParamKind,
    pub required: bool,
    pub about: String,
}

/// How a command documents its parameters: typed (preferred, gives JSON Schema and validation) or the
/// legacy free-text description older command tables use (accepted, validated loosely).
#[derive(Debug, Clone, PartialEq)]
pub enum Params {
    None,
    Typed(Vec<ParamSpec>),
    Text(&'static str),
}

/// CONTRACT: what discovery returns for one command (palette, menus, MCP tools, CLI `commands`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandInfo {
    pub id: String,
    pub label: String,
    /// Menu path, e.g. `["File", "Export"]`. Empty = not in menus.
    #[serde(default)]
    pub menu: Vec<String>,
    /// Portable shortcut, `Mod+Shift+S` (`Mod` = ⌘ on macOS, Ctrl elsewhere).
    #[serde(default)]
    pub shortcut: Option<String>,
    /// JSON Schema (`{"type":"object","properties":…}`) for the parameters.
    #[serde(default)]
    pub params_schema: Value,
    /// Free-text parameter description when the command is not typed yet.
    #[serde(default)]
    pub params_text: Option<String>,
    pub enabled: bool,
    #[serde(default)]
    pub disabled_reason: Option<String>,
    /// Changes the document (journaled, undoable) vs a pure query / view command.
    #[serde(default)]
    pub mutates: bool,
}

/// CONTRACT: anything that can list and run commands. Implemented by `Registry`-backed sessions,
/// by the control-port client (remote app) and by test doubles. Object-safe.
pub trait Dispatch {
    fn commands(&self) -> Vec<CommandInfo>;
    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError>;
}

// ---------------------------------------------------------------------------------------------
// Registry: implementation owned by the ashlar-command agent. The signatures below are the intended
// shape; extend freely, keep `CommandSpec` constructible in a `static`/`const`-friendly way so the
// existing `cmd!` tables port mechanically.
// ---------------------------------------------------------------------------------------------

/// Enabled predicate: `Err(reason)` disables with a human-readable reason.
pub type Enabled<S> = fn(&S) -> Result<(), String>;
/// Command body.
pub type Run<S> = fn(&mut S, &Value) -> Result<Value, CommandError>;

/// One command of host state `S`.
pub struct CommandSpec<S> {
    pub id: &'static str,
    pub label: &'static str,
    pub menu: &'static [&'static str],
    pub shortcut: Option<&'static str>,
    pub params: Params,
    pub enabled: Enabled<S>,
    pub run: Run<S>,
    /// Record in the journal / history (false for pure queries and view commands).
    pub mutates: bool,
}

/// Hooks a host gives the chokepoint (a video editor's `Session::execute` needs all of these: name
/// resolution before, trim-playback settle before, persistence sync after, secret scrubbing).
pub trait Host: Sized {
    /// Rewrite params before validation (resolve names to ids…).
    fn before(&mut self, _spec: &CommandSpec<Self>, _params: &mut Value) -> Result<(), CommandError> {
        Ok(())
    }
    /// Enabled check; override when explicit targets in params change availability.
    fn enabled_for(&self, spec: &CommandSpec<Self>, _params: &Value) -> Result<(), String> {
        (spec.enabled)(self)
    }
    /// Called after every run (ok or err), at every nesting depth.
    fn after(&mut self, _spec: &CommandSpec<Self>, _params: &Value, _result: &Result<Value, CommandError>) {}
    /// Remove secrets from params before they reach the journal.
    fn scrub(&self, _id: &str, _params: &mut Value) {}
}

/// The command table of one app.
pub struct Registry<S: 'static> {
    specs: Vec<CommandSpec<S>>,
}

impl<S: Host + 'static> Registry<S> {
    pub fn new(specs: Vec<CommandSpec<S>>) -> Self {
        Self { specs }
    }
    pub fn find(&self, id: &str) -> Option<&CommandSpec<S>> {
        self.specs.iter().find(|c| c.id == id)
    }
    pub fn specs(&self) -> &[CommandSpec<S>] {
        &self.specs
    }
}
