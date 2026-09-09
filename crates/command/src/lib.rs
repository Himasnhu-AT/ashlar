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

/// Why a call through [`Registry::execute_host`] failed: the chokepoint refused or lost it
/// (`Command`: unknown, too deeply nested, [`Host::before`], validation, disabled, panicked), or
/// the command body returned the host's own error (`Host`, unchanged, as callers match on it).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ExecError<E> {
    #[error(transparent)]
    Command(CommandError),
    #[error("{0}")]
    Host(E),
}

impl<E> ExecError<E> {
    /// The host's error, when the command body returned one.
    pub fn host(&self) -> Option<&E> {
        match self {
            Self::Host(e) => Some(e),
            Self::Command(_) => None,
        }
    }
    /// The chokepoint's error, when the call never ran or panicked.
    pub fn command(&self) -> Option<&CommandError> {
        match self {
            Self::Command(e) => Some(e),
            Self::Host(_) => None,
        }
    }
}

impl<E: Into<CommandError>> ExecError<E> {
    /// Everything as a [`CommandError`], as [`Registry::execute`] returns it (`command` fills in an
    /// empty id).
    pub fn into_command(self, command: &str) -> CommandError {
        match self {
            Self::Command(e) => e,
            Self::Host(e) => e.into().or_id(command),
        }
    }
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
    /// Typed, built in a `const`/`static` table (see [`Param`]).
    Static(&'static [Param]),
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

mod fuzzy;
mod journal;
pub mod keymap;
mod legacy;
mod macros;
pub mod menu;
mod params;
mod registry;
pub mod shortcut;

pub use fuzzy::{FuzzyMatch, fuzzy_match, fuzzy_rank};
pub use journal::{CommandRun, Journal, JournalEntry, RunObserver};
pub use keymap::Keymap;
pub use legacy::accepted_params;
pub use params::{Kind, Param, params_schema, validate_params};
pub use registry::{Bound, MAX_DEPTH, Registry, Validation};

impl CommandError {
    /// The command the error is about.
    pub fn id(&self) -> &str {
        match self {
            Self::Unknown { id } | Self::Disabled { id, .. } | Self::BadParams { id, .. } | Self::Failed { id, .. } | Self::Panicked { id } => id,
        }
    }
    pub fn failed(id: impl Into<String>, msg: impl Into<String>) -> Self {
        Self::Failed { id: id.into(), msg: msg.into() }
    }
    pub fn bad_params(id: impl Into<String>, msg: impl Into<String>) -> Self {
        Self::BadParams { id: id.into(), msg: msg.into() }
    }
    /// Fill in the command id when the error was built without one (a host error converted with
    /// `From`, which does not know which command it came from).
    pub fn or_id(mut self, command: &str) -> Self {
        let (Self::Unknown { id } | Self::Disabled { id, .. } | Self::BadParams { id, .. } | Self::Failed { id, .. } | Self::Panicked { id }) =
            &mut self;
        if id.is_empty() {
            command.clone_into(id);
        }
        self
    }
}

impl ParamSpec {
    /// An optional parameter; chain [`ParamSpec::required`] and [`ParamSpec::about`].
    pub fn new(name: impl Into<String>, kind: ParamKind) -> Self {
        Self { name: name.into(), kind, required: false, about: String::new() }
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn about(mut self, about: impl Into<String>) -> Self {
        self.about = about.into();
        self
    }
}

// ---------------------------------------------------------------------------------------------
// Commands of one host state
// ---------------------------------------------------------------------------------------------

/// Enabled predicate: `Err(reason)` disables with a human-readable reason.
pub type Enabled<S> = fn(&S) -> Result<(), String>;
/// Command body. Returns the host's own error type (see [`Host::Error`]), so command tables written
/// against an app's `Result` alias port unchanged.
pub type Run<S> = fn(&mut S, &Value) -> Result<Value, <S as Host>::Error>;

/// The enabled predicate of a command that is always available.
pub fn always<S>(_: &S) -> Result<(), String> {
    Ok(())
}

/// One command of host state `S`. Build it with [`cmd!`]/[`query!`] (the shape of existing
/// command tables) or with [`CommandSpec::new`] and its `const` setters.
pub struct CommandSpec<S: Host> {
    pub id: &'static str,
    pub label: &'static str,
    pub menu: &'static [&'static str],
    /// Portable shortcut(s): `Mod+Shift+S`, alternatives separated by ` / ` (see [`shortcut`]).
    pub shortcut: Option<&'static str>,
    pub params: Params,
    pub enabled: Enabled<S>,
    pub run: Run<S>,
    /// Record in the journal / history (false for pure queries and view commands).
    pub mutates: bool,
}

impl<S: Host> CommandSpec<S> {
    /// A mutating command without menu, shortcut or parameters that is always enabled.
    pub const fn new(id: &'static str, label: &'static str, run: Run<S>) -> Self {
        Self { id, label, menu: &[], shortcut: None, params: Params::None, enabled: always::<S>, run, mutates: true }
    }
    pub const fn menu(mut self, menu: &'static [&'static str]) -> Self {
        self.menu = menu;
        self
    }
    pub const fn shortcut(mut self, shortcut: &'static str) -> Self {
        self.shortcut = Some(shortcut);
        self
    }
    pub const fn enabled(mut self, enabled: Enabled<S>) -> Self {
        self.enabled = enabled;
        self
    }
    /// A pure query / view command: not journaled.
    pub const fn query(mut self) -> Self {
        self.mutates = false;
        self
    }
    pub fn params(mut self, params: Params) -> Self {
        self.params = params;
        self
    }
}

/// The host state commands run on, and the hooks the chokepoint calls around every command.
///
/// [`Registry::execute`] runs, in order: lookup → [`Host::before`] (may rewrite params) → parameter
/// validation (the registry's [`Validation`] policy, then [`Host::validate`]) → [`Host::enabled_for`] → the command under `catch_unwind` → [`Host::after`] →
/// journal (mutating commands that succeeded, params passed through [`Host::scrub`]). A call that
/// is turned away before it runs goes to [`Host::rejected`] instead of `after`.
///
/// `depth` is 0 for a call from outside (UI, CLI, agent) and grows by one for each command run
/// from inside another command; hooks use it to log or sync only once per top-level call.
///
/// The chokepoint keeps `depth` itself, including for commands a body runs through
/// `Registry::execute`. A host touches it ([`Journal::set_depth`]) only to run a command from a
/// hook, where the chokepoint has already put the depth back to the caller's: to make that command
/// count as nested in the current call (not journaled as top-level, not logged twice), save
/// [`Journal::depth`], set it one higher, execute, and set the saved value back.
///
/// Mutable execution state (nesting depth, the journal) lives in a [`Journal`] the host owns, so
/// one `Registry` (often a `static`) serves any number of documents/sessions.
pub trait Host: Sized {
    /// The error type command bodies return. `CommandError` itself, or the app's own error with
    /// `From<AppError> for CommandError` (leave the `id` empty: the chokepoint fills it in).
    type Error: Into<CommandError>;
    /// The execution state: nesting depth and journal.
    fn journal(&mut self) -> &mut Journal;
    /// Rewrite params before validation (resolve names to ids, settle an ongoing gesture…). An
    /// `Err` refuses the call: [`Host::rejected`] sees it converted, and
    /// [`Registry::execute_host`] hands it back unchanged as [`ExecError::Host`].
    fn before(&mut self, _spec: &CommandSpec<Self>, _params: &mut Value, _depth: usize) -> Result<(), Self::Error> {
        Ok(())
    }
    /// Extra parameter checks after the registry's [`Validation`] policy and before
    /// [`Host::enabled_for`] (an `Err` is refused and goes to [`Host::rejected`]). With
    /// [`Validation::Off`] this is where a host validates when it wants to, e.g. only at depth 0
    /// or only for agent calls. Default: nothing.
    fn validate(&mut self, _spec: &CommandSpec<Self>, _params: &Value, _depth: usize) -> Result<(), CommandError> {
        Ok(())
    }
    /// Enabled check for one call; override when explicit targets in params change availability
    /// (the state may be adjusted temporarily, e.g. targets swapped in for the selection).
    fn enabled_for(&mut self, spec: &CommandSpec<Self>, _params: &Value) -> Result<(), String> {
        (spec.enabled)(self)
    }
    /// Called after every run (ok, error or panic), at every nesting depth.
    fn after(&mut self, _spec: &CommandSpec<Self>, _params: &Value, _result: &Result<Value, CommandError>, _depth: usize) {}
    /// Called when a call is refused before running: unknown command, bad params, disabled, too
    /// deeply nested, or an error from [`Host::before`].
    fn rejected(&mut self, _id: &str, _error: &CommandError, _depth: usize) {}
    /// Remove secrets from params before they reach the journal.
    fn scrub(&self, _id: &str, _params: &mut Value) {}
}
