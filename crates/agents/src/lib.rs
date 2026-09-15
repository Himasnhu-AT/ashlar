//! Connect a user's coding agents to an app: its MCP server goes into each agent's own config
//! file, and its skills (made from a category of the app's docs, see [`skills`]) into the skills
//! folder the agent reads: `~/.claude/skills` for Claude Code, the shared `~/.agents/skills` for
//! the others (Codex, Cursor, Copilot in VS Code, Gemini CLI and Devin Desktop all read it).
//!
//! - [`detect`] says which agents are on this machine.
//! - [`install`] and [`uninstall`] return a [`Plan`]: every file they would create, change or
//!   remove, with the text, and nothing written yet (`--print`). [`Plan::apply`] writes it,
//!   copying each existing file it changes to a backup first.
//! - Re-running is idempotent: an entry or block already as wanted is [`Action::Unchanged`].
//!   Edits merge into the user's files without touching their other entries (JSON with comments
//!   is edited in place, see [`jsonc`]; TOML through `toml_edit`), and uninstalling removes
//!   exactly what an install adds: the server entry named after the app and the skills whose
//!   `SKILL.md` carries the install marker. A shared skills folder keeps our skills while another
//!   agent that reads it still has the server.
//!
//! Everything reads the machine through a [`Host`], so tests and dry runs can point it at a
//! scratch folder.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod host;
pub mod jsonc;
pub mod skills;

use std::fmt;
use std::path::{Path, PathBuf};

pub use host::{Host, Os, VARS, cli_path};
use serde::Serialize;
use serde_json::{Value, json};
pub use skills::Skill;

/// A coding agent this crate can set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Cursor,
    Windsurf,
    #[serde(rename = "vscode")]
    VsCode,
    Gemini,
}

impl Agent {
    pub const ALL: [Self; 6] = [Self::Claude, Self::Codex, Self::Cursor, Self::Windsurf, Self::VsCode, Self::Gemini];

    /// The id `--for` takes: `claude`, `codex`, `cursor`, `windsurf`, `vscode`, `gemini`.
    pub fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Windsurf => "windsurf",
            Self::VsCode => "vscode",
            Self::Gemini => "gemini",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf / Devin Desktop",
            Self::VsCode => "VS Code (GitHub Copilot)",
            Self::Gemini => "Gemini CLI",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|a| a.id() == s).or(match s.as_str() {
            "claude-code" | "claudecode" => Some(Self::Claude),
            "code" | "copilot" | "vs-code" => Some(Self::VsCode),
            "gemini-cli" => Some(Self::Gemini),
            "devin" | "devin-desktop" | "cascade" => Some(Self::Windsurf),
            _ => None,
        })
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.title())
    }
}

/// Where an install goes: the user's own config (every project), or one project folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    User,
    Project(PathBuf),
}

/// The MCP server an agent starts: a program on stdio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Server {
    pub command: String,
    pub args: Vec<String>,
}

/// What an app installs into agents.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The app's command-line name: the MCP server's name in every config, the skills' prefix and
    /// the marker of everything installed (`<id> agent install`).
    pub id: String,
    /// The app's display name.
    pub title: String,
    pub server: Server,
    /// Skills, the entry skill first ([`skills::from_category`] makes the rest).
    pub skills: Vec<Skill>,
}

impl Setup {
    /// The line that marks a `SKILL.md` as ours.
    pub fn marker(&self) -> String {
        marker(&self.id)
    }
}

/// The marker line of `id`'s files.
pub fn marker(id: &str) -> String {
    format!("<!-- Installed by `{id} agent install`; re-run it to update, `{id} agent uninstall` removes it. -->")
}

/// What a change does to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Create,
    Update,
    Remove,
    Unchanged,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Remove => "remove",
            Self::Unchanged => "unchanged",
        }
    }
}

