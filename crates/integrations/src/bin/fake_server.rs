//! A tiny MCP server over stdio for tests (feature `fake-server`).
//!
//! Tools: `echo` (read-only, returns its `text`), `env` (returns the variable `name`), `slow` (sleeps
//! `ms`, then answers), `crash` (exits at once), `write` (not read-only), `secret` (hidden by tests).
//! Prompts: `summarise` (argument `topic`). Resources: `skill://mixing` (a skill) and
//! `file:///notes.txt`.
//! `--fail-init` answers `initialize` with an error; `--bad-version` with an unknown protocol version;
//! `--silent` never answers. Each start appends a line to the file named by `FAKE_STARTS`, when set.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |f: &str| args.iter().any(|a| a == f);
    if let Ok(path) = std::env::var("FAKE_STARTS")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(f, "start");
    }
    eprintln!("fake server: started with {args:?}");
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue }; // notifications
        if has("--silent") {
            continue;
        }
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let reply = match method {
            "initialize" if has("--fail-init") => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": "init refused"}}),
            "initialize" => {
                let version = if has("--bad-version") { "1999-01-01" } else { "2025-06-18" };
                json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": version, "capabilities": {"tools": {}, "prompts": {}, "resources": {}},
                    "serverInfo": {"name": "fake", "version": "1.0"}, "instructions": "a fake"}})
            }
            "tools/list" => {
                // Two pages, to exercise the cursor.
                let first = params.get("cursor").is_none();
                let tools = if first {
                    json!([
                        {"name": "echo", "description": "Echo text", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
                         "annotations": {"readOnlyHint": true}},
                        {"name": "env", "description": "An env var", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}},
                        {"name": "slow", "description": "Sleeps", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}}
                    ])
                } else {
                    json!([
                        {"name": "crash", "description": "Exits", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}},
                        {"name": "write", "description": "Writes a file", "inputSchema": {"type": "object"}},
                        {"name": "secret", "description": "Hidden", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true, "openWorldHint": true}}
                    ])
                };
                let mut result = json!({"tools": tools});
                if first {
                    result["nextCursor"] = json!("page2");
                }
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
                let a = params.get("arguments").cloned().unwrap_or(Value::Null);
                let text = match name {
                    "echo" => a.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
                    "env" => std::env::var(a.get("name").and_then(Value::as_str).unwrap_or_default()).unwrap_or_else(|_| "<unset>".into()),
                    "slow" => {
                        std::thread::sleep(Duration::from_millis(a.get("ms").and_then(Value::as_u64).unwrap_or(1000)));
                        "slept".into()
                    }
                    "crash" => std::process::exit(3),
                    "write" => "wrote".into(),
                    _ => {
                        let reply =
                            json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "unknown tool"}], "isError": true}});
                        let _ = writeln!(out, "{reply}");
                        let _ = out.flush();
                        continue;
                    }
                };
                json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}], "isError": false}})
            }
            "prompts/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"prompts": [
                {"name": "summarise", "description": "Summarise a topic", "arguments": [{"name": "topic", "required": true}]}
            ]}}),
            "prompts/get" => {
                let topic = params.get("arguments").and_then(|a| a.get("topic")).and_then(Value::as_str).unwrap_or_default();
                json!({"jsonrpc": "2.0", "id": id, "result": {"messages": [{"role": "user", "content": {"type": "text", "text": format!("Summarise {topic} in three lines.")}}]}})
            }
            "resources/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"resources": [
                {"uri": "skill://mixing", "name": "Mixing", "description": "Use when balancing levels.", "mimeType": "text/markdown"},
                {"uri": "file:///notes.txt", "name": "Notes"}
            ]}}),
            "resources/read" => {
                let uri = params.get("uri").and_then(Value::as_str).unwrap_or_default();
                json!({"jsonrpc": "2.0", "id": id, "result": {"contents": [{"uri": uri, "mimeType": "text/markdown", "text": format!("# {uri}\nKeep peaks under -1 dB.")}]}})
            }
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            other => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("no {other}")}}),
        };
        // A ping of our own before answering, which the client must answer.
        if method == "tools/call" {
            let _ = writeln!(out, "{}", json!({"jsonrpc": "2.0", "id": "srv-1", "method": "ping"}));
        }
        let _ = writeln!(out, "{reply}");
        let _ = out.flush();
    }
}
