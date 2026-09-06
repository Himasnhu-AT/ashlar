#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The standard verbs, their one-line replies and exit codes.

use ashlar_cli::{Args, Cli, Exit, Io};
use ashlar_command::{CommandError, CommandInfo, Dispatch};
use ashlar_control::{AppInfo, Config, NoUi, Server, handle};
use serde_json::{Value, json};

#[derive(Default)]
struct Counter(i64);

impl Dispatch for Counter {
    fn commands(&self) -> Vec<CommandInfo> {
        let c = |id: &str, label: &str, enabled: bool| CommandInfo {
            id: id.into(),
            label: label.into(),
            menu: vec![],
            shortcut: (id == "count.add").then(|| "Mod+Up".to_string()),
            params_schema: json!({"type": "object", "properties": {"by": {"type": "integer"}}}),
            params_text: None,
            enabled,
            disabled_reason: (!enabled).then(|| "already zero".to_string()),
            mutates: true,
        };
        vec![c("count.add", "Add", true), c("count.fail", "Fail", true), c("count.reset", "Reset", false)]
    }
    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        match id {
            "count.add" => {
                self.0 += params.get("by").and_then(Value::as_i64).unwrap_or(1);
                Ok(json!(self.0))
            }
            "count.fail" => Err(CommandError::Failed { id: id.into(), msg: "it always fails".into() }),
            _ => Err(CommandError::Unknown { id: id.into() }),
        }
    }
}

fn key_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ashlar-cli-test-{}", std::process::id()))
}

fn cli() -> Cli {
    Cli::new("counter", "1.2.3", || Box::new(Counter::default())).key_dir(key_dir()).instructions("A counter.")
}

struct Out {
    exit: Exit,
    out: String,
    err: String,
}

impl Out {
    fn lines(&self) -> Vec<Value> {
        self.out.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}"))).collect()
    }
    fn reply(&self) -> Value {
        let l = self.lines();
        assert_eq!(l.len(), 1, "{}", self.out);
        l[0].clone()
    }
}

fn run_in(cli: &mut Cli, args: &[&str], stdin: &str) -> Out {
    let (mut input, mut out, mut err) = (stdin.as_bytes(), Vec::new(), Vec::new());
    let exit = cli.run_with(args.iter().map(|s| s.to_string()).collect(), &mut Io { input: &mut input, out: &mut out, err: &mut err });
    Out { exit, out: String::from_utf8(out).unwrap(), err: String::from_utf8(err).unwrap() }
}

fn run(args: &[&str]) -> Out {
    run_in(&mut cli(), args, "")
}

#[test]
fn version() {
    let o = run(&["version"]);
    assert_eq!((o.exit, o.out.as_str()), (Exit::Ok, "counter 1.2.3\n"));
    assert_eq!(run(&["version", "--json"]).reply(), json!({"ok": true, "result": {"name": "counter", "version": "1.2.3"}}));
    assert_eq!(run(&["--version"]).exit, Exit::Ok);
}

#[test]
fn commands_human_and_json() {
    let o = run(&["commands"]);
    assert_eq!(o.exit, Exit::Ok);
    assert_eq!(o.out, "count.add    Add  [Mod+Up]\ncount.fail   Fail\ncount.reset  Reset  (disabled: already zero)\n");
    let r = run(&["commands", "--json"]).reply();
    assert_eq!(r["ok"], true);
    assert_eq!(r["result"].as_array().unwrap().len(), 3);
    assert_eq!(r["result"][0]["id"], "count.add");
    let r = run(&["commands", "--json", "--query", "RESET"]).reply();
    assert_eq!(r["result"].as_array().unwrap().len(), 1);
    assert_eq!(run(&["commands", "--jsn"]).exit, Exit::Usage);
}

#[test]
fn describe() {
    let r = run(&["describe", "count.add"]).reply();
    assert_eq!((r["ok"].clone(), r["result"]["params_schema"]["properties"]["by"]["type"].clone()), (json!(true), json!("integer")));
    let o = run(&["describe", "count.nope"]);
    assert_eq!(o.exit, Exit::Failed);
    assert!(o.reply()["error"].as_str().unwrap().contains("no command `count.nope`"));
    let o = run(&["describe"]);
    assert_eq!(o.exit, Exit::Usage);
    assert!(o.err.contains("usage: describe <id>"), "{}", o.err);
}