/// One file of a plan.
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub path: PathBuf,
    pub action: Action,
    /// What it is, for people: "MCP server `app`", "skill `app-review`".
    pub what: String,
    /// The part that is ours: the server entry, the block, or the whole file.
    pub preview: String,
    /// The whole new file (`None` removes it).
    #[serde(skip)]
    new: Option<String>,
    /// The file is the user's (a config file): back it up before changing it.
    #[serde(skip)]
    shared: bool,
}

/// What an install or uninstall would do for one agent; nothing is written until [`Plan::apply`].
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub agent: Agent,
    pub changes: Vec<Change>,
    /// Things the user should know (an entry with our name that points elsewhere, a scope the
    /// agent has no file for).
    pub notes: Vec<String>,
}

/// A file [`Plan::apply`] wrote or removed.
#[derive(Debug, Clone, Serialize)]
pub struct Applied {
    pub path: PathBuf,
    pub action: Action,
    pub what: String,
    /// Where the previous version was copied.
    pub backup: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{path}: {what}; nothing was changed")]
    Unreadable { path: PathBuf, what: String },
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io { path: path.to_path_buf(), source }
}

impl Plan {
    /// True when everything is already as wanted.
    pub fn is_noop(&self) -> bool {
        self.changes.iter().all(|c| c.action == Action::Unchanged)
    }

    /// Write the plan. Each user file that changes is copied first to
    /// `<file>.<backup_tag>-backup-<unix seconds>`; our own files (the skills) are
    /// just replaced. Folders are created as needed; a skill folder left empty is removed.
    pub fn apply(&self, backup_tag: &str) -> Result<Vec<Applied>, Error> {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut done = Vec::new();
        for c in &self.changes {
            if c.action == Action::Unchanged {
                continue;
            }
            let mut backup = None;
            if c.shared && c.path.is_file() {
                let name = c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let b = c.path.with_file_name(format!("{name}.{backup_tag}-backup-{stamp}"));
                std::fs::copy(&c.path, &b).map_err(io(&b))?;
                backup = Some(b);
            }
            match &c.new {
                Some(text) => {
                    if let Some(dir) = c.path.parent() {
                        std::fs::create_dir_all(dir).map_err(io(dir))?;
                    }
                    ashlar_fs::atomic_write(&c.path, text.as_bytes()).map_err(io(&c.path))?;
                }
                None => {
                    match std::fs::remove_file(&c.path) {
                        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(&c.path)(e)),
                        _ => {}
                    }
                    // a skill's folder goes with its SKILL.md when nothing else is in it
                    if c.path.file_name().is_some_and(|n| n == SKILL_FILE)
                        && let Some(dir) = c.path.parent()
                    {
                        let _ = std::fs::remove_dir(dir);
                    }
                }
            }
            done.push(Applied { path: c.path.clone(), action: c.action, what: c.what.clone(), backup });
        }
        Ok(done)
    }

    /// The plan as text: one line per file, then the part that is ours for each change.
    pub fn describe(&self, with_preview: bool) -> String {
        let mut out = format!("{}:\n", self.agent.title());
        for c in &self.changes {
            out.push_str(&format!("  {:<9} {}  ({})\n", c.action.as_str(), c.path.display(), c.what));
            if with_preview && matches!(c.action, Action::Create | Action::Update) {
                for line in c.preview.lines() {
                    out.push_str(&format!("      | {line}\n"));
                }
            }
        }
        for n in &self.notes {
            out.push_str(&format!("  note: {n}\n"));
        }
        out
    }
}

const SKILL_FILE: &str = "SKILL.md";

/// Whether an agent seems to be installed, and why.
#[derive(Debug, Clone, Serialize)]
pub struct Detection {
    pub agent: Agent,
    pub found: bool,
    /// What was found (a config folder, a program, an application).
    pub evidence: Vec<String>,
}

