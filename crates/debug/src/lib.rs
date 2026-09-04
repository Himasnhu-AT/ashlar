//! `ashlar-debug`: drive a running ashlar app's UI from the shell, the way a browser automation CLI
//! drives a web page: snapshot the UI as an outline with short refs (`@e3`), act on refs, wait,
//! read values, take (annotated) screenshots, and run scripts of these as UI tests.
//!
//! ```text
//! ashlar-debug open ./target/debug/notes        # launch with a control port, remember the session
//! ashlar-debug snapshot -i                      # - button "Save" [ref=e3] …
//! ashlar-debug click @e3
//! ashlar-debug fill @e2 "Groceries" --submit
//! ashlar-debug wait --text "Saved"
//! ashlar-debug screenshot --annotate            # prints the PNG's path
//! ashlar-debug batch ui-test.txt                # one command per line, plus `assert …` lines
//! ashlar-debug close
//! ```
//!
//! Every command is one control-protocol call (`docs/control-protocol.md`) over
//! [`ashlar_control::Client`]. Human output by default, `--json` for one `{"ok":…}` line; exit codes
//! 0 (ok), 1 (failed: the app said no, an assertion failed, nothing answers), 2 (usage).
//!
//! The library is the whole CLI ([`run`]) so tests and other tools can embed it; [`demo`] is a
//! deterministic stand-in app that answers the same methods.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod batch;
pub mod demo;
pub mod session;
pub mod target;
mod verbs;
pub mod words;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use ashlar_control::{Client, ControlError};
use serde_json::{Value, json};

pub use session::Env;
use words::Args;

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Ok,
    /// The app refused, an assertion failed, or nothing answered.
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

/// The streams a command uses (real stdio in `main`, buffers in tests).
pub struct Io<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

/// A command's success: the machine result (`--json`) and the human text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Done {
    pub result: Value,
    pub text: String,
}

impl Done {
    pub fn new(result: Value, text: impl Into<String>) -> Self {
        Self { result, text: text.into() }
    }
}

/// A command's failure.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Fail {
    /// Bad arguments: the message and the verb's usage line.
    Usage(String, String),
    /// Everything else: the message, machine detail, and a hint for humans.
    Failed { error: String, detail: Option<Value>, hint: Option<String> },
}

impl Fail {
    pub fn msg(e: impl std::fmt::Display) -> Self {
        Fail::Failed { error: e.to_string(), detail: None, hint: None }
    }

    pub fn text(&self) -> String {
        match self {
            Fail::Usage(m, _) => m.clone(),
            Fail::Failed { error, .. } => error.clone(),
        }
    }

    /// From a control call: hints for stale refs and methods the app does not answer.
    pub fn control(method: &str, e: ControlError) -> Self {
        match e {
            ControlError::Remote { error, detail } => {
                let kind = detail.as_ref().and_then(|d| d.get("kind")).and_then(Value::as_str).unwrap_or("");
                let hint = if kind == "stale_ref" || error.starts_with("stale_ref") {
                    Some("refs come from the latest snapshot: run `ashlar-debug snapshot` again".to_string())
                } else if error.starts_with("unknown method") {
                    Some(format!("this app does not answer `{method}`; `ashlar-debug status` lists what it does"))
                } else {
                    None
                };
                Fail::Failed { error, detail, hint }
            }
            other => Fail::Failed {
                error: other.to_string(),
                detail: None,
                hint: Some("is the app still running? `ashlar-debug status` checks the session".into()),
            },
        }
    }
}

pub(crate) type Res = Result<Done, Fail>;

/// One invocation's state: options, the session it talks to, the connection once made.
pub(crate) struct Ctx<'e, 'io, 'a> {
    pub env: &'e Env,
    pub io: &'io mut Io<'a>,
    pub json: bool,
    pub session: String,
    pub port: Option<u16>,
    pub key_dir: Option<PathBuf>,
    pub app: Option<String>,
    /// Longest wait for one reply.
    pub timeout: Duration,
    pub conn: Option<Client>,
    /// Inside `batch` (no nested batches; step output is quieter).
    pub in_batch: bool,
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// `(verb, usage, about)` for help and usage errors.
pub(crate) const VERBS: &[(&str, &str, &str)] = &[
    ("open", "open <exe> [--wait MS] [-- app-args…]", "launch an app with a control port and remember it as the session"),
    ("connect", "connect <port>", "remember an app already running with `--control <port>`"),
    ("close", "close [--quit]", "quit the app `open` started (or forget a connected one; --quit quits it too)"),
    ("status", "status", "the session and the app's name, version and methods"),
    ("sessions", "sessions", "every recorded session and whether it answers"),
    ("snapshot", "snapshot [-i] [--scope T] [--depth N] [-c]", "the UI as an outline with refs (-i: interactive elements only)"),
    ("click", "click <T> [--button primary|secondary|middle] [--count N] [--mods Shift+Mod]", "click an element"),
    ("dblclick", "dblclick <T>", "double-click an element"),
    ("hover", "hover <T>", "move the pointer over an element"),
    ("focus", "focus <T>", "give an element keyboard focus"),
    ("fill", "fill <T> <text> [--submit]", "replace a text field's contents (--submit presses Enter)"),
    ("type", "type <text>", "type text into the focused element"),
    ("press", "press <keys>", "press a chord or a sequence: Mod+S, \"Tab Tab Enter\""),
    ("scroll", "scroll [T] [up|down|left|right] [px] | --dx N --dy N", "scroll (over an element, or the window)"),
    ("scrollintoview", "scrollintoview <T>", "scroll until an element is visible"),
    ("drag", "drag <from> <to> [--steps N]", "press on one element, move, release on another"),
    ("get", "get <text|value|rect|enabled|visible|focused|checked> <T>", "read one property of an element"),
    (
        "wait",
        "wait <T | ms> | --text S | --gone T | --enabled T | --idle  [--timeout MS]",
        "wait for an element, text, its absence, enablement, or an idle app",
    ),
    ("screenshot", "screenshot [path.png] [--target T] [--annotate] [--highlight T] [--pad N]", "save a PNG and print its path"),
    ("highlight", "highlight <T> [--ms N]", "flash an outline around an element in the live app"),
    ("exec", "exec <command> [json|-]", "run one of the app's commands"),
    ("commands", "commands [query]", "list the app's commands"),
    ("menu", "menu [id]", "print the menu tree, or invoke the item `id`"),
    ("logs", "logs [--lines N] [--level LEVEL]", "the app's recent log lines"),
    ("stats", "stats [--reset]", "the app's frame and job statistics"),
    ("devtools", "devtools [open|close|toggle|state]", "the in-app inspector"),
    ("call", "call <method> [json|-]", "any control method, raw"),
    ("batch", "batch <file|-> [--verbose]", "run a script of these commands and `assert` lines; stops at the first failure"),
    ("trace", "trace start <dir> | trace stop", "record a screenshot and snapshot after every action"),
    ("demo", "demo [--control PORT]", "serve a small stand-in app (for trying the CLI and for tests)"),
    ("version", "version", "print the version"),
];

