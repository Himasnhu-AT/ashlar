#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! MCP conformance transcripts (golden JSON) and the three backends end to end.
//!
//! A transcript is one JSON object per line: `{"send": <message>}` or `{"send_raw": "<text>"}`,
//! and `"expect"`: the reply, or `null` when none is due. `UPDATE_GOLDEN=1 cargo test -p ashlar-mcp`
//! rewrites the expectations (review the diff).

mod support;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use ashlar_control::{AppInfo, Client, Config, Request, Server, UiHandler, handle};
use ashlar_mcp::{Backend, McpServer, ServerOptions, Subprocess, ToolMode};
use serde_json::{Value, json};
use support::Counter;

fn options() -> ServerOptions {
    ServerOptions::new("counter", "0.1.0").title("Counter").instructions("A counter app for tests.")
}

fn headless(mode: ToolMode) -> McpServer {
    McpServer::new(Backend::Headless(Box::new(Counter::default())), options().tools(mode))
}

/// Compare, treating strings that hold JSON as that JSON (tool text results), so key order and
/// formatting never matter.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) if x != y => match (serde_json::from_str::<Value>(x), serde_json::from_str::<Value>(y)) {
            (Ok(p), Ok(q)) => same(&p, &q),
            _ => false,
        },
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

fn golden(name: &str, mut server: McpServer) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(&path).unwrap();
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
        let mut step: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}:{}: {e}", n + 1));
        let sent = match (step.get("send"), step.get("send_raw")) {
            (Some(m), _) => m.to_string(),
            (None, Some(Value::String(raw))) => raw.clone(),
            _ => panic!("{name}:{}: no send", n + 1),
        };
        let got = server.handle_line(&sent).map(|r| serde_json::from_str::<Value>(&r).unwrap()).unwrap_or(Value::Null);
        if update {
            step["expect"] = got;
        } else {
            let want = step.get("expect").unwrap_or(&Value::Null);
            assert!(same(&got, want), "{name}:{}: sent {sent}\n   got {got}\n  want {want}", n + 1);
        }
        out.push(step.to_string());
    }
    if update {
        std::fs::write(&path, out.join("\n") + "\n").unwrap();
    }
}

#[test]
fn lifecycle() {
    golden("lifecycle", headless(ToolMode::Fixed));
}

#[test]
fn jsonrpc_errors() {
    golden("jsonrpc_errors", headless(ToolMode::Fixed));
}

#[test]
fn tools_headless() {
    golden("tools_headless", headless(ToolMode::Fixed));
}

#[test]
fn tools_per_command() {
    golden("tools_per_command", headless(ToolMode::PerCommand));
}

#[test]
fn serve_writes_only_protocol_and_survives_junk() {
    let mut server = headless(ToolMode::Fixed);
    let mut input = Vec::new();
    input.extend_from_slice(b"\xff\xfe\n");
    input.extend_from_slice(&vec![b'x'; ashlar_mcp::MAX_MESSAGE + 10]);
    input.extend_from_slice(b"\n\n");
    input.extend_from_slice(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    input.extend_from_slice(b"\n");
    input.extend_from_slice(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#);
    let mut out = Vec::new();
    server.serve(&input[..], &mut out).unwrap();
    let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert_eq!(lines[1]["error"]["code"], -32700);
    assert!(lines[1]["error"]["message"].as_str().unwrap().contains("longer than"));
    assert_eq!(lines[2], json!({"jsonrpc": "2.0", "id": 1, "result": {}}));
    assert!(server.is_initialized());
}

fn call(server: &mut McpServer, id: u64, tool: &str, args: Value) -> Value {
    let msg = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}});
    let r: Value = serde_json::from_str(&server.handle_line(&msg.to_string()).unwrap()).unwrap();
    r["result"].clone()
}