/// Which agents are on this machine: their config folder, their program on the search path, or
/// their application.
pub fn detect(host: &Host) -> Vec<Detection> {
    Agent::ALL
        .into_iter()
        .map(|agent| {
            let mut evidence = Vec::new();
            let (dirs, programs, apps): (Vec<PathBuf>, &[&str], &[&str]) = match agent {
                Agent::Claude => (vec![claude_dir(host), claude_json(host)], &["claude"], &["Claude"]),
                Agent::Codex => (vec![codex_dir(host)], &["codex"], &["Codex"]),
                Agent::Cursor => (vec![host.home_join(".cursor")], &["cursor", "cursor-agent"], &["Cursor"]),
                Agent::Windsurf => (vec![windsurf_dir(host), devin_dir(host)], &["devin", "windsurf"], &["Devin", "Windsurf"]),
                Agent::VsCode => (vec![vscode_user(host), host.home_join(".copilot")], &["code"], &["Visual Studio Code"]),
                Agent::Gemini => (vec![gemini_dir(host)], &["gemini"], &[]),
            };
            for d in dirs {
                if d.exists() {
                    evidence.push(d.display().to_string());
                }
            }
            for p in programs {
                if let Some(found) = host.which(p) {
                    evidence.push(found.display().to_string());
                }
            }
            for a in apps {
                if let Some(found) = host.app(a) {
                    evidence.push(found.display().to_string());
                }
            }
            // the Claude desktop app alone is not Claude Code
            let found = match agent {
                Agent::Claude => evidence.iter().any(|e| !e.ends_with("Claude.app")),
                _ => !evidence.is_empty(),
            };
            Detection { agent, found, evidence }
        })
        .collect()
}

// ---- where each agent keeps things ----

fn claude_dir(host: &Host) -> PathBuf {
    host.var("CLAUDE_CONFIG_DIR").map_or_else(|| host.home_join(".claude"), PathBuf::from)
}

fn claude_json(host: &Host) -> PathBuf {
    match host.var("CLAUDE_CONFIG_DIR") {
        Some(d) => Path::new(d).join(".claude.json"),
        None => host.home_join(".claude.json"),
    }
}

fn codex_dir(host: &Host) -> PathBuf {
    host.var("CODEX_HOME").map_or_else(|| host.home_join(".codex"), PathBuf::from)
}

fn gemini_dir(host: &Host) -> PathBuf {
    host.var("GEMINI_CLI_HOME").map_or_else(|| host.home.clone(), PathBuf::from).join(".gemini")
}

fn vscode_user(host: &Host) -> PathBuf {
    host.config_dir().join("Code/User")
}

/// Windsurf's folder (Cascade, the agent of Windsurf before it became Devin Desktop).
fn windsurf_dir(host: &Host) -> PathBuf {
    host.home_join(".codeium/windsurf")
}

/// Devin's config folder (Devin Desktop and the Devin CLI): `~/.config/devin` or
/// `$XDG_CONFIG_HOME/devin`; `%APPDATA%\devin` on Windows.
fn devin_dir(host: &Host) -> PathBuf {
    match host.os {
        Os::Windows => host.config_dir().join("devin"),
        _ => host.var("XDG_CONFIG_HOME").map_or_else(|| host.home_join(".config"), PathBuf::from).join("devin"),
    }
}

