//! The command table of one app and its execute chokepoint.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;

use serde_json::Value;

use crate::{CommandError, CommandInfo, CommandSpec, Dispatch, ExecError, Host, Params, params, shortcut};

/// Commands may run other commands up to this depth; deeper calls are refused (a command that
/// calls itself, directly or through others, cannot overflow the stack).
pub const MAX_DEPTH: usize = 64;

/// Which declarations [`Registry::execute`] checks params against before [`Host::enabled_for`]
/// (set with [`Registry::with_validation`]). Whatever the policy, [`Host::validate`] runs at the
/// same step, and [`Registry::validate`] stays available for checking on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Validation {
    /// Every declaration: typed params fully, legacy text docs loosely (keys the doc clearly does
    /// not mention, non-object params). The default.
    #[default]
    Strict,
    /// Only typed declarations ([`Params::Typed`], [`Params::Static`]); legacy text docs and
    /// [`Params::None`] accept anything. For a table whose text docs are descriptions, not contracts.
    TypedOnly,
    /// None at the chokepoint: a call goes straight to [`Host::enabled_for`], so a disabled
    /// command reports `Disabled` whatever its params. The host validates where it wants to
    /// ([`Host::validate`], or [`Registry::validate`] from its own code).
    Off,
}

/// The command table of one app. Immutable once built (a `static` is typical); the execution state
/// lives in the host's [`crate::Journal`].
pub struct Registry<S: Host> {
    specs: Vec<CommandSpec<S>>,
    index: HashMap<&'static str, usize>,
    common: &'static [&'static str],
    aliases: &'static [(&'static str, &'static str)],
    schemas: OnceLock<Vec<Value>>,
    validation: Validation,
}

impl<S: Host> Registry<S> {
    /// Index the table. When an id appears twice the first spec wins (see [`Registry::duplicates`]).
    pub fn new(specs: Vec<CommandSpec<S>>) -> Self {
        let mut index = HashMap::with_capacity(specs.len());
        for (i, c) in specs.iter().enumerate() {
            index.entry(c.id).or_insert(i);
        }
        Self { specs, index, common: &[], aliases: &[], schemas: OnceLock::new(), validation: Validation::Strict }
    }

