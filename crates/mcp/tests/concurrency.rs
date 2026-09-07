#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! What `serve` does beyond one request at a time: `ping` and cancellations taken while a call
//! runs, progress for a stepwise batch, resources, tool hints and the app's own tools.

mod support;

use ashlar_mcp::{Access, Backend, McpServer, PendingCall, ServerOptions, ToolCx, ToolMode, ToolOutcome, ToolResult};
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
        .tool("double", "Double", "Add the counter to itself.", json!({"type": "object"}), Access::Undoable, |_, cx| {
            let by = match cx.execute("count.get", json!({})) {
                ashlar_control::Reply::Ok(v) => v.as_i64().unwrap_or(0),
                ashlar_control::Reply::Err { error, .. } => return ToolResult::error(error),
            };
            ToolResult::from_reply(cx.execute("count.add", json!({"by": by})))
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

/// A call that finishes after `steps` polls, reporting progress; cancelling it adds 100 to the
/// counter so the test can see the clean-up ran.
struct Steps {
    left: u32,
    total: u32,
}

impl PendingCall for Steps {
    fn poll(&mut self, cx: &mut ToolCx<'_>) -> Option<ToolResult> {
        let done = self.total - self.left;
        cx.progress(f64::from(done), Some(f64::from(self.total)), "working");
        if self.left == 0 {
            return Some(ToolResult::text("finished"));
        }
        self.left -= 1;
        None
    }

    fn cancel(&mut self, cx: &mut ToolCx<'_>) {
        let _ = cx.execute("count.add", json!({"by": 100}));
    }
}

fn with_long_tool() -> McpServer {
    server().tool(
        "long",
        "Long",
        "Finishes after a few polls.",
        json!({"type": "object", "properties": {"steps": {"type": "integer"}}}),
        Access::Read,
        |a, _| {
            let n = a["steps"].as_u64().unwrap_or(3) as u32;
            ToolOutcome::pending(Steps { left: n, total: n })
        },
    )
}

#[test]
fn a_pending_tool_reports_progress_while_other_requests_are_answered() {
    let mut s = with_long_tool();
    let mut long = call(1, "long", json!({"steps": 4}));
    long["params"]["_meta"] = json!({"progressToken": "L"});
    let out = serve(&mut s, &[long, call(2, "execute", json!({"command": "count.add"}))]);
    let ids: Vec<&Value> = out.iter().filter_map(|m| m.get("id")).collect();
    assert_eq!(ids, [&json!(2), &json!(1)], "the quick call is answered while the long one runs: {out:?}");
    let progress: Vec<f64> =
        out.iter().filter(|m| m["method"] == "notifications/progress").map(|m| m["params"]["progress"].as_f64().unwrap()).collect();
    assert_eq!(progress, [0.0, 1.0, 2.0, 3.0, 4.0]);
    assert_eq!(out.last().unwrap()["result"]["content"][0]["text"], "finished");
}

#[test]
fn a_cancelled_pending_tool_is_cleaned_up_and_gets_no_response() {
    use std::io::Write;
    let mut s = with_long_tool();
    // the cancellation comes while the call runs (all at once, it would cancel it before it starts)
    let (reader, mut writer) = std::io::pipe().unwrap();
    let feed = std::thread::spawn(move || {
        writeln!(writer, "{}", call(1, "long", json!({"steps": 50}))).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        writeln!(writer, "{}", json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}})).unwrap();
        writeln!(writer, "{}", json!({"jsonrpc": "2.0", "id": 2, "method": "ping"})).unwrap();
    });
    let mut out = Vec::new();
    s.serve(std::io::BufReader::new(reader), &mut out).unwrap();
    feed.join().unwrap();
    let out: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(out.iter().all(|m| m["id"] != 1), "{out:?}");
    assert!(out.iter().any(|m| m["id"] == 2));
    assert_eq!(text(&serve(&mut s, &[call(3, "execute", json!({"command": "count.get"}))])[0]), json!(100), "cancel ran");
}

#[test]
fn handled_without_serve_a_pending_tool_finishes_in_place() {
    let mut s = with_long_tool();
    let r: Value = serde_json::from_str(&s.handle_line(&call(1, "long", json!({"steps": 2})).to_string()).unwrap()).unwrap();
    assert_eq!(r["result"]["content"][0]["text"], "finished");
}

#[test]
fn the_app_can_take_over_execute_for_some_commands() {
    let mut s = server().on_execute(|id, params, _| (id == "count.add" && params["by"] == 7).then(|| ToolResult::text("taken over").into()));
    let out = serve(
        &mut s,
        &[call(1, "execute", json!({"command": "count.add", "params": {"by": 7}})), call(2, "execute", json!({"command": "count.add"}))],
    );
    assert_eq!(out[0]["result"]["content"][0]["text"], "taken over");
    assert_eq!(text(&out[1]), json!(1), "other calls run as usual (and the hooked one did not)");
}

#[test]
fn app_resources_are_listed_and_read() {
    let mut s =
        server().resource("app://counter", "counter", "Counter", "The count.", "application/json", |cx| match cx.execute("count.get", json!({})) {
            ashlar_control::Reply::Ok(v) => Ok(json!({"count": v}).to_string()),
            ashlar_control::Reply::Err { error, .. } => Err(error),
        });
    let out = serve(
        &mut s,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "resources/read", "params": {"uri": "app://counter"}}),
        ],
    );
    assert!(out[0]["result"]["resources"].as_array().unwrap().iter().any(|r| r["uri"] == "app://counter"));
    assert_eq!(out[1]["result"]["contents"][0]["text"], json!({"count": 0}).to_string());
}

#[test]
fn strict_arguments_refuse_what_a_tool_does_not_take() {
    let mut s = McpServer::new(Backend::Headless(Box::new(Counter::default())), ServerOptions::new("counter", "0.1.0").strict_arguments(true));
    let out = serve(&mut s, &[call(1, "execute", json!({"command": "count.get", "parms": {}})), call(2, "execute", json!({"command": "count.get"}))]);
    assert_eq!(out[0]["error"]["code"], -32602, "{out:?}");
    assert!(out[0]["error"]["message"].as_str().unwrap().contains("`parms`"));
    assert_eq!(text(&out[1]), json!(0));
}