/// How an agent's MCP config file is written.
#[derive(Debug, Clone, Copy)]
enum ConfigFormat {
    /// JSON (comments allowed) with servers under this key.
    Json { key: &'static str, with_type: bool },
    /// TOML with `[mcp_servers.<name>]` tables.
    Toml,
}

/// The MCP config files of `agent` for `scope` (Windsurf / Devin Desktop has two: Devin's, and
/// Windsurf's own while its folder is there).
fn mcp_files(agent: Agent, scope: &Scope, host: &Host) -> Vec<(PathBuf, ConfigFormat)> {
    let json = |key, with_type| ConfigFormat::Json { key, with_type };
    match (agent, scope) {
        (Agent::Claude, Scope::User) => vec![(claude_json(host), json("mcpServers", true))],
        (Agent::Claude, Scope::Project(d)) => vec![(d.join(".mcp.json"), json("mcpServers", true))],
        (Agent::Codex, Scope::User) => vec![(codex_dir(host).join("config.toml"), ConfigFormat::Toml)],
        (Agent::Codex, Scope::Project(d)) => vec![(d.join(".codex/config.toml"), ConfigFormat::Toml)],
        (Agent::Cursor, Scope::User) => vec![(host.home_join(".cursor/mcp.json"), json("mcpServers", true))],
        (Agent::Cursor, Scope::Project(d)) => vec![(d.join(".cursor/mcp.json"), json("mcpServers", true))],
        (Agent::Windsurf, Scope::User) => {
            let mut v = vec![(devin_dir(host).join("mcp_config.json"), json("mcpServers", false))];
            if windsurf_dir(host).is_dir() {
                v.push((windsurf_dir(host).join("mcp_config.json"), json("mcpServers", false)));
            }
            v
        }
        (Agent::Windsurf, Scope::Project(d)) => vec![(d.join(".devin/mcp_config.json"), json("mcpServers", false))],
        (Agent::VsCode, Scope::User) => vec![(vscode_user(host).join("mcp.json"), json("servers", true))],
        (Agent::VsCode, Scope::Project(d)) => vec![(d.join(".vscode/mcp.json"), json("servers", true))],
        (Agent::Gemini, Scope::User) => vec![(gemini_dir(host).join("settings.json"), json("mcpServers", false))],
        (Agent::Gemini, Scope::Project(d)) => vec![(d.join(".gemini/settings.json"), json("mcpServers", false))],
    }
}

/// The shared skills folder most agents read.
fn shared_skills(scope: &Scope, host: &Host) -> PathBuf {
    match scope {
        Scope::User => host.home_join(".agents/skills"),
        Scope::Project(d) => d.join(".agents/skills"),
    }
}

/// The skills folder of `agent` for `scope`.
fn skills_dir(agent: Agent, scope: &Scope, host: &Host) -> PathBuf {
    match (agent, scope) {
        (Agent::Claude, Scope::User) => claude_dir(host).join("skills"),
        (Agent::Claude, Scope::Project(d)) => d.join(".claude/skills"),
        _ => shared_skills(scope, host),
    }
}

/// The server entry as `agent` writes it (`type` only for the agents that ask for it).
fn entry(server: &Server, with_type: bool) -> Value {
    let mut v = json!({"command": server.command, "args": server.args});
    if with_type && let Some(m) = v.as_object_mut() {
        m.insert("type".into(), json!("stdio"));
    }
    v
}

fn read(path: &Path) -> Result<Option<String>, Error> {
    match std::fs::read(path) {
        Ok(b) => String::from_utf8(b).map(Some).map_err(|_| Error::Unreadable { path: path.to_path_buf(), what: "not UTF-8 text".into() }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(path)(e)),
    }
}

fn change(path: PathBuf, old: Option<&str>, new: Option<String>, what: String, preview: String, shared: bool) -> Change {
    let action = match (old, &new) {
        (None, None) => Action::Unchanged,
        (None, Some(_)) => Action::Create,
        (Some(_), None) => Action::Remove,
        (Some(o), Some(n)) if o == n => Action::Unchanged,
        (Some(_), Some(_)) => Action::Update,
    };
    Change { path, action, what, preview, new, shared }
}

/// The MCP server entry: set (`Some`) or removed (`None`).
fn server_change(path: PathBuf, format: ConfigFormat, id: &str, server: Option<&Server>, notes: &mut Vec<String>) -> Result<Change, Error> {
    let old = read(&path)?;
    let text = old.as_deref().unwrap_or("");
    let bad = |what: String| Error::Unreadable { path: path.clone(), what };
    let what = format!("MCP server `{id}`");
    let (new, preview) = match format {
        ConfigFormat::Json { key, with_type } => match server {
            Some(s) => {
                let e = entry(s, with_type);
                let new = jsonc::set(text, &[key, id], &e).map_err(|e| bad(e.to_string()))?;
                (Some(new), format!("\"{key}\": {{ \"{id}\": {} }}", serde_json::to_string(&e).unwrap_or_default()))
            }
            None => {
                let existing = jsonc::get(text, &[key, id]).map_err(|e| bad(e.to_string()))?;
                if let Some(cmd) = existing.as_ref().and_then(|v| v.get("command")).and_then(Value::as_str)
                    && !is_ours(cmd, id)
                {
                    notes.push(format!("left the `{id}` entry in {}: it runs `{cmd}`, not this app's tool", path.display()));
                    return Ok(change(path.clone(), old.as_deref(), old.clone(), what, String::new(), true));
                }
                let new = jsonc::remove(text, &[key, id]).map_err(|e| bad(e.to_string()))?;
                (new.or_else(|| old.clone()), String::new())
            }
        },
        ConfigFormat::Toml => {
            let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| bad(format!("not valid TOML ({})", e.message())))?;
            let preview;
            match server {
                Some(s) => {
                    let servers = doc.entry("mcp_servers").or_insert_with(|| {
                        let mut t = toml_edit::Table::new();
                        t.set_implicit(true);
                        toml_edit::Item::Table(t)
                    });
                    let Some(servers) = servers.as_table_mut() else { return Err(bad("`mcp_servers` is not a table".into())) };
                    let mut args = toml_edit::Array::new();
                    for a in &s.args {
                        args.push(a.as_str());
                    }
                    // update in place so comments and other keys in our table survive
                    let t = servers.entry(id).or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
                    let Some(t) = t.as_table_mut() else { return Err(bad(format!("`mcp_servers.{id}` is not a table"))) };
                    if t.get("command").and_then(|v| v.as_str()) != Some(s.command.as_str()) {
                        t["command"] = toml_edit::value(s.command.as_str());
                    }
                    let same_args = t
                        .get("args")
                        .and_then(|v| v.as_array())
                        .is_some_and(|a| a.iter().map(|x| x.as_str()).eq(s.args.iter().map(|x| Some(x.as_str()))));
                    if !same_args {
                        t["args"] = toml_edit::value(args);
                    }
                    preview = format!("[mcp_servers.{id}]\n{}", t);
                }
                None => {
                    preview = String::new();
                    let cmd =
                        doc.get("mcp_servers").and_then(|s| s.get(id)).and_then(|t| t.get("command")).and_then(|c| c.as_str()).map(str::to_string);
                    if let Some(cmd) = cmd.filter(|c| !is_ours(c, id)) {
                        notes.push(format!("left `[mcp_servers.{id}]` in {}: it runs `{cmd}`, not this app's tool", path.display()));
                    } else if let Some(servers) = doc.get_mut("mcp_servers").and_then(|s| s.as_table_mut()) {
                        servers.remove(id);
                    }
                }
            }
            let rendered = doc.to_string();
            let new = if old.is_none() && server.is_none() { None } else { Some(rendered) };
            (new, preview)
        }
    };
    Ok(change(path, old.as_deref(), new, what, preview, true))
}

