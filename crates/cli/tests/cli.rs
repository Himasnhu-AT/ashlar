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
    assert!(o.err.contains("counter exec <id>"), "{}", o.err);
    let o = run(&["frobnicate"]);
    assert_eq!(o.exit, Exit::Usage);
    assert!(o.err.contains("unknown command `frobnicate`"));
    let o = run(&["help"]);
    assert_eq!(o.exit, Exit::Ok);
    assert!(o.out.contains("counter mcp"));
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

const ROOT: &str = "---\ntitle: Counter docs\ndescription: How to drive the counter\n---\nStart with the skills.\n";
const ADD: &str = "---\ntitle: Add skill\ndescription: Read when adding\n---\n# Add\n\nInstructions on adding.\n";
const GUIDE: &str = "---\ntitle: User guide\ndescription: Read first\n---\n# Guide\n";

fn library() -> ashlar_mcp::Library {
    ashlar_mcp::Library::from_files([("_category.md", ROOT), ("guide.md", GUIDE), ("skills/add.md", ADD), ("skills/more/deep.md", "# Deep\n")])
}

#[test]
fn docs_shows_the_tree_a_category_and_a_file_verbatim() {
    let mut c = cli().docs(library());
    let o = run_in(&mut c, &["docs"], "");
    assert_eq!(o.exit, Exit::Ok);
    assert!(o.out.starts_with("Counter docs: 3 documents\nHow to drive the counter\n"), "{}", o.out);
    assert!(o.out.contains("  skills/") && o.out.contains("Skills (2)") && o.out.contains("    skills/more/"), "{}", o.out);
    assert!(o.out.contains("guide.md  User guide"), "{}", o.out);

    let tree = run_in(&mut c, &["docs", "--json"], "").reply();
    assert_eq!(tree["result"]["total"], 3);
    assert_eq!(tree["result"]["subcategories"][0]["path"], "skills");
    assert_eq!(tree["result"]["subcategories"][0]["subcategories"][0]["docs"], 1);

    let cat = run_in(&mut c, &["docs", "skills"], "");
    assert!(cat.out.starts_with("Skills (skills/)\n"), "{}", cat.out);
    assert!(cat.out.contains("Subcategories:") && cat.out.contains("skills/add.md  Add skill"), "{}", cat.out);
    let cat = run_in(&mut c, &["docs", "skills", "--json"], "").reply();
    assert_eq!(cat["result"]["docs"][0]["path"], "skills/add.md");
    assert_eq!(cat["result"]["docs"][0]["source"], "built-in");

    let file = run_in(&mut c, &["docs", "skills/add.md"], "");
    assert_eq!((file.exit, file.out.as_str()), (Exit::Ok, ADD));
    let file = run_in(&mut c, &["docs", "skills/add.md", "--json"], "").reply();
    assert_eq!(file["result"]["text"], ADD);

    let q = run_in(&mut c, &["docs", "--query", "INSTRUCTIONS", "--json"], "").reply();
    assert_eq!(q["result"]["docs"].as_array().unwrap().len(), 1);

    // errors are plain text on stderr for a person, a reply line with --json
    let miss = run_in(&mut c, &["docs", "add.md"], "");
    assert_eq!((miss.exit, miss.out.as_str()), (Exit::Failed, ""));
    assert!(miss.err.starts_with("counter: no document `add.md`") && miss.err.contains("`skills/add.md`"), "{}", miss.err);
    let miss = run_in(&mut c, &["docs", "add.md", "--json"], "");
    assert!(miss.reply()["error"].as_str().unwrap().contains("`skills/add.md`"));
    let nope = run_in(&mut c, &["docs", "add"], "");
    assert_eq!((nope.exit, nope.out.as_str()), (Exit::Failed, ""));
    assert!(nope.err.contains("no category `add`; did you mean skills/add.md?"), "{}", nope.err);
    assert!(serde_json::from_str::<Value>(nope.err.trim()).is_err(), "not JSON: {}", nope.err);
    assert_eq!(run_in(&mut c, &["docs", "nope", "--json"], "").reply()["ok"], false);
    // no folder of the user's own
    let place = run_in(&mut c, &["docs", "--where"], "");
    assert_eq!((place.exit, place.out.as_str()), (Exit::Failed, ""));
    assert!(place.err.contains("no folder for your own documentation"), "{}", place.err);

    // the MCP server serves the same library
    let mcp_in = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "read_doc", "arguments": {"path": "skills/add.md"}}}),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "docs_categories", "arguments": {}}}),
    ]
    .map(|v| v.to_string())
    .join("\n")
        + "\n";
    let l = run_in(&mut c, &["mcp"], &mcp_in).lines();
    assert!(l[0]["result"]["instructions"].as_str().unwrap().contains("Start with the skills."));
    assert!(l[1]["result"]["resources"].as_array().unwrap().iter().any(|r| r["uri"] == "docs://skills/add.md"));
    assert_eq!(l[2]["result"]["content"][0]["text"], ADD);
    assert!(l[3]["result"]["content"][0]["text"].as_str().unwrap().contains("\"total\": 3"));
}