fn tool_names(server: &mut McpServer) -> Vec<String> {
    let r: Value = serde_json::from_str(&server.handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap()).unwrap();
    r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

/// A UI that answers screenshots and clicks.
struct FakeUi;

impl UiHandler for FakeUi {
    fn methods(&self) -> Vec<String> {
        vec!["ui.screenshot".into(), "ui.click".into(), "ui.inspect".into()]
    }
    fn handle(&mut self, req: Request) {
        match req.method() {
            "ui.screenshot" => req.ok(json!({"png": "iVBORw0KGgo=", "width": 2, "height": 1})),
            "ui.click" => {
                let p = req.params().clone();
                req.ok(json!({"clicked": p}));
            }
            "ui.inspect" => req.ok(json!({"elements": []})),
            _ => req.unknown(),
        }
    }
}

fn app_thread(rx: Receiver<Request>) {
    std::thread::spawn(move || {
        let mut c = Counter::default();
        let info = AppInfo::new("counter", "0.1.0");
        while let Ok(req) = rx.recv() {
            handle(req, &mut c, &mut FakeUi, &info);
        }
    });
}

#[test]
fn remote_backend_drives_a_running_app_with_ui() {
    let dir = std::env::temp_dir().join(format!("ashlar-mcp-remote-{}", std::process::id()));
    let (app, rx) = Server::start(Config::new(0, &dir), || {}).unwrap();
    app_thread(rx);
    let client = Client::connect(app.port(), &dir).unwrap();
    let mut server = McpServer::new(Backend::Remote(client), options());
    let names = tool_names(&mut server);
    assert_eq!(names, ["list_commands", "describe_command", "execute", "batch", "screenshot", "ui_inspect", "ui_click"]);
    let shot = call(&mut server, 2, "screenshot", json!({}));
    assert_eq!(shot["isError"], false);
    assert_eq!(shot["content"][0], json!({"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"}));
    let click = call(&mut server, 3, "ui_click", json!({"id": "toolbar.save"}));
    assert_eq!(click["content"][0]["text"], json!({"clicked": {"id": "toolbar.save"}}).to_string());
    let r = call(&mut server, 4, "execute", json!({"command": "count.add", "params": {"by": 2}}));
    assert_eq!((r["isError"].clone(), r["content"][0]["text"].clone()), (json!(false), json!("2")));
    // Not listed and not answered: the app says so.
    let r = call(&mut server, 5, "ui_drag", json!({"from": {"x": 0}, "to": {"x": 1}}));
    assert_eq!(r["isError"], true);
    let init: Value = serde_json::from_str(&server.handle_line(r#"{"jsonrpc":"2.0","id":9,"method":"initialize","params":{}}"#).unwrap()).unwrap();
    assert!(init["result"]["instructions"].as_str().unwrap().contains(&format!("127.0.0.1:{}", app.port())));
    // The app quits: tool calls fail softly.
    drop(app);
    std::thread::sleep(Duration::from_millis(100));
    let r = call(&mut server, 10, "execute", json!({"command": "count.get"}));
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"].as_str().unwrap().contains("not reachable"), "{r}");
}

fn worker() -> PathBuf {
    // tests run from target/<profile>/deps; cargo builds examples into target/<profile>/examples.
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    dir.join("examples").join(format!("stdio_worker{}", std::env::consts::EXE_SUFFIX))
}

#[test]
fn subprocess_backend_survives_a_crashing_command() {
    let w = worker();
    assert!(w.exists(), "{} (built by `cargo test`)", w.display());
    let mut server = McpServer::new(Backend::Subprocess(Subprocess::new(&w, Vec::<String>::new())), options());
    assert_eq!(tool_names(&mut server).len(), 4, "a worker has no UI");
    assert_eq!(call(&mut server, 2, "execute", json!({"command": "count.add", "params": {"by": 5}}))["content"][0]["text"], "5");
    assert_eq!(call(&mut server, 3, "execute", json!({"command": "count.get"}))["content"][0]["text"], "5");
    let crash = call(&mut server, 4, "execute", json!({"command": "count.crash"}));
    assert_eq!(crash["isError"], true);
    assert!(crash["content"][0]["text"].as_str().unwrap().contains("stopped without an answer"), "{crash}");
    // A fresh worker answers the next call.
    assert_eq!(call(&mut server, 5, "execute", json!({"command": "count.get"}))["content"][0]["text"], "0");
}

#[test]
fn subprocess_backend_times_out_a_hung_worker() {
    let sub = Subprocess::new(worker(), Vec::<String>::new()).with_timeout(Duration::from_millis(1500));
    let mut server = McpServer::new(Backend::Subprocess(sub), options());
    let r = call(&mut server, 1, "execute", json!({"command": "count.slow", "params": {"ms": 20000}}));
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"].as_str().unwrap().contains("did not finish"), "{r}");
    assert_eq!(call(&mut server, 2, "execute", json!({"command": "count.get"}))["content"][0]["text"], "0");
}

#[test]
fn subprocess_backend_reports_a_missing_program() {
    let mut server = McpServer::new(Backend::Subprocess(Subprocess::new("/nonexistent/worker", ["serve"])), options());
    let r = call(&mut server, 1, "execute", json!({"command": "count.get"}));
    assert_eq!(r["isError"], true);
    assert!(r["content"][0]["text"].as_str().unwrap().contains("could not start"), "{r}");
}