/// A server entry is ours when it runs a program named after the app.
fn is_ours(command: &str, id: &str) -> bool {
    Path::new(command).file_stem().is_some_and(|s| s == id)
}

/// The skills: ours are created or updated, ours that the app no longer ships are removed.
fn skill_changes(dir: &Path, setup: Option<&Setup>, id: &str) -> Result<Vec<Change>, Error> {
    let mark = marker(id);
    let mut out = Vec::new();
    let mut wanted: Vec<String> = Vec::new();
    if let Some(s) = setup {
        for skill in &s.skills {
            let path = dir.join(&skill.name).join(SKILL_FILE);
            let old = read(&path)?;
            if old.as_deref().is_some_and(|o| !o.contains(&mark)) {
                // a skill of the same name that someone else wrote: leave it
                continue;
            }
            let text = skill.to_file(&mark);
            wanted.push(skill.name.clone());
            out.push(change(path, old.as_deref(), Some(text.clone()), format!("skill `{}`", skill.name), text, false));
        }
    }
    // ours that are no longer wanted
    let mut entries: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let prefixed = name == id || name.starts_with(&format!("{id}-"));
        if !prefixed || wanted.contains(&name) {
            continue;
        }
        let path = e.path().join(SKILL_FILE);
        if let Some(old) = read(&path)?.filter(|t| t.contains(&mark)) {
            out.push(change(path, Some(&old), None, format!("skill `{name}`"), String::new(), false));
        }
    }
    Ok(out)
}