#[test]
fn exec() {
    assert_eq!(run(&["exec", "count.add"]).reply(), json!({"ok": true, "result": 1}));
    assert_eq!(run(&["exec", "count.add", r#"{"by": 41}"#]).reply(), json!({"ok": true, "result": 41}));
    assert_eq!(run_in(&mut cli(), &["exec", "count.add", "-"], r#"{"by": 7}"#).reply(), json!({"ok": true, "result": 7}));
    let o = run(&["exec", "count.fail"]);
    assert_eq!(o.exit, Exit::Failed);
    assert_eq!(
        o.reply(),
        json!({"ok": false, "error": "it always fails", "detail": {"kind": "failed", "id": "count.fail", "msg": "it always fails"}})
    );
    let o = run(&["exec", "count.nope"]);
    assert_eq!((o.exit, o.reply()["detail"]["kind"].clone()), (Exit::Failed, json!("unknown")));
    let o = run(&["exec", "count.add", "{by: 1}"]);
    assert_eq!(o.exit, Exit::Usage);
    assert!(o.reply()["error"].as_str().unwrap().starts_with("parameters are not valid JSON"));
    assert_eq!(run(&["exec"]).exit, Exit::Usage);
    assert_eq!(run(&["exec", "count.add", "{}", "extra"]).exit, Exit::Usage);
}

#[test]
fn run_lines_stops_at_the_first_failure_unless_keep_going() {
    let script = "# a comment\n{\"command\":\"count.add\",\"params\":{\"by\":2}}\n\n{\"command\":\"count.fail\"}\n{\"command\":\"count.add\"}\n";
    let o = run_in(&mut cli(), &["run", "-"], script);
    assert_eq!(o.exit, Exit::Failed);
    let l = o.lines();
    assert_eq!(l.len(), 2);
    assert_eq!(l[0], json!({"ok": true, "result": 2}));
    assert_eq!(l[1]["ok"], false);
    let o = run_in(&mut cli(), &["run", "-", "--keep-going"], script);
    assert_eq!(o.exit, Exit::Failed);
    assert_eq!(o.lines().len(), 3);
    assert_eq!(o.lines()[2], json!({"ok": true, "result": 3}));
    let o = run_in(&mut cli(), &["run", "-"], "not json\n");
    assert_eq!(o.exit, Exit::Failed);
    assert!(o.reply()["error"].as_str().unwrap().starts_with("line 1: bad JSON"));
    let o = run_in(&mut cli(), &["run", "-"], "{\"params\":{}}\n");
    assert!(o.reply()["error"].as_str().unwrap().starts_with("line 1: expected"));
    // From a file; all ok.
    let file = tempfile::Builder::new().suffix(".jsonl").tempfile().unwrap();
    let path = file.path();
    std::fs::write(path, "{\"command\":\"count.add\"}\n{\"command\":\"count.add\"}\n").unwrap();
    let o = run(&["run", path.to_str().unwrap()]);
    assert_eq!((o.exit, o.lines().len()), (Exit::Ok, 2));
    assert_eq!(run(&["run", "/nonexistent/x.jsonl"]).exit, Exit::Usage);
}

#[test]
fn unknown_verbs_help_and_no_arguments() {
    let o = run(&[]);
    assert_eq!(o.exit, Exit::Usage);
    assert!(o.err.contains("counter-cli exec <id>"), "{}", o.err);
    let o = run(&["frobnicate"]);
    assert_eq!(o.exit, Exit::Usage);
    assert!(o.err.contains("unknown command `frobnicate`"));
    let o = run(&["help"]);
    assert_eq!(o.exit, Exit::Ok);
    assert!(o.out.contains("counter-cli mcp"));
}

#[test]
fn app_verbs_extend_and_override() {
    let mut c = cli()
        .verb("stats", "stats [--deep]", "app statistics", |mut a: Args, io: &mut Io<'_>| {
            let deep = a.flag("--deep");
            if let Err(e) = a.finish() {
                return io.usage(e, "stats [--deep]");
            }
            io.reply(&ashlar_control::Reply::ok(json!({"deep": deep})))
        })
        .verb("version", "version", "custom version", |_, io: &mut Io<'_>| {
            let _ = writeln!(io.out, "custom");
            Exit::Ok
        });
    assert_eq!(run_in(&mut c, &["stats", "--deep"], "").reply(), json!({"ok": true, "result": {"deep": true}}));
    assert_eq!(run_in(&mut c, &["stats", "--nope"], "").exit, Exit::Usage);
    assert_eq!(run_in(&mut c, &["version"], "").out, "custom\n");
    let help = run_in(&mut c, &["help"], "").out;
    assert!(help.contains("app statistics") && help.contains("custom version"));
    assert!(!help.contains("print the version"), "the standard version line is replaced: {help}");
}

#[test]
fn mcp_on_stdio_writes_only_protocol() {
    let input = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "execute", "arguments": {"command": "count.add", "params": {"by": 5}}}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let o = run_in(&mut cli(), &["mcp"], &input);
    assert_eq!(o.exit, Exit::Ok);
    let l = o.lines();
    assert_eq!(l.len(), 2);
    assert_eq!(l[0]["result"]["protocolVersion"], "2025-03-26");
    assert!(l[0]["result"]["instructions"].as_str().unwrap().starts_with("A counter."));
    assert_eq!(l[1]["result"]["content"][0]["text"], "5");
    // Per-command tools.
    let o = run_in(&mut cli(), &["mcp", "--per-command"], r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#);
    assert!(o.out.contains("\"count_add\""));
    assert_eq!(run(&["mcp", "--isolate", "--connect", "1"]).exit, Exit::Usage);
}

#[test]
fn serve_speaks_the_control_protocol_on_stdio() {
    let o = run_in(
        &mut cli(),
        &["serve"],
        "{\"id\":1,\"method\":\"engine.execute\",\"params\":{\"command\":\"count.add\"}}\n{\"id\":2,\"method\":\"app.version\"}\nGET / HTTP/1.1\n{\"id\":3,\"method\":\"app.ping\"}\n",
    );
    let l = o.lines();
    assert_eq!(l.len(), 3, "junk ends the session");
    assert_eq!(l[0], json!({"id": 1, "ok": true, "result": 1}));
    assert_eq!(l[1]["result"]["name"], "counter");
    assert_eq!(l[2]["ok"], false);
}

#[test]
fn connect_drives_the_running_app() {
    let keys = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| run_in(&mut cli().key_dir(keys.path()), args, "");
    let (server, rx) = Server::start(Config::new(0, keys.path()), || {}).unwrap();
    std::thread::spawn(move || {
        let mut c = Counter(100);
        let info = AppInfo::new("counter", "1.2.3");
        while let Ok(req) = rx.recv() {
            handle(req, &mut c, &mut NoUi, &info);
        }
    });
    let port = server.port().to_string();
    assert_eq!(run(&["exec", "count.add", "--connect", &port]).reply(), json!({"ok": true, "result": 101}));
    assert_eq!(run(&["exec", "--connect", &port, "count.add", r#"{"by":2}"#]).reply(), json!({"ok": true, "result": 103}));
    assert_eq!(run(&["describe", "count.fail", "--connect", &port]).reply()["result"]["id"], "count.fail");
    let port_num = server.port();
    drop(server);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let o = run(&["exec", "count.add", "--connect", &port_num.to_string()]);
    assert_eq!(o.exit, Exit::Failed);
    assert_eq!(o.reply()["ok"], false);
    assert_eq!(run(&["exec", "count.add", "--connect", "nope"]).exit, Exit::Usage);
    // A CLI without a key folder cannot connect.
    let mut bare = Cli::new("counter", "1", || Box::new(Counter::default()));
    assert_eq!(run_in(&mut bare, &["exec", "count.add", "--connect", "7"], "").exit, Exit::Failed);
}
