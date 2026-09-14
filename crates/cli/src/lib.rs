//! The standard verbs of an app's command-line tool, over its command registry (`<tool>` is the
//! name given to [`Cli::new`], the command as it is installed):
//!
//! ```text
//! <tool> commands [--json] [--query Q]          list commands
//! <tool> describe <id>                          one command in full (parameter schema)
//! <tool> docs [path] [--json] [--query Q]       list or read documentation and skills
//! <tool> exec <id> [json|-]                     run one command
//! <tool> run <file.jsonl|-> [--keep-going]    run {"command","params"} lines
//! <tool> mcp [--isolate] [--per-command]        MCP server on stdio
//! <tool> serve                                  control protocol on stdio (a worker for `mcp --isolate`)
//! <tool> version [--json]
//! ```
//!
//! `commands`, `describe`, `exec`, `run` and `mcp` take `--connect PORT` to drive the running app
//! instead of a headless session. Replies are one line, `{"ok":true,"result":…}` or
//! `{"ok":false,"error":"…"}`; exit codes are 0 (ok), 1 (the command failed), 2 (usage). Apps add
//! their own verbs with [`Cli::verb`].
//!
//! ```no_run
//! # fn session() -> Box<dyn ashlar_command::Dispatch> { unimplemented!() }
//! fn main() -> std::process::ExitCode {
//!     ashlar_cli::Cli::new("notes", env!("CARGO_PKG_VERSION"), session).run()
//! }
//! ```
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod args;

pub use args::Args;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use ashlar_command::{CommandInfo, Dispatch};
use ashlar_control::{AppInfo, Client, DEFAULT_MAX_LINE, NoUi, Reply};
use ashlar_mcp::{Backend, McpServer, ServerOptions, Subprocess, ToolMode};
use serde_json::{Value, json};

/// How a verb ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok,
    /// The command (or one of a run's) failed.
    Failed,
    /// Bad arguments.
    Usage,
}

impl Exit {
    pub fn code(self) -> u8 {
        match self {
            Exit::Ok => 0,
            Exit::Failed => 1,
            Exit::Usage => 2,
        }
    }
}

impl From<Exit> for std::process::ExitCode {
    fn from(e: Exit) -> Self {
        std::process::ExitCode::from(e.code())
    }
}

/// The streams a verb uses (real stdio in [`Cli::run`], buffers in tests). `Send`, so `mcp` can
/// read and write on their own threads.
pub struct Io<'a> {
    pub input: &'a mut (dyn BufRead + Send),
    pub out: &'a mut (dyn Write + Send),
    pub err: &'a mut (dyn Write + Send),
}

impl Io<'_> {
    /// Print a reply line and turn it into an exit code.
    pub fn reply(&mut self, r: &Reply) -> Exit {
        let _ = writeln!(self.out, "{}", r.to_json(None));
        if r.is_ok() { Exit::Ok } else { Exit::Failed }
    }

    /// A usage error: the reply line on stdout, a hint on stderr.
    pub fn usage(&mut self, msg: impl std::fmt::Display, usage: &str) -> Exit {
        let _ = writeln!(self.out, "{}", Reply::err(&msg).to_json(None));
        let _ = writeln!(self.err, "{msg}\nusage: {usage}");
        Exit::Usage
    }
}