    /// Keys every command accepts on top of its own (document targeting, gesture merging…).
    pub fn with_common_params(mut self, keys: &'static [&'static str]) -> Self {
        self.common = keys;
        self
    }

    /// Alternative spellings legacy text-documented commands accept: `(documented, alias)`.
    pub fn with_param_aliases(mut self, aliases: &'static [(&'static str, &'static str)]) -> Self {
        self.aliases = aliases;
        self
    }

    /// Where the chokepoint validates params (default [`Validation::Strict`]).
    pub fn with_validation(mut self, validation: Validation) -> Self {
        self.validation = validation;
        self
    }

    pub fn validation(&self) -> Validation {
        self.validation
    }

    pub fn find(&self, id: &str) -> Option<&CommandSpec<S>> {
        self.index.get(id).and_then(|&i| self.specs.get(i))
    }

    pub fn specs(&self) -> &[CommandSpec<S>] {
        &self.specs
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Ids declared more than once (a test should assert this is empty).
    pub fn duplicates(&self) -> Vec<&'static str> {
        self.specs.iter().enumerate().filter(|(i, c)| self.index.get(c.id) != Some(i)).map(|(_, c)| c.id).collect()
    }

    /// Check params against a command's declaration without running it.
    pub fn validate(&self, spec: &CommandSpec<S>, params: &Value) -> Result<(), CommandError> {
        params::validate_params(spec.id, &spec.params, params, self.common, self.aliases)
    }

    /// The JSON Schema of a command's parameters (computed once for the whole table, on first use).
    pub fn schema(&self, id: &str) -> Option<&Value> {
        let i = *self.index.get(id)?;
        self.schemas().get(i)
    }

    fn schemas(&self) -> &[Value] {
        self.schemas.get_or_init(|| self.specs.iter().map(|c| params::params_schema(&c.params)).collect())
    }

    /// Discovery info of one command against the current state (enabled by its own predicate).
    pub fn info(&self, spec: &CommandSpec<S>, state: &S) -> CommandInfo {
        let enabled = (spec.enabled)(state);
        let schema = self.schema(spec.id).cloned().unwrap_or(Value::Null);
        CommandInfo {
            id: spec.id.to_string(),
            label: spec.label.to_string(),
            menu: spec.menu.iter().map(|m| (*m).to_string()).collect(),
            shortcut: spec.shortcut.map(|s| shortcut::normalize(s).unwrap_or_else(|| s.to_string())),
            params_schema: schema,
            params_text: match spec.params {
                crate::Params::Text(t) => Some(t.to_string()),
                _ => None,
            },
            enabled: enabled.is_ok(),
            disabled_reason: enabled.err(),
            mutates: spec.mutates,
        }
    }

    /// Discovery info of every command.
    pub fn infos(&self, state: &S) -> Vec<CommandInfo> {
        self.specs.iter().map(|c| self.info(c, state)).collect()
    }

    /// The one chokepoint every caller goes through (UI, menus, palette, shortcuts, CLI, control
    /// port, MCP, and commands running other commands). See [`Host`] for the order of steps.
    /// Host errors come back converted into [`CommandError`]; [`Registry::execute_host`] keeps them.
    pub fn execute(&self, state: &mut S, id: &str, params: Value) -> Result<Value, CommandError> {
        let converted = |e: S::Error| {
            let e = e.into().or_id(id);
            (e.clone(), e)
        };
        self.chokepoint(state, id, params, converted).map_err(|e| match e {
            ExecError::Command(e) | ExecError::Host(e) => e,
        })
    }

    /// [`Registry::execute`] for a host whose callers match on its own error type: an error a
    /// command body returned comes back unchanged as [`ExecError::Host`]; a refusal or failure of
    /// the chokepoint itself (unknown, too deep, validation, disabled, panic) is
    /// [`ExecError::Command`]. An error from [`Host::before`] is the host's too: `Host`. Same steps and hooks; [`Host::after`] still sees the error converted
    /// into a `CommandError` (hence `Clone`: the original is kept for the caller).
    pub fn execute_host(&self, state: &mut S, id: &str, params: Value) -> Result<Value, ExecError<S::Error>>
    where
        S::Error: Clone,
    {
        self.chokepoint(state, id, params, |e: S::Error| (e.clone().into().or_id(id), e))
    }

    /// The steps of [`Host`]. `host_error` turns an error a body returned into what `after` sees
    /// and what the caller gets.
    fn chokepoint<E>(
        &self,
        state: &mut S,
        id: &str,
        params: Value,
        host_error: impl FnOnce(S::Error) -> (CommandError, E),
    ) -> Result<Value, ExecError<E>> {
        let depth = state.journal().depth();
        let Some(spec) = self.find(id) else {
            return reject(state, id, CommandError::Unknown { id: id.to_string() }, depth);
        };
        if depth >= MAX_DEPTH {
            let e = CommandError::failed(id, format!("commands are nested more than {MAX_DEPTH} deep (a command that runs itself?)"));
            return reject(state, id, e, depth);
        }
        let mut params = params;
        if let Err(e) = state.before(spec, &mut params, depth) {
            let (seen, host) = host_error(e);
            state.rejected(id, &seen, depth);
            return Err(ExecError::Host(host));
        }
        if let Err(e) = self.validate_by_policy(spec, &params) {
            return reject(state, id, e, depth);
        }
        if let Err(e) = state.validate(spec, &params, depth) {
            return reject(state, id, e.or_id(id), depth);
        }
        if let Err(reason) = state.enabled_for(spec, &params) {
            return reject(state, id, CommandError::Disabled { id: id.to_string(), reason }, depth);
        }
        state.journal().set_depth(depth + 1);
        // Last-resort guard: a panic in a command (a bug that escaped the never-crash rules)
        // becomes an error for the caller instead of taking the app and its unsaved work down.
        let run = catch_unwind(AssertUnwindSafe(|| (spec.run)(state, &params)));
        // set, not decrement: a nested call that went wrong cannot leave the depth off by one
        state.journal().set_depth(depth);
        // `after` sees a `CommandError`; the caller gets the host's own error when there is one
        let mut kept = None;
        let result = match run {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                let (seen, host) = host_error(e);
                kept = Some(host);
                Err(seen)
            }
            Err(_) => Err(CommandError::Panicked { id: id.to_string() }),
        };
        state.after(spec, &params, &result, depth);
        if result.is_ok() && spec.mutates && state.journal().records(depth) {
            // a secret must never reach the journal (and with it session logs)
            state.scrub(id, &mut params);
            state.journal().push(id, params);
        }
        result.map_err(|e| kept.map_or(ExecError::Command(e), ExecError::Host))
    }

    fn validate_by_policy(&self, spec: &CommandSpec<S>, params: &Value) -> Result<(), CommandError> {
        match (self.validation, &spec.params) {
            (Validation::Strict, _) | (Validation::TypedOnly, Params::Typed(_) | Params::Static(_)) => self.validate(spec, params),
            (Validation::TypedOnly | Validation::Off, _) => Ok(()),
        }
    }

    /// Bind the table to a state: a [`Dispatch`] for discovery and execution.
    pub fn bind<'a>(&'a self, state: &'a mut S) -> Bound<'a, S> {
        Bound { registry: self, state }
    }
}

fn reject<S: Host, E>(state: &mut S, id: &str, e: CommandError, depth: usize) -> Result<Value, ExecError<E>> {
    state.rejected(id, &e, depth);
    Err(ExecError::Command(e))
}

/// A registry bound to one state: what control port, MCP and CLI drive through [`Dispatch`].
pub struct Bound<'a, S: Host> {
    pub registry: &'a Registry<S>,
    pub state: &'a mut S,
}

impl<S: Host> Dispatch for Bound<'_, S> {
    fn commands(&self) -> Vec<CommandInfo> {
        self.registry.infos(self.state)
    }
    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        self.registry.execute(self.state, id, params)
    }
}
