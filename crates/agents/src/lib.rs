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
    /// The open app's MCP endpoint (`<id>-window`, streamable HTTP), for agents that speak it.
    pub window: Window,
}

/// What an install does with the `<id>-window` entry (the open app's HTTP endpoint).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Window {
    /// Leave it as it is (a tool that doesn't know the endpoint, such as the command line).
    Keep,
    /// Write it: the app serves MCP at `url` and wants `token` as the bearer token.
    Set { url: String, token: String },
    /// Take it out (the app doesn't serve one).
    Remove,
}

/// The name of the open app's entry: `<id>-window`.
pub fn window_name(id: &str) -> String {
    format!("{id}-window")
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
    /// Make the change by running these (the agent's own `mcp add` command) instead of writing
    /// the file; the file is written when they fail.
    #[serde(skip)]
    via: Vec<(PathBuf, Vec<String>)>,
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
                // never over an earlier backup (two runs in the same second)
                let mut b = c.path.with_file_name(format!("{name}.{backup_tag}-backup-{stamp}"));
                let mut n = 1u32;
                while b.exists() && n < 1000 {
                    n += 1;
                    b = c.path.with_file_name(format!("{name}.{backup_tag}-backup-{stamp}-{n}"));
                }
                std::fs::copy(&c.path, &b).map_err(io(&b))?;
                backup = Some(b);
            }
            let mut what = c.what.clone();
            if !c.via.is_empty() {
                match run_all(&c.via) {
                    Ok(()) => {
                        done.push(Applied { path: c.path.clone(), action: c.action, what, backup });
                        continue;
                    }
                    Err(e) => what = format!("{what}; written directly, the agent's command failed: {e}"),
                }
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
                    // a skill's folder goes with its SKILL.md when nothing else is in it, and the
                    // skills folder when that was the last skill
                    if c.path.file_name().is_some_and(|n| n == SKILL_FILE)
                        && let Some(dir) = c.path.parent()
                        && std::fs::remove_dir(dir).is_ok()
                        && let Some(skills) = dir.parent()
                    {
                        let _ = std::fs::remove_dir(skills);
                    }
                }
            }
            done.push(Applied { path: c.path.clone(), action: c.action, what, backup });
        }
        Ok(done)
    }

    /// The plan as text: one line per file that changes (and how many are already as wanted),
    /// with the part that is ours for each change when `with_preview` is set (long files are cut
    /// after their first lines).
    pub fn describe(&self, with_preview: bool) -> String {
        const PREVIEW_LINES: usize = 12;
        let mut out = format!("{}:\n", self.agent.title());
        // without previews, skills are one line per folder and action
        let mut grouped: Vec<(Action, PathBuf, Vec<String>)> = Vec::new();
        for c in self.changes.iter().filter(|c| c.action != Action::Unchanged) {
            if !with_preview
                && let Some(name) = c.what.strip_prefix("skill `").and_then(|n| n.strip_suffix('`'))
                && let Some(dir) = c.path.parent().and_then(Path::parent)
            {
                match grouped.iter_mut().find(|(a, d, _)| *a == c.action && d == dir) {
                    Some((_, _, names)) => names.push(name.to_string()),
                    None => grouped.push((c.action, dir.to_path_buf(), vec![name.to_string()])),
                }
                continue;
            }
            out.push_str(&format!("  {:<9} {}  ({})\n", c.action.as_str(), c.path.display(), c.what));
            if with_preview && matches!(c.action, Action::Create | Action::Update) {
                let lines: Vec<&str> = c.preview.lines().collect();
                for line in lines.iter().take(PREVIEW_LINES) {
                    out.push_str(&format!("      | {line}\n"));
                }
                if lines.len() > PREVIEW_LINES {
                    out.push_str(&format!("      | … {} more lines\n", lines.len() - PREVIEW_LINES));
                }
            }
        }
        for (action, dir, names) in grouped {
            let list = if names.len() > 4 {
                format!("{}, … {} more", names.get(..3).unwrap_or_default().join(", "), names.len() - 3)
            } else {
                names.join(", ")
            };
            let n = names.len();
            out.push_str(&format!("  {:<9} {}/  ({n} skill{}: {list})\n", action.as_str(), dir.display(), if n == 1 { "" } else { "s" }));
        }
        let same = self.changes.iter().filter(|c| c.action == Action::Unchanged).count();
        if same > 0 {
            out.push_str(&format!("  ({same} file{} already up to date)\n", if same == 1 { "" } else { "s" }));
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
/// Run each program with its arguments; the first failure, with what it printed.
fn run_all(cmds: &[(PathBuf, Vec<String>)]) -> Result<(), String> {
    for (program, args) in cmds {
        let out = std::process::Command::new(program).args(args).stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
        // removing an entry that isn't there is fine
        let removing = args.get(1).is_some_and(|a| a == "remove");
        if !out.status.success() && !removing {
            let text = String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect::<String>();
            return Err(if text.is_empty() { format!("exit status {}", out.status) } else { text });
        }
    }
    Ok(())
}

/// The window entry as `agent` writes it, as JSON; `None` for an agent without HTTP servers.
fn http_entry(agent: Agent, url: &str, token: &str) -> Option<Value> {
    let headers = json!({"Authorization": format!("Bearer {token}")});
    match agent {
        Agent::Claude | Agent::VsCode => Some(json!({"type": "http", "url": url, "headers": headers})),
        Agent::Cursor | Agent::Codex => Some(json!({"url": url, "headers": headers})),
        Agent::Gemini => Some(json!({"httpUrl": url, "headers": headers})),
        Agent::Windsurf => None,
    }
}

/// An entry is ours when it runs a program named after the app, or points at the app's loopback
/// endpoint.
fn entry_is_ours(v: &Value, id: &str) -> bool {
    if let Some(cmd) = v.get("command").and_then(Value::as_str) {
        return is_ours(cmd, id);
    }
    let url = v.get("url").or_else(|| v.get("httpUrl")).and_then(Value::as_str).unwrap_or("");
    url.starts_with("http://127.0.0.1:") || url.starts_with("http://localhost:") || url.is_empty()
}

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
    Change { path, action, what, preview, new, shared, via: Vec::new() }
}

/// One server entry named `name`: set to `want` (the agent's JSON shape; TOML is made from it)
/// or removed (`None`).
fn server_change(path: PathBuf, format: ConfigFormat, id: &str, name: &str, want: Option<&Value>, notes: &mut Vec<String>) -> Result<Change, Error> {
    let old = read(&path)?;
    server_change_on(path, old, format, id, name, want, notes)
}

/// [`server_change`] on `old`, the file as an earlier change leaves it.
fn server_change_on(
    path: PathBuf,
    old: Option<String>,
    format: ConfigFormat,
    id: &str,
    name: &str,
    want: Option<&Value>,
    notes: &mut Vec<String>,
) -> Result<Change, Error> {
    let text = old.as_deref().unwrap_or("");
    let bad = |what: String| Error::Unreadable { path: path.clone(), what };
    let what = format!("MCP server `{name}`");
    let not_ours = |v: &Value, notes: &mut Vec<String>| {
        let shown = v.get("command").or_else(|| v.get("url")).and_then(Value::as_str).unwrap_or("something else").to_string();
        notes.push(format!("left the `{name}` entry in {}: it points at `{shown}`, not this app", path.display()));
    };
    let (new, preview) = match format {
        ConfigFormat::Json { key, .. } => match want {
            Some(e) => {
                let new = jsonc::set(text, &[key, name], e).map_err(|e| bad(e.to_string()))?;
                (Some(new), format!("\"{key}\": {{ \"{name}\": {} }}", serde_json::to_string(e).unwrap_or_default()))
            }
            None => {
                let existing = jsonc::get(text, &[key, name]).map_err(|e| bad(e.to_string()))?;
                if let Some(v) = existing.filter(|v| !entry_is_ours(v, id)) {
                    not_ours(&v, notes);
                    return Ok(change(path.clone(), old.as_deref(), old.clone(), what, String::new(), true));
                }
                let new = jsonc::remove(text, &[key, name]).map_err(|e| bad(e.to_string()))?;
                (new.or_else(|| old.clone()), String::new())
            }
        },
        ConfigFormat::Toml => {
            let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| bad(format!("not valid TOML ({})", e.message())))?;
            let preview;
            match want {
                Some(e) => {
                    let servers = doc.entry("mcp_servers").or_insert_with(|| {
                        let mut t = toml_edit::Table::new();
                        t.set_implicit(true);
                        toml_edit::Item::Table(t)
                    });
                    let Some(servers) = servers.as_table_mut() else { return Err(bad("`mcp_servers` is not a table".into())) };
                    // update in place so comments and other keys in our table survive
                    let t = servers.entry(name).or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
                    let Some(t) = t.as_table_mut() else { return Err(bad(format!("`mcp_servers.{name}` is not a table"))) };
                    // in the order people read them, not the map's
                    const ORDER: [&str; 6] = ["type", "command", "args", "url", "httpUrl", "headers"];
                    let mut keys: Vec<(&String, &Value)> = e.as_object().into_iter().flatten().collect();
                    keys.sort_by_key(|(k, _)| ORDER.iter().position(|o| o == k).unwrap_or(ORDER.len()));
                    for (k, v) in keys {
                        // Codex names the headers `http_headers`
                        let k = if k == "headers" { "http_headers" } else { k.as_str() };
                        let Some(tv) = toml_value(v) else { continue };
                        if t.get(k).and_then(|i| i.as_value()).map(|x| x.to_string().trim().to_string()) != Some(tv.to_string().trim().to_string()) {
                            t[k] = toml_edit::Item::Value(tv);
                        }
                    }
                    preview = format!("[mcp_servers.{name}]\n{}", t);
                }
                None => {
                    preview = String::new();
                    let existing = doc.get("mcp_servers").and_then(|s| s.get(name)).and_then(|t| t.as_table_like()).map(|t| {
                        let get = |k: &str| t.get(k).and_then(|i| i.as_str()).map(|x| json!(x));
                        json!({"command": get("command"), "url": get("url")})
                    });
                    let existing = existing.map(|v| {
                        Value::Object(
                            v.as_object().into_iter().flatten().filter(|(_, x)| !x.is_null()).map(|(k, x)| (k.clone(), x.clone())).collect(),
                        )
                    });
                    if let Some(v) = existing.filter(|v| !entry_is_ours(v, id)) {
                        not_ours(&v, notes);
                    } else if let Some(servers) = doc.get_mut("mcp_servers").and_then(|s| s.as_table_mut()) {
                        servers.remove(name);
                    }
                }
            }
            let rendered = doc.to_string();
            let new = if old.is_none() && want.is_none() { None } else { Some(rendered) };
            (new, preview)
        }
    };
    Ok(change(path, old.as_deref(), new, what, preview, true))
}