type SessionFn = Box<dyn FnMut() -> Box<dyn Dispatch>>;
type VerbFn = Box<dyn FnMut(Args, &mut Io<'_>) -> Exit>;

struct Verb {
    name: String,
    usage: String,
    about: String,
    run: VerbFn,
}

/// An app's CLI: standard verbs plus its own.
pub struct Cli {
    app: AppInfo,
    session: SessionFn,
    key_dir: Option<PathBuf>,
    instructions: String,
    docs_dir: Option<PathBuf>,
    docs: Vec<ashlar_mcp::Doc>,
    verbs: Vec<Verb>,
}

/// Where commands go: a headless session or the running app.
enum Target {
    Local(Box<dyn Dispatch>),
    Remote(Client),
}

impl Target {
    fn call(&mut self, method: &str, params: Value, app: &AppInfo) -> Reply {
        match self {
            Target::Local(d) => ashlar_control::answer(method, params, d.as_mut(), &mut NoUi, app),
            Target::Remote(c) => c.request(method, params).unwrap_or_else(|e| Reply::err(format!("the app is not reachable: {e}"))),
        }
    }
}

const STANDARD: &[(&str, &str, &str)] = &[
    ("commands", "commands [--json] [--query Q] [--connect PORT]", "list commands"),
    ("describe", "describe <id> [--connect PORT]", "one command in full, with its parameter schema"),
    ("docs", "docs [path] [--json] [--query Q] [--section S]", "list or read documentation and skills"),
    ("exec", "exec <id> [json|-] [--connect PORT]", "run one command (params as JSON, `-` reads stdin)"),
    ("run", "run <file.jsonl|-> [--keep-going] [--connect PORT]", "run {\"command\",\"params\"} lines, one reply each"),
    ("mcp", "mcp [--connect PORT | --isolate] [--per-command]", "MCP server on stdio"),
    ("serve", "serve", "control protocol on stdio (a worker for `mcp --isolate`)"),
    ("version", "version [--json]", "print the version"),
];

impl Cli {
    /// `session` makes a fresh headless session (one per invocation).
    pub fn new(name: impl Into<String>, version: impl Into<String>, session: impl FnMut() -> Box<dyn Dispatch> + 'static) -> Self {
        Self {
            app: AppInfo::new(name, version),
            session: Box::new(session),
            key_dir: None,
            instructions: String::new(),
            docs_dir: None,
            docs: Vec::new(),
            verbs: Vec::new(),
        }
    }

    /// Where the running app writes its control key (its settings folder); enables `--connect`.
    pub fn key_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.key_dir = Some(dir.into());
        self
    }

    /// The MCP server's instructions (what the app is, how to drive it).
    pub fn instructions(mut self, text: impl Into<String>) -> Self {
        self.instructions = text.into();
        self
    }

    /// Serve the markdown files under `dir` (read when a verb needs them; symbolic links skipped),
    /// when no [`Self::docs`] are given. Only the directory named here is read: a shipped app
    /// embeds its pages instead.
    pub fn docs_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.docs_dir = Some(dir.into());
        self
    }

    /// The documents and skills for agents (`docs`, and the MCP server's resources and tools);
    /// the first is the entry point.
    pub fn docs(mut self, docs: impl IntoIterator<Item = ashlar_mcp::Doc>) -> Self {
        self.docs.extend(docs);
        self
    }

    /// Add an app-specific verb (or replace a standard one of the same name).
    pub fn verb(mut self, name: &str, usage: &str, about: &str, run: impl FnMut(Args, &mut Io<'_>) -> Exit + 'static) -> Self {
        self.verbs.retain(|v| v.name != name);
        self.verbs.push(Verb { name: name.into(), usage: usage.into(), about: about.into(), run: Box::new(run) });
        self
    }

    /// Run with the process arguments and real stdio.
    pub fn run(mut self) -> std::process::ExitCode {
        let args: Vec<String> = std::env::args().skip(1).collect();
        // the handles, not their locks: those cannot move to another thread
        let (mut i, mut o, mut e) = (std::io::BufReader::new(std::io::stdin()), std::io::stdout(), std::io::stderr());
        self.run_with(args, &mut Io { input: &mut i, out: &mut o, err: &mut e }).into()
    }

    /// Run one invocation (`args` without the program name).
    pub fn run_with(&mut self, args: Vec<String>, io: &mut Io<'_>) -> Exit {
        let mut it = args.into_iter();
        let Some(verb) = it.next() else {
            self.help(io.err);
            return Exit::Usage;
        };
        let args = Args::new(it);
        if let Some(v) = self.verbs.iter_mut().find(|v| v.name == verb) {
            return (v.run)(args, io);
        }
        match verb.as_str() {
            "commands" => self.commands(args, io),
            "describe" => self.describe(args, io),
            "docs" => self.docs_verb(args, io),
            "exec" => self.exec(args, io),
            "run" => self.run_lines(args, io),
            "mcp" => self.mcp(args, io),
            "serve" => self.serve(args, io),
            "version" | "--version" | "-V" => self.version(args, io),
            "help" | "--help" | "-h" => {
                self.help(io.out);
                Exit::Ok
            }
            other => {
                let _ = writeln!(io.err, "unknown command `{other}`\n");
                self.help(io.err);
                Exit::Usage
            }
        }
    }

    fn help(&self, out: &mut dyn Write) {
        let name = &self.app.name;
        let _ = writeln!(out, "{name} {}\n\nusage:", self.app.version);
        for (verb, usage, about) in STANDARD {
            if !self.verbs.iter().any(|v| v.name == *verb) {
                let _ = writeln!(out, "  {name} {usage:<52} {about}");
            }
        }
        for v in &self.verbs {
            let _ = writeln!(out, "  {name} {:<52} {}", v.usage, v.about);
        }
        let _ = writeln!(out, "\nReplies are one JSON line {{\"ok\":…}}; exit codes 0 ok, 1 failed, 2 usage.");
    }

    fn usage_of(verb: &str) -> &'static str {
        STANDARD.iter().find(|s| s.0 == verb).map_or("", |s| s.1)
    }

    /// The target for `--connect PORT`, or a fresh headless session.
    fn target(&mut self, connect: Option<u16>) -> Result<Target, String> {
        match connect {
            Some(port) => {
                let dir = self.key_dir.as_ref().ok_or("this app's CLI cannot connect (no key folder configured)")?;
                Client::connect(port, dir).map(Target::Remote).map_err(|e| e.to_string())
            }
            None => Ok(Target::Local((self.session)())),
        }
    }

    fn commands(&mut self, mut args: Args, io: &mut Io<'_>) -> Exit {
        let usage = Self::usage_of("commands");
        let json = args.flag("--json");
        let query = match args.value("--query") {
            Ok(q) => q.unwrap_or_default(),
            Err(e) => return io.usage(e, usage),
        };
        let connect = args.number::<u16>("--connect");
        if let Err(e) = args.finish() {
            return io.usage(e, usage);
        }
        let connect = match connect {
            Ok(c) => c,
            Err(e) => return io.usage(e, usage),
        };
        let mut target = match self.target(connect) {
            Ok(t) => t,
            Err(e) => return io.reply(&Reply::err(e)),
        };
        let r = target.call("engine.commands", json!({"query": query}), &self.app);
        if json {
            return io.reply(&r);
        }
        match r {
            Reply::Ok(v) => {
                let list: Vec<CommandInfo> = serde_json::from_value(v).unwrap_or_default();
                let width = list.iter().map(|c| c.id.chars().count()).max().unwrap_or(0);
                for c in &list {
                    let mut line = format!("{:<width$}  {}", c.id, c.label);
                    if let Some(s) = &c.shortcut {
                        line.push_str(&format!("  [{s}]"));
                    }
                    if !c.enabled {
                        line.push_str(&format!("  (disabled: {})", c.disabled_reason.as_deref().unwrap_or("not available")));
                    }
                    let _ = writeln!(io.out, "{}", line.trim_end());
                }
                Exit::Ok
            }
            e => io.reply(&e),
        }
    }

    fn describe(&mut self, mut args: Args, io: &mut Io<'_>) -> Exit {
        let usage = Self::usage_of("describe");
        // Options first: `--connect PORT` must not lose its value to a positional.
        let connect = args.number::<u16>("--connect");
        let id = args.positional();
        if let Err(e) = args.finish() {
            return io.usage(e, usage);
        }
        let Some(id) = id else { return io.usage("describe needs a command id", usage) };
        let connect = match connect {
            Ok(c) => c,
            Err(e) => return io.usage(e, usage),
        };
        let mut target = match self.target(connect) {
            Ok(t) => t,
            Err(e) => return io.reply(&Reply::err(e)),
        };
        let r = match target.call("engine.commands", json!({}), &self.app) {
            Reply::Ok(v) => {
                let list: Vec<CommandInfo> = serde_json::from_value(v).unwrap_or_default();
                match list.into_iter().find(|c| c.id == id) {
                    Some(c) => Reply::ok(serde_json::to_value(c).unwrap_or_default()),
                    None => Reply::err(format!("no command `{id}` (see `{} commands`)", self.app.name)),
                }
            }
            e => e,
        };
        io.reply(&r)
    }

    fn exec(&mut self, mut args: Args, io: &mut Io<'_>) -> Exit {
        let usage = Self::usage_of("exec");
        let connect = args.number::<u16>("--connect");
        let id = args.positional();
        let raw = args.positional();
        if let Err(e) = args.finish() {
            return io.usage(e, usage);
        }
        let Some(id) = id else { return io.usage("exec needs a command id", usage) };
        let text = match raw.as_deref() {
            None => String::new(),
            Some("-") => {
                let mut s = String::new();
                if let Err(e) = io.input.read_to_string(&mut s) {
                    return io.usage(format!("could not read parameters from stdin: {e}"), usage);
                }
                s
            }
            Some(t) => t.to_string(),
        };
        let params = if text.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => v,
                Err(e) => return io.usage(format!("parameters are not valid JSON: {e}"), usage),
            }
        };
        let connect = match connect {
            Ok(c) => c,
            Err(e) => return io.usage(e, usage),
        };
        let mut target = match self.target(connect) {
            Ok(t) => t,
            Err(e) => return io.reply(&Reply::err(e)),
        };
        let r = target.call("engine.execute", json!({"command": id, "params": params}), &self.app);
        io.reply(&r)
    }

    fn run_lines(&mut self, mut args: Args, io: &mut Io<'_>) -> Exit {
        let usage = Self::usage_of("run");
        let keep_going = args.flag("--keep-going");
        let connect = args.number::<u16>("--connect");
        let file = args.positional();
        if let Err(e) = args.finish() {
            return io.usage(e, usage);
        }
        let Some(file) = file else { return io.usage("run needs a file (or `-` for stdin)", usage) };
        let text = if file == "-" {
            let mut s = String::new();
            match io.input.read_to_string(&mut s) {
                Ok(_) => s,
                Err(e) => return io.usage(format!("could not read stdin: {e}"), usage),
            }
        } else {
            match std::fs::read_to_string(&file) {
                Ok(s) => s,
                Err(e) => return io.usage(format!("could not read {file}: {e}"), usage),
            }
        };
        let connect = match connect {
            Ok(c) => c,
            Err(e) => return io.usage(e, usage),
        };
        let mut target = match self.target(connect) {
            Ok(t) => t,
            Err(e) => return io.reply(&Reply::err(e)),
        };
        let mut exit = Exit::Ok;
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let r = match serde_json::from_str::<Value>(line) {
                Ok(v) => match v.get("command").and_then(Value::as_str) {
                    Some(id) => {
                        let params = v.get("params").cloned().unwrap_or(json!({}));
                        target.call("engine.execute", json!({"command": id, "params": params}), &self.app)
                    }
                    None => Reply::err(format!("line {}: expected {{\"command\": \"…\", \"params\": {{…}}}}", n.saturating_add(1))),
                },
                Err(e) => Reply::err(format!("line {}: bad JSON: {e}", n.saturating_add(1))),
            };
            if io.reply(&r) == Exit::Failed {
                exit = Exit::Failed;
                if !keep_going {
                    break;
                }
            }
        }
        exit
    }

    fn mcp(&mut self, mut args: Args, io: &mut Io<'_>) -> Exit {
        let usage = Self::usage_of("mcp");
        let isolate = args.flag("--isolate");
        let per_command = args.flag("--per-command");
        let connect = args.number::<u16>("--connect");
        if let Err(e) = args.finish() {
            return io.usage(e, usage);
        }
        let backend = match connect {
            Err(e) => return io.usage(e, usage),
            Ok(Some(_)) if isolate => return io.usage("--connect and --isolate exclude each other", usage),
            Ok(Some(port)) => {
                let Some(dir) = &self.key_dir else { return io.usage("this app's CLI cannot connect (no key folder configured)", usage) };
                match Client::connect(port, dir) {
                    Ok(c) => Backend::Remote(c),
                    Err(e) => {
                        let _ = writeln!(io.err, "{e}");
                        return Exit::Failed;
                    }
                }
            }
            Ok(None) if isolate => match std::env::current_exe() {
                Ok(exe) => Backend::Subprocess(Subprocess::new(exe, ["serve"])),
                Err(e) => {
                    let _ = writeln!(io.err, "cannot find this program to start workers: {e}");
                    return Exit::Failed;
                }
            },
            Ok(None) => Backend::Headless((self.session)()),
        };
        let mut opts = ServerOptions::new(&self.app.name, &self.app.version).instructions(&self.instructions);
        if per_command {
            opts = opts.tools(ToolMode::PerCommand);
        }
        let mut server = McpServer::new(backend, opts).docs(self.resolve_docs());
        // stdout carries only the protocol; problems go to stderr.
        match server.serve(&mut *io.input, &mut *io.out) {
            Ok(()) => Exit::Ok,
            Err(e) => {
                let _ = writeln!(io.err, "mcp: {e}");
                Exit::Failed
            }
        }
    }

    fn resolve_docs(&self) -> Vec<ashlar_mcp::Doc> {
        match &self.docs_dir {
            Some(dir) if self.docs.is_empty() => ashlar_mcp::docs::load_docs_from_dir(dir),
            _ => self.docs.clone(),
        }
    }

    fn docs_verb(&mut self, mut args: Args, io: &mut Io<'_>) -> Exit {
        let usage = Self::usage_of("docs");
        let json = args.flag("--json");
        let query = match args.value("--query") {
            Ok(q) => q,
            Err(e) => return io.usage(e, usage),
        };
        let section = match args.value("--section") {
            Ok(s) => s,
            Err(e) => return io.usage(e, usage),
        };
        let path = args.positional();
        if let Err(e) = args.finish() {
            return io.usage(e, usage);
        }

        let docs = self.resolve_docs();
        if let Some(target) = path {
            match ashlar_mcp::docs::find(&docs, &target) {
                Ok(d) => match d.read_content() {
                    Ok(content) => {
                        if json {
                            io.reply(&Reply::ok(json!({
                                "path": d.path,
                                "title": d.title,
                                "description": d.description,
                                "section": d.section,
                                "uri": d.uri,
                                "content": content
                            })))
                        } else {
                            let _ = write!(io.out, "{content}");
                            if !content.ends_with('\n') {
                                let _ = writeln!(io.out);
                            }
                            Exit::Ok
                        }
                    }
                    Err(e) => io.reply(&Reply::err(format!("reading {}: {e}", d.path))),
                },
                Err(e) => io.reply(&Reply::err(format!("{e} (see `{} docs`)", self.app.name))),
            }
        } else {
            let filtered: Vec<&ashlar_mcp::Doc> = docs.iter().filter(|d| d.matches(query.as_deref(), section.as_deref())).collect();

            if json {
                let list: Vec<Value> = filtered
                    .iter()
                    .map(|d| {
                        json!({
                            "path": d.path,
                            "title": d.title,
                            "description": d.description,
                            "section": d.section,
                            "uri": d.uri,
                        })
                    })
                    .collect();
                io.reply(&Reply::ok(json!({"docs": list})))
            } else {
                if filtered.is_empty() {
                    let _ = writeln!(io.out, "no documentation found");
                    return Exit::Ok;
                }
                let width = filtered.iter().map(|d| d.path.chars().count()).max().unwrap_or(0);
                for d in &filtered {
                    let _ = writeln!(io.out, "{:<width$}  {}", d.path, d.title);
                }
                Exit::Ok
            }
        }
    }

    fn serve(&mut self, args: Args, io: &mut Io<'_>) -> Exit {
        if let Err(e) = args.finish() {
            return io.usage(e, Self::usage_of("serve"));
        }
        let mut session = (self.session)();
        let app = self.app.clone();
        ashlar_control::serve_stream(&mut *io.input, &mut *io.out, DEFAULT_MAX_LINE, |method, params| {
            ashlar_control::answer(method, params, session.as_mut(), &mut NoUi, &app)
        });
        Exit::Ok
    }

    fn version(&mut self, mut args: Args, io: &mut Io<'_>) -> Exit {
        let json = args.flag("--json");
        if let Err(e) = args.finish() {
            return io.usage(e, Self::usage_of("version"));
        }
        if json {
            return io.reply(&Reply::ok(json!({"name": self.app.name, "version": self.app.version})));
        }
        let _ = writeln!(io.out, "{} {}", self.app.name, self.app.version);
        Exit::Ok
    }
}
