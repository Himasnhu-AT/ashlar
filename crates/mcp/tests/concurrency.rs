#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! What `serve` does beyond one request at a time: `ping` and cancellations taken while a call
//! runs, progress for a stepwise batch, resources, tool hints and the app's own tools.

mod support;

use ashlar_mcp::{Access, Backend, McpServer, ServerOptions, ToolMode, ToolResult};
use serde_json::{Value, json};
use support::Counter;

fn server() -> McpServer {
    McpServer::new(Backend::Headless(Box::new(Counter::default())), ServerOptions::new("counter", "0.1.0").tools(ToolMode::Fixed))
}

/// Serve these messages (one per line) and return what was written, one value per line.
fn serve(server: &mut McpServer, messages: &[Value]) -> Vec<Value> {
    let input: String = messages.iter().map(|m| m.to_string() + "\n").collect();
    let mut out = Vec::new();
    server.serve(input.as_bytes(), &mut out).unwrap();
    String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn call(id: u64, tool: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}})
}

fn slow(id: u64, ms: u64) -> Value {
    call(id, "execute", json!({"command": "count.slow", "params": {"ms": ms}}))
}

fn text(r: &Value) -> Value {
    serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn a_ping_is_answered_while_a_call_runs() {
    let mut s = server();
    let out = serve(&mut s, &[slow(1, 400), json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})]);
    let ids: Vec<&Value> = out.iter().map(|m| &m["id"]).collect();
    assert_eq!(ids, [&json!(2), &json!(1)], "the ping is answered before the slow call ends: {out:?}");
    assert_eq!(out[0]["result"], json!({}));
    assert_eq!(out[1]["result"]["isError"], false);
}

#[test]
fn a_cancelled_request_gets_no_response_running_or_queued() {
    let mut s = server();
    let cancel = |id: u64| json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": id, "reason": "user"}});
    let out = serve(
        &mut s,
        &[
            slow(1, 300),
            cancel(1),
            call(2, "execute", json!({"command": "count.add"})),
            cancel(2),
            json!({"jsonrpc": "2.0", "id": 3, "method": "ping"}),
        ],
    );
    let ids: Vec<&Value> = out.iter().map(|m| &m["id"]).collect();
    assert_eq!(ids, [&json!(3)], "{out:?}");
    // the queued one never ran
    let out = serve(&mut s, &[call(4, "execute", json!({"command": "count.get"}))]);
    assert_eq!(text(&out[0]), json!(0));
}

#[test]
fn a_batch_with_a_progress_token_reports_each_step_and_stops_when_cancelled() {
    let mut s = server();
    let steps = json!([{"command": "count.add"}, {"command": "count.add", "params": {"by": 2}}, {"command": "count.get"}]);
    let mut msg = call(1, "batch", json!({"commands": steps}));
    msg["params"]["_meta"] = json!({"progressToken": "p1"});
    let out = serve(&mut s, &[msg]);
    let progress: Vec<(u64, u64)> = out
        .iter()
        .filter(|m| m["method"] == "notifications/progress")
        .inspect(|m| assert_eq!(m["params"]["progressToken"], "p1"))
        .map(|m| (m["params"]["progress"].as_u64().unwrap(), m["params"]["total"].as_u64().unwrap()))
        .collect();
    assert_eq!(progress, [(0, 3), (1, 3), (2, 3), (3, 3)]);
    let result = out.last().unwrap();
    assert_eq!(result["id"], 1);
    let steps = text(result);
    assert_eq!(steps.as_array().unwrap().iter().map(|s| s["result"].clone()).collect::<Vec<_>>(), [json!(1), json!(3), json!(3)]);

    // same results as engine.batch without a token, failures included
    let mut fail = call(2, "batch", json!({"commands": [{"command": "count.add"}, {"command": "count.fail"}, {"command": "count.add"}]}));
    let plain = serve(&mut s, std::slice::from_ref(&fail));
    fail["params"]["_meta"] = json!({"progressToken": 7});
    let stepwise: Vec<Value> = serve(&mut s, &[fail]).into_iter().filter(|m| m.get("id").is_some()).collect();
    assert_eq!(plain[0]["result"]["isError"], true);
    let first_error = |m: &Value| m["result"]["content"][0]["text"].as_str().unwrap().lines().next().unwrap().to_string();
    assert_eq!(first_error(&plain[0]), first_error(&stepwise[0]));

    // cancelled while its first step runs: the rest never runs
    let before = text(&serve(&mut s, &[call(3, "execute", json!({"command": "count.get"}))])[0]);
    let mut long =
        call(4, "batch", json!({"commands": [{"command": "count.slow", "params": {"ms": 300}}, {"command": "count.add", "params": {"by": 100}}]}));
    long["params"]["_meta"] = json!({"progressToken": "p2"});
    let out = serve(&mut s, &[long, json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 4}})]);
    assert!(out.iter().all(|m| m.get("id").is_none()), "no response for the cancelled batch: {out:?}");
    assert_eq!(text(&serve(&mut s, &[call(5, "execute", json!({"command": "count.get"}))])[0]), before);
}

