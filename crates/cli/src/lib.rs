//! The standard verbs of an app's command-line tool, over its command registry (`<tool>` is the
//! name given to [`Cli::new`], the command as it is installed):
//!
//! ```text
//! <tool> commands [--json] [--query Q]          list commands
//! <tool> describe <id>                          one command in full (parameter schema)
//! <tool> docs [category|path.md] [--query Q]   the documentation: categories, one category, one file
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

    /// A failure: with `json`, the reply line on stdout (as [`Io::reply`]); without it, one line
    /// of plain text on stderr (`<tool>: <message>`), for a person reading a terminal.
    pub fn fail(&mut self, json: bool, tool: &str, msg: impl std::fmt::Display) -> Exit {
        if json {
            return self.reply(&Reply::err(msg));
        }
        let _ = writeln!(self.err, "{tool}: {msg}");
        Exit::Failed
    }

    /// A reply: with `json` the line on stdout; without it a success is the result as indented
    /// JSON on stdout and an error is plain text on stderr (see [`Io::fail`]).
    pub fn reply_as(&mut self, json: bool, tool: &str, r: &Reply) -> Exit {
        match r {
            _ if json => self.reply(r),
            Reply::Ok(v) => {
                let _ = writeln!(self.out, "{}", serde_json::to_string_pretty(v).unwrap_or_default());
                Exit::Ok
            }
            Reply::Err { error, .. } => self.fail(false, tool, error),
        }
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
    docs: ashlar_mcp::Library,
    user_docs: Option<PathBuf>,
    prompts: Option<String>,
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
    (
        "docs",
        "docs [category|path.md] [--query Q] [--json] [--where] | docs save|append <path.md> [--file F] [--replace-builtin] | docs delete <path.md>",
        "the documentation: categories, a category, or a file verbatim; save, append to or delete a page of your own",
    ),
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
            docs: ashlar_mcp::Library::new(),
            user_docs: None,
            prompts: None,
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

    /// The documentation library (`docs`, and the MCP server's tools and resources), usually
    /// the app's built-in pages.
    pub fn docs(mut self, library: ashlar_mcp::Library) -> Self {
        self.docs = library;
        self
    }

    /// The user's own documentation folder, read on top of [`Self::docs`] whenever they are
    /// used (same path: the user's page wins). `docs --where` prints it; it is never created.
    pub fn user_docs(mut self, dir: impl Into<PathBuf>) -> Self {
        self.user_docs = Some(dir.into());
        self
    }

    /// The documentation category `mcp` serves as prompts ([`McpServer::prompts`]).
    pub fn prompts(mut self, category: impl Into<String>) -> Self {
        self.prompts = Some(category.into());
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
        let _ = writeln!(
            out,
            "\nReplies are one JSON line {{\"ok\":…}} (`commands`, `docs` and `version` print text unless --json, and their errors as text on stderr); exit codes 0 ok, 1 failed, 2 usage."
        );
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
        let name = self.app.name.clone();
        let mut target = match self.target(connect) {
            Ok(t) => t,
            Err(e) => return io.fail(json, &name, e),
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
            e => io.reply_as(false, &name, &e),
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
                match list.iter().find(|c| c.id == id) {
                    Some(c) => Reply::ok(serde_json::to_value(c).unwrap_or_default()),
                    None => {
                        let hint = ashlar_command::did_you_mean_command(&id, &list).map(|h| format!("; {h}")).unwrap_or_default();
                        Reply::err(format!("no command `{id}`{hint} (see `{} commands`)", self.app.name))
                    }
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
        let mut server = McpServer::new(backend, opts).docs(self.library());
        if let Some(dir) = &self.user_docs {
            server = server.user_docs(dir.clone());
        }
        if let Some(c) = &self.prompts {
            server = server.prompts(c.clone());
        }
        // stdout carries only the protocol; problems go to stderr.
        match server.serve(&mut *io.input, &mut *io.out) {
            Ok(()) => Exit::Ok,
            Err(e) => {
                let _ = writeln!(io.err, "mcp: {e}");
                Exit::Failed
            }
        }
    }

    /// The built-in pages with the user's folder on top.
    fn library(&self) -> ashlar_mcp::Library {
        match &self.user_docs {
            Some(dir) => self.docs.clone().overlay_dir(dir),
            None => self.docs.clone(),
        }
    }

    fn docs_verb(&mut self, mut args: Args, io: &mut Io<'_>) -> Exit {
        use ashlar_mcp::docs::{category_json, doc_json, node_json};
        let usage = Self::usage_of("docs");
        let json = args.flag("--json");
        let place = args.flag("--where");
        let query = match args.value("--query") {
            Ok(q) => q,
            Err(e) => return io.usage(e, usage),
        };
        let file = match args.value("--file") {
            Ok(f) => f,
            Err(e) => return io.usage(e, usage),
        };
        let replace = args.flag("--replace-builtin");
        let target = args.positional();
        let write = target.as_deref().filter(|t| matches!(*t, "save" | "append" | "delete")).map(str::to_string);
        let page = if write.is_some() { args.positional() } else { None };
        if let Err(e) = args.finish() {
            return io.usage(e, usage);
        }
        if let Some(verb) = write {
            return self.docs_write(&verb, page, file, replace, json, io);
        }
        if file.is_some() || replace {
            return io.usage("--file and --replace-builtin go with `docs save` / `docs append`", usage);
        }
        let name = self.app.name.clone();
        if place {
            return match &self.user_docs {
                Some(dir) if json => io.reply(&Reply::ok(json!({"path": dir.display().to_string(), "exists": dir.is_dir()}))),
                Some(dir) => {
                    let _ = writeln!(io.out, "{}", dir.display());
                    if !dir.is_dir() {
                        let _ = writeln!(io.err, "(not created yet: make it and add .md files, in folders for categories, to add your own pages)");
                    }
                    Exit::Ok
                }
                None => io.fail(json, &name, format!("{name} has no folder for your own documentation")),
            };
        }
        let lib = self.library();
        let target = target.map(|t| t.trim().trim_matches('/').to_string());
        // a file, verbatim
        if let Some(t) = target.as_deref().filter(|t| t.ends_with(".md") || t.starts_with("docs://")) {
            return match lib.read(t) {
                Ok(text) if json => {
                    let doc =
                        lib.docs().iter().find(|d| d.path == t.trim_start_matches("docs://")).map(doc_json).unwrap_or_else(|| json!({"path": t}));
                    let mut v = doc;
                    if let Some(o) = v.as_object_mut() {
                        o.insert("text".into(), json!(text));
                    }
                    io.reply(&Reply::ok(v))
                }
                Ok(text) => {
                    let _ = write!(io.out, "{text}");
                    Exit::Ok
                }
                Err(e) => io.fail(json, &name, format!("{e} (see `{name} docs`)")),
            };
        }
        let category = target.unwrap_or_default();
        let Some(cat) = lib.category(&category) else {
            let close = lib.close_matches(&category);
            let hint = if close.is_empty() { String::new() } else { format!("; did you mean {}?", close.join(", ")) };
            return io.fail(json, &name, format!("no category `{category}`{hint} (see `{name} docs`)"));
        };
        if let Some(q) = query.as_deref() {
            let hits = lib.list(Some(&cat.path), Some(q), true);
            if json {
                return io.reply(&Reply::ok(json!({"docs": hits.into_iter().map(doc_json).collect::<Vec<_>>()})));
            }
            if hits.is_empty() {
                let _ = writeln!(io.out, "nothing matches `{q}`");
            }
            write_docs(io.out, &hits);
            return Exit::Ok;
        }
        if cat.path.is_empty() {
            let Some(tree) = lib.tree("") else { return Exit::Ok };
            if json {
                return io.reply(&Reply::ok(node_json(&tree)));
            }
            let _ = writeln!(io.out, "{}: {} documents", tree.title, tree.total);
            if !tree.description.is_empty() {
                let _ = writeln!(io.out, "{}", tree.description);
            }
            let _ = writeln!(io.out);
            write_tree(io.out, &tree.children, 1);
            write_docs(io.out, &lib.list(Some(""), None, false));
            let _ = writeln!(io.out, "\n`{name} docs <category>` shows one, `{name} docs <path.md>` prints a file, `--query Q` searches.");
            return Exit::Ok;
        }
        if json {
            return io.reply(&Reply::ok(category_json(&lib, &cat)));
        }
        let _ = writeln!(io.out, "{} ({}/)", cat.title, cat.path);
        if !cat.overview.is_empty() {
            let _ = writeln!(io.out, "\n{}", cat.overview);
        }
        if let Some(node) = lib.tree(&cat.path)
            && !node.children.is_empty()
        {
            let _ = writeln!(io.out, "\nSubcategories:");
            write_tree(io.out, &node.children, 1);
        }
        let docs = lib.list(Some(&cat.path), None, false);
        if !docs.is_empty() {
            let _ = writeln!(io.out, "\nDocuments:");
            write_docs(io.out, &docs);
        }
        Exit::Ok
    }

    /// `docs save|append|delete <path>`: write the user's own pages (text from `--file` or stdin).
    fn docs_write(&mut self, verb: &str, page: Option<String>, file: Option<String>, replace: bool, json: bool, io: &mut Io<'_>) -> Exit {
        let usage = Self::usage_of("docs");
        let name = self.app.name.clone();
        let Some(dir) = self.user_docs.clone() else { return io.fail(json, &name, format!("{name} has no folder for your own documentation")) };
        let Some(page) = page else { return io.usage(format!("docs {verb} needs a page path, e.g. memory/style.md"), usage) };
        let text = if verb == "delete" {
            String::new()
        } else {
            let read = match &file {
                Some(f) if f != "-" => std::fs::read_to_string(f).map_err(|e| format!("could not read {f}: {e}")),
                _ => {
                    let mut s = String::new();
                    io.input.read_to_string(&mut s).map(|_| s).map_err(|e| format!("could not read stdin: {e}"))
                }
            };
            match read {
                Ok(t) => t,
                Err(e) => return io.usage(e, usage),
            }
        };
        let built_in = self.docs.built_in_only();
        let result = match verb {
            "save" => ashlar_mcp::docs::write::save(&dir, &built_in, &page, &text, replace),
            "append" => ashlar_mcp::docs::write::append(&dir, &page, &text),
            _ => ashlar_mcp::docs::write::delete(&dir, &built_in, &page),
        };
        match result {
            Ok(w) if json => io.reply(&Reply::ok(json!({"path": w.path, "file": w.file.display().to_string(), "created": w.created,
                "bytes": w.bytes, "replacesBuiltIn": w.replaces_built_in}))),
            Ok(w) => {
                let done = match verb {
                    "delete" => "deleted",
                    _ if w.created => "created",
                    _ => "saved",
                };
                let _ = writeln!(io.out, "{done} {} ({})", w.path, w.file.display());
                Exit::Ok
            }
            Err(e) => io.fail(json, &name, e),
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

/// Categories as an indented list: path, title, documents (in it and below).
fn write_tree(out: &mut dyn Write, nodes: &[ashlar_mcp::docs::Node], depth: usize) {
    for n in nodes {
        let path = format!("{}{}/", "  ".repeat(depth), n.path);
        let _ = writeln!(out, "{path:<36} {} ({})", n.title, n.total);
        write_tree(out, &n.children, depth + 1);
    }
}

/// Documents: path and title, with the description under it; the user's own marked.
fn write_docs(out: &mut dyn Write, docs: &[&ashlar_mcp::docs::Doc]) {
    for d in docs {
        let mine = if d.source == ashlar_mcp::docs::Source::User { "  [yours]" } else { "" };
        let _ = writeln!(out, "  {}  {}{mine}", d.path, d.title);
        if !d.description.is_empty() {
            let _ = writeln!(out, "      {}", d.description);
        }
    }
}
