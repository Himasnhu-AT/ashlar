#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Testing documentation and skills as MCP resources and tools.

use ashlar_command::{CommandError, CommandInfo, Dispatch};
use ashlar_mcp::{Backend, Doc, McpServer, ServerOptions};
use serde_json::{Value, json};

#[derive(Default)]
struct DummySession;

impl Dispatch for DummySession {
    fn commands(&self) -> Vec<CommandInfo> {
        vec![]
    }
    fn execute(&mut self, _id: &str, _params: Value) -> Result<Value, CommandError> {
        Ok(json!({}))
    }
}

fn server_with_docs() -> McpServer {
    let opts = ServerOptions::new("tester", "0.1.0").instructions("Test server.");
    let doc1 = Doc::new("guide.md", "User Guide", "The counter user guide.", "# Guide\n\nWelcome to counter.");
    let doc2 = Doc::new(
        "skills/launch-film.md",
        "Launch Film Skill",
        "Step by step guide for launch films.",
        "# Launch Film\n\nHow to create a launch film.",
    );
    McpServer::new(Backend::Headless(Box::new(DummySession)), opts).docs(vec![doc1, doc2])
}

fn rpc(server: &mut McpServer, req: Value) -> Value {
    let resp = server.handle_line(&req.to_string()).expect("a response line");
    serde_json::from_str(&resp).unwrap()
}

#[test]
fn docs_exposed_as_mcp_resources() {
    let mut s = server_with_docs();
    // Initialize
    let init = rpc(
        &mut s,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        }),
    );
    assert!(init["result"]["serverInfo"].is_object());

    // resources/list
    let res = rpc(&mut s, json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list"}));
    let list = res["result"]["resources"].as_array().unwrap();
    let uris: Vec<&str> = list.iter().filter_map(|r| r["uri"].as_str()).collect();
    assert!(uris.contains(&"docs://guide.md"));
    assert!(uris.contains(&"docs://skills/launch-film.md"));

    // Check annotations on skills doc (higher priority 0.9)
    let skill_res = list.iter().find(|r| r["uri"] == "docs://skills/launch-film.md").unwrap();
    assert_eq!(skill_res["annotations"]["priority"], 0.9);

    // resources/read with full URI
    let read1 = rpc(
        &mut s,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "resources/read",
            "params": {"uri": "docs://guide.md"}
        }),
    );
    assert_eq!(read1["result"]["contents"][0]["text"], "# Guide\n\nWelcome to counter.");

    // resources/read with relative path
    let read2 = rpc(
        &mut s,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "resources/read",
            "params": {"uri": "skills/launch-film.md"}
        }),
    );
    assert_eq!(read2["result"]["contents"][0]["text"], "# Launch Film\n\nHow to create a launch film.");
}

#[test]
fn docs_exposed_as_mcp_tools() {
    let mut s = server_with_docs();
    rpc(
        &mut s,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        }),
    );

    // tools/list includes list_docs and read_doc
    let t_list = rpc(&mut s, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let tools = t_list["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"list_docs"));
    assert!(names.contains(&"read_doc"));

    // tools/call list_docs without filter
    let c1 = rpc(
        &mut s,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "list_docs", "arguments": {}}
        }),
    );
    let matched: Value = serde_json::from_str(c1["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(matched.as_array().unwrap().len(), 2);

    // tools/call list_docs with section filter
    let c2 = rpc(
        &mut s,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {"name": "list_docs", "arguments": {"section": "skills"}}
        }),
    );
    let matched_skills: Value = serde_json::from_str(c2["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(matched_skills.as_array().unwrap().len(), 1);
    assert_eq!(matched_skills[0]["path"], "skills/launch-film.md");

    // tools/call read_doc
    let c3 = rpc(
        &mut s,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {"name": "read_doc", "arguments": {"path": "skills/launch-film.md"}}
        }),
    );
    assert_eq!(c3["result"]["content"][0]["text"], "# Launch Film\n\nHow to create a launch film.");

    // tools/call read_doc missing
    let c4 = rpc(
        &mut s,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {"name": "read_doc", "arguments": {"path": "nope.md"}}
        }),
    );
    assert_eq!(c4["result"]["isError"], true);
}