#[test]
fn resources_list_and_read() {
    let mut s = server();
    let out = serve(
        &mut s,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": "app://commands"}}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "resources/read", "params": {"uri": "app://nope"}}),
        ],
    );
    let uris: Vec<&str> = out[0]["result"]["resources"].as_array().unwrap().iter().map(|r| r["uri"].as_str().unwrap()).collect();
    assert_eq!(uris, ["app://info", "app://commands"], "no UI resource for a headless backend");
    let c = &out[1]["result"]["contents"][0];
    assert_eq!(c["mimeType"], "application/json");
    let list: Value = serde_json::from_str(c["text"].as_str().unwrap()).unwrap();
    assert!(list.as_array().unwrap().iter().any(|c| c["id"] == "count.add"));
    assert_eq!(out[2]["error"]["code"], -32002);
}

#[test]
fn tools_carry_read_only_destructive_and_idempotent_hints() {
    let mut s = McpServer::new(Backend::Headless(Box::new(Counter::default())), ServerOptions::new("counter", "0.1.0").tools(ToolMode::PerCommand));
    let out = serve(&mut s, &[json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})]);
    let tools = out[0]["result"]["tools"].as_array().unwrap().clone();
    let hints = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("no {name}"))["annotations"].clone();
    assert_eq!((hints("list_commands")["readOnlyHint"].clone(), hints("list_commands")["idempotentHint"].clone()), (json!(true), json!(true)));
    assert_eq!((hints("execute")["readOnlyHint"].clone(), hints("execute")["destructiveHint"].clone()), (json!(false), json!(true)));
    // a command that changes the document is undoable: not destructive
    assert_eq!((hints("count_add")["readOnlyHint"].clone(), hints("count_add")["destructiveHint"].clone()), (json!(false), json!(false)));
    assert_eq!(hints("count_get")["readOnlyHint"], true);
}

#[test]
fn the_apps_own_tools_are_listed_and_called_and_cannot_shadow_the_fixed_ones() {
    let mut s = server()
        .tool("double", "Double", "Add the counter to itself.", json!({"type": "object"}), Access::Undoable, |_, backend| {
            let app = ashlar_control::AppInfo::new("counter", "0.1.0");
            let now = backend.call("engine.execute", json!({"command": "count.get"}), &app);
            let by = match now {
                ashlar_control::Reply::Ok(v) => v.as_i64().unwrap_or(0),
                ashlar_control::Reply::Err { error, .. } => return ToolResult::error(error),
            };
            ToolResult::from_reply(backend.call("engine.execute", json!({"command": "count.add", "params": {"by": by}}), &app))
        })
        .tool("execute", "Shadow", "Must not replace execute.", json!({"type": "object"}), Access::Read, |_, _| ToolResult::text("shadowed"));
    let out = serve(
        &mut s,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            call(2, "execute", json!({"command": "count.add", "params": {"by": 3}})),
            call(3, "double", json!({})),
        ],
    );
    let names: Vec<&str> = out[0]["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names.iter().filter(|n| **n == "execute").count(), 1);
    assert!(names.contains(&"double"));
    assert_eq!(text(&out[1]), json!(3), "execute is still the fixed tool");
    assert_eq!(text(&out[2]), json!(6));
}