/// What installing `setup` into `agent` would change.
pub fn install(agent: Agent, scope: &Scope, setup: &Setup, host: &Host) -> Result<Plan, Error> {
    plan(agent, scope, Some(setup), &setup.id, host)
}

/// What removing everything an install of `id` added to `agent` would change.
pub fn uninstall(agent: Agent, scope: &Scope, id: &str, host: &Host) -> Result<Plan, Error> {
    plan(agent, scope, None, id, host)
}

fn plan(agent: Agent, scope: &Scope, setup: Option<&Setup>, id: &str, host: &Host) -> Result<Plan, Error> {
    let mut notes = Vec::new();
    let mut changes = Vec::new();
    for (path, format) in mcp_files(agent, scope, host) {
        changes.push(server_change(path, format, id, setup.map(|s| &s.server), &mut notes)?);
    }
    let dir = skills_dir(agent, scope, host);
    // the shared folder keeps our skills while another agent that reads it still has our server
    let keep = setup.is_none() && dir == shared_skills(scope, host) && {
        let others: Vec<Agent> = Agent::ALL.into_iter().filter(|a| *a != agent && *a != Agent::Claude && has_server(*a, scope, id, host)).collect();
        if !others.is_empty() {
            let names: Vec<&str> = others.iter().map(|a| a.title()).collect();
            notes.push(format!("kept the skills in {}: {} still use them", dir.display(), names.join(", ")));
        }
        !others.is_empty()
    };
    if !keep {
        changes.extend(skill_changes(&dir, setup, id)?);
    }
    // drop what was never there (an uninstall of something not installed)
    changes.retain(|c| !(c.action == Action::Unchanged && c.new.is_none()));
    Ok(Plan { agent, changes, notes })
}

/// Whether `agent`'s config for `scope` has a server named `id` (an unreadable file counts as no).
fn has_server(agent: Agent, scope: &Scope, id: &str, host: &Host) -> bool {
    mcp_files(agent, scope, host).into_iter().any(|(path, format)| {
        let Ok(Some(text)) = read(&path) else { return false };
        match format {
            ConfigFormat::Json { key, .. } => jsonc::get(&text, &[key, id]).ok().flatten().is_some(),
            ConfigFormat::Toml => text.parse::<toml_edit::DocumentMut>().ok().is_some_and(|d| d.get("mcp_servers").and_then(|s| s.get(id)).is_some()),
        }
    })
}

/// How far `setup` is installed in `agent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Nothing of ours is there.
    Missing,
    /// Some of it is there, but not as this version would write it.
    Outdated,
    /// Everything is as this version would write it.
    Current,
}

/// Whether `setup` is installed in `agent` (by comparing with what an install would write).
pub fn state(agent: Agent, scope: &Scope, setup: &Setup, host: &Host) -> Result<State, Error> {
    let p = install(agent, scope, setup, host)?;
    if p.is_noop() {
        return Ok(State::Current);
    }
    let any_ours = !uninstall(agent, scope, &setup.id, host)?.is_noop();
    Ok(if any_ours { State::Outdated } else { State::Missing })
}