pub(crate) fn usage_of(verb: &str) -> String {
    VERBS.iter().find(|v| v.0 == verb).map_or_else(String::new, |v| format!("ashlar-debug {}", v.1))
}

fn help(out: &mut dyn Write) {
    let _ = writeln!(out, "ashlar-debug {}: drive a running app's UI\n", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "usage: ashlar-debug [--session NAME | --port N [--key-dir D] | --app NAME] [--json] [--call-timeout MS] <command>\n");
    for (_, usage, about) in VERBS {
        let _ = writeln!(out, "  {usage}\n      {about}");
    }
    let _ = writeln!(
        out,
        "\nTargets (T): @e3 or e3 (a ref from the latest snapshot), id=<automation id>, text=<label>,\n\
         <role>=<label> (button=Save), or x,y in window points.\n\
         Exit codes: 0 ok, 1 failed, 2 usage. --json prints one {{\"ok\":…}} line."
    );
}

/// Run one invocation (`args` without the program name).
pub fn run(args: Vec<String>, env: &Env, io: &mut Io<'_>) -> Exit {
    let shown = args.clone();
    let mut a = Args::new(args);
    let json = a.flag(&["--json"]);
    let parsed = (|| -> Result<_, String> {
        let session = a.value(&["--session"])?.unwrap_or_else(|| env.session.clone());
        let port = a.number::<u16>(&["--port"])?;
        let key_dir = a.value(&["--key-dir"])?.map(PathBuf::from);
        let app = a.value(&["--app"])?;
        let timeout = a.number::<u64>(&["--call-timeout"])?.map(Duration::from_millis);
        Ok((session, port, key_dir, app, timeout))
    })();
    let (session, port, key_dir, app, timeout) = match parsed {
        Ok(p) => p,
        Err(e) => return finish(json, io, Err(Fail::Usage(e, "ashlar-debug --help".into()))),
    };
    if let Err(e) = session::check_name(&session) {
        return finish(json, io, Err(Fail::Usage(e, "ashlar-debug --help".into())));
    }
    let Some(verb) = a.positional() else {
        if a.flag(&["--help", "-h"]) || a.flag(&["--version", "-V"]) {
            help(io.out);
            return Exit::Ok;
        }
        help(io.err);
        return Exit::Usage;
    };
    let mut ctx = Ctx { env, io, json, session, port, key_dir, app, timeout: timeout.unwrap_or(DEFAULT_TIMEOUT), conn: None, in_batch: false };
    let r = ctx.run_verb(&verb, a, &shown);
    finish(json, ctx.io, r)
}

/// Print a command's outcome and turn it into an exit code.
pub(crate) fn finish(json: bool, io: &mut Io<'_>, r: Res) -> Exit {
    match r {
        Ok(d) => {
            if json {
                let _ = writeln!(io.out, "{}", json!({"ok": true, "result": d.result}));
            } else if !d.text.is_empty() {
                let _ = writeln!(io.out, "{}", d.text.trim_end_matches('\n'));
            }
            Exit::Ok
        }
        Err(Fail::Usage(msg, usage)) => {
            if json {
                let _ = writeln!(io.out, "{}", json!({"ok": false, "error": msg}));
            }
            let _ = writeln!(io.err, "error: {msg}\nusage: {usage}");
            Exit::Usage
        }
        Err(Fail::Failed { error, detail, hint }) => {
            if json {
                let mut o = json!({"ok": false, "error": error});
                if let Some(d) = detail {
                    o["detail"] = d;
                }
                let _ = writeln!(io.out, "{o}");
            } else {
                let _ = writeln!(io.err, "error: {error}");
                if let Some(h) = hint {
                    let _ = writeln!(io.err, "hint: {h}");
                }
            }
            Exit::Failed
        }
    }
}

/// `main` for the binary: process arguments, real stdio, the OS environment.
pub fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Env::from_os();
    let (stdin, stdout, stderr) = (std::io::stdin(), std::io::stdout(), std::io::stderr());
    let (mut i, mut o, mut e) = (stdin.lock(), stdout.lock(), stderr.lock());
    run(args, &env, &mut Io { input: &mut i, out: &mut o, err: &mut e }).into()
}