/// A JSON value as a TOML value (objects become inline tables); `None` for null.
fn toml_value(v: &Value) -> Option<toml_edit::Value> {
    Some(match v {
        Value::String(s) => s.as_str().into(),
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64().unwrap_or(0.0).into(),
        },
        Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a.iter().filter_map(toml_value) {
                arr.push(x);
            }
            arr.into()
        }
        Value::Object(m) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, x) in m {
                if let Some(x) = toml_value(x) {
                    t.insert(k, x);
                }
            }
            t.into()
        }
        Value::Null => return None,
    })
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
    let window = window_name(id);
    for (path, format) in mcp_files(agent, scope, host) {
        let with_type = matches!(format, ConfigFormat::Json { with_type: true, .. });
        let stdio = setup.map(|s| entry(&s.server, with_type));
        changes.push(server_change(path.clone(), format, id, id, stdio.as_ref(), &mut notes)?);
        let http = match setup.map(|s| &s.window) {
            Some(Window::Keep) => continue,
            Some(Window::Set { url, token }) => match http_entry(agent, url, token) {
                Some(v) => Some(v),
                None => {
                    notes.push(format!(
                        "{} has no entry for an HTTP server here: connect it to the open app by hand (Settings ▸ AI shows the URL and header)",
                        agent.title()
                    ));
                    continue;
                }
            },
            Some(Window::Remove) | None => None,
        };
        // on top of the first entry's change: one change per file
        let Some(c1) = changes.pop() else { continue };
        let base = match c1.action {
            Action::Unchanged => read(&path)?,
            _ => c1.new.clone(),
        };
        let c2 = server_change_on(path.clone(), base, format, id, &window, http.as_ref(), &mut notes)?;
        let original = read(&path)?;
        let mut merged = change(path, original.as_deref(), c2.new.clone(), c1.what.clone(), c1.preview.clone(), true);
        let moved: Vec<&Change> = [&c1, &c2].into_iter().filter(|c| c.action != Action::Unchanged).collect();
        if !moved.is_empty() {
            merged.what = moved.iter().map(|c| c.what.as_str()).collect::<Vec<_>>().join(", ");
            merged.preview = moved.iter().map(|c| c.preview.as_str()).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("\n");
        }
        changes.push(merged);
    }
    // Claude Code keeps its own state file: when its command is here, it makes the change
    if agent == Agent::Claude
        && *scope == Scope::User
        && let Some(claude) = host.which("claude")
    {
        for c in changes.iter_mut().filter(|c| c.action != Action::Unchanged && c.what.starts_with("MCP server `")) {
            let names: Vec<String> =
                c.what.split(", ").filter_map(|w| w.strip_prefix("MCP server `").and_then(|n| n.strip_suffix('`'))).map(str::to_string).collect();
            let mut via = Vec::new();
            for name in &names {
                via.push((claude.clone(), vec!["mcp".to_string(), "remove".into(), name.clone(), "-s".into(), "user".into()]));
                if let Some(v) = c.new.as_deref().and_then(|t| jsonc::get(t, &["mcpServers", name]).ok().flatten()) {
                    via.push((claude.clone(), vec!["mcp".to_string(), "add-json".into(), name.clone(), v.to_string(), "-s".into(), "user".into()]));
                }
            }
            c.via = via;
            c.preview = format!("{}\n(through `claude mcp add-json {} … -s user`)", c.preview, names.join(" / "));
        }
    }
    let dir = skills_dir(agent, scope, host);
    // the shared folder keeps our skills while another agent that reads it still has our server
    let keep = setup.is_none() && dir == shared_skills(scope, host) && {
        let others: Vec<Agent> = Agent::ALL.into_iter().filter(|a| *a != agent && *a != Agent::Claude && has_server(*a, scope, id, host)).collect();
        if !others.is_empty() {
            let names: Vec<&str> = others.iter().map(|a| a.title()).collect();
            notes.push(format!("kept the skills in {}: {} read them too and still have the server", dir.display(), names.join(", ")));
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
                for a in o.applied.iter().filter(|a| a.what.contains("written directly")) {
                    out.push_str(&format!("  note: {}\n", a.what));
                }
                if write && !o.applied.is_empty() {
                    out.push_str(&format!("  Restart {} to pick this up.\n", o.agent.title()));
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

/// One agent in [`overview`]: found on this machine or not, and how far `setup` is installed.
#[derive(Debug, Clone, Serialize)]
pub struct AgentStatus {
    pub agent: Agent,
    pub id: &'static str,
    pub title: &'static str,
    pub found: bool,
    pub evidence: Vec<String>,
    /// `None` when its config could not be read (`error` says why).
    pub state: Option<State>,
    pub error: Option<String>,
}

/// Every supported agent with what [`detect`] found and its [`state`] for `setup`.
pub fn overview(setup: &Setup, scope: &Scope, host: &Host) -> Vec<AgentStatus> {
    detect(host)
        .into_iter()
        .map(|d| {
            let (state, error) = match state(d.agent, scope, setup, host) {
                Ok(s) => (Some(s), None),
                Err(e) => (None, Some(e.to_string())),
            };
            AgentStatus { agent: d.agent, id: d.agent.id(), title: d.agent.title(), found: d.found, evidence: d.evidence, state, error }
        })
        .collect()
}

#[cfg(test)]
mod tests;