/// Install or uninstall.
#[derive(Debug, Clone, Copy)]
pub enum Op<'a> {
    Install(&'a Setup),
    /// Remove what an install of this id added.
    Uninstall(&'a str),
}

/// What happened for one agent in [`run`].
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub agent: Agent,
    /// What would change (with `write`, what was meant to).
    pub plan: Option<Plan>,
    /// The files written, when `write` was set and it worked.
    pub applied: Vec<Applied>,
    pub error: Option<String>,
}

/// The agents `spec` names: `None` for the ones [`detect`] finds, `all` for every one, else a
/// comma-separated list of [`Agent::id`]s (`claude,codex`).
pub fn select(spec: Option<&str>, host: &Host) -> Result<Vec<Agent>, String> {
    match spec.map(str::trim) {
        None | Some("") => {
            let found: Vec<Agent> = detect(host).into_iter().filter(|d| d.found).map(|d| d.agent).collect();
            if found.is_empty() {
                return Err(format!("no coding agent found on this machine; name one with --for ({})", ids()));
            }
            Ok(found)
        }
        Some("all") => Ok(Agent::ALL.to_vec()),
        Some(list) => {
            let mut out = Vec::new();
            for part in list.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let a = Agent::parse(part).ok_or_else(|| format!("unknown agent `{part}` (known: {}, all)", ids()))?;
                if !out.contains(&a) {
                    out.push(a);
                }
            }
            Ok(out)
        }
    }
}

fn ids() -> String {
    Agent::ALL.iter().map(|a| a.id()).collect::<Vec<_>>().join(", ")
}

/// Plan `op` for each of `agents` and, with `write`, apply it (backups tagged `backup_tag`). One
/// agent's failure (an unreadable config) does not stop the others.
pub fn run(op: Op<'_>, agents: &[Agent], scope: &Scope, host: &Host, write: bool, backup_tag: &str) -> Vec<Outcome> {
    agents
        .iter()
        .map(|&agent| {
            let planned = match op {
                Op::Install(s) => install(agent, scope, s, host),
                Op::Uninstall(id) => uninstall(agent, scope, id, host),
            };
            match planned {
                Err(e) => Outcome { agent, plan: None, applied: Vec::new(), error: Some(e.to_string()) },
                Ok(plan) if !write => Outcome { agent, plan: Some(plan), applied: Vec::new(), error: None },
                Ok(plan) => match plan.apply(backup_tag) {
                    Ok(applied) => Outcome { agent, plan: Some(plan), applied, error: None },
                    Err(e) => Outcome { agent, plan: Some(plan), applied: Vec::new(), error: Some(e.to_string()) },
                },
            }
        })
        .collect()
}

/// [`run`]'s outcomes as text for a terminal: per agent, each file and what was (or, without
/// `write`, would be) done, with our part of each file when `preview` is set.
pub fn report(outcomes: &[Outcome], write: bool, preview: bool) -> String {
    let mut out = String::new();
    for o in outcomes {
        match (&o.plan, &o.error) {
            (_, Some(e)) if o.applied.is_empty() => out.push_str(&format!("{}:\n  failed: {e}\n", o.agent.title())),
            (Some(p), _) if p.is_noop() => {
                out.push_str(&format!("{}: up to date\n", o.agent.title()));
                for n in &p.notes {
                    out.push_str(&format!("  note: {n}\n"));
                }
            }
            (Some(p), _) => {
                out.push_str(&p.describe(preview || !write));
                for a in o.applied.iter().filter(|a| a.backup.is_some()) {
                    if let Some(b) = &a.backup {
                        out.push_str(&format!("  backup    {}\n", b.display()));
                    }
                }
            }
            (None, _) => {}
        }
    }
    if !write {
        out.push_str("(nothing written: --print)\n");
    }
    out
}

#[cfg(test)]
mod tests;