#[test]
fn the_users_folder_is_read_on_top_and_where_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let mine = tmp.path().join("agent-docs");
    let mut c = cli().docs(library()).user_docs(&mine);
    let o = run_in(&mut c, &["docs", "--where"], "");
    assert_eq!((o.exit, o.out.trim()), (Exit::Ok, mine.display().to_string().as_str()));
    assert_eq!(run_in(&mut c, &["docs", "--where", "--json"], "").reply()["result"]["exists"], false);

    std::fs::create_dir_all(mine.join("skills")).unwrap();
    std::fs::write(mine.join("skills/add.md"), "---\ntitle: My add\ndescription: Mine\n---\nmine\n").unwrap();
    std::fs::write(mine.join("notes.md"), "---\ntitle: Notes\ndescription: Mine\n---\n").unwrap();
    let file = run_in(&mut c, &["docs", "skills/add.md"], "");
    assert_eq!(file.out, "---\ntitle: My add\ndescription: Mine\n---\nmine\n");
    let cat = run_in(&mut c, &["docs", "skills"], "");
    assert!(cat.out.contains("skills/add.md  My add  [yours]"), "{}", cat.out);
    let all = run_in(&mut c, &["docs", "--query", "mine", "--json"], "").reply();
    let sources: Vec<&str> = all["result"]["docs"].as_array().unwrap().iter().map(|d| d["source"].as_str().unwrap()).collect();
    assert_eq!(sources, ["yours", "yours"]);
}

#[test]
fn a_docs_category_is_served_as_prompts() {
    let mut c = cli().docs(library()).prompts("skills");
    let mcp_in = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "prompts/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "prompts/get", "params": {"name": "add", "arguments": {"brief": "add three"}}}),
    ]
    .map(|v| v.to_string())
    .join("\n")
        + "\n";
    let l = run_in(&mut c, &["mcp"], &mcp_in).lines();
    assert_eq!(l[0]["result"]["capabilities"]["prompts"], json!({"listChanged": false}));
    let names: Vec<&str> = l[1]["result"]["prompts"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["add", "more/deep"]);
    let text = l[2]["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("Instructions on adding.") && text.ends_with("## The brief\n\nadd three\n"), "{text}");
    // without prompts registered there is no capability
    let l = run_in(&mut cli().docs(library()), &["mcp"], &(mcp_in.lines().next().unwrap().to_string() + "\n")).lines();
    assert!(l[0]["result"]["capabilities"].get("prompts").is_none());
}

#[test]
fn describe_names_the_closest_command() {
    let o = run(&["describe", "count.ad"]);
    assert_eq!(o.exit, Exit::Failed);
    assert!(o.reply()["error"].as_str().unwrap().contains("did you mean `count.add` {by?}"), "{}", o.out);
    let o = run(&["exec", "count.adx"]);
    let e = o.reply()["error"].as_str().unwrap().to_string();
    assert!(e.starts_with("unknown command `count.adx`; did you mean `count.add`"), "{e}");
}
