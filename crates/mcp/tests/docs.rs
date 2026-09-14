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

#[test]
fn markdown_reads_title_and_description_from_the_text() {
    let d = Doc::markdown("skills/cut.md", "# Cut a scene\n\nSplit the clip at the playhead.\n\nMore.");
    assert_eq!(
        (d.title.as_str(), d.description.as_str(), d.section.as_str(), d.uri.as_str()),
        ("Cut a scene", "Split the clip at the playhead.", "skills", "docs://skills/cut.md")
    );
    let fm = Doc::markdown("intro.md", "---\ntitle: Start here\ndescription: What is served\n---\n# Other\n");
    assert_eq!((fm.title.as_str(), fm.description.as_str(), fm.section.as_str()), ("Start here", "What is served", "general"));
}

#[test]
fn find_takes_an_exact_path_or_a_unique_name_and_lists_ambiguous_ones() {
    let docs = vec![
        Doc::markdown("a/skills/review.md", "# A"),
        Doc::markdown("b/skills/review.md", "# B"),
        Doc::markdown("b/skills/sound.md", "# S"),
        Doc::markdown("review.md", "# Top"),
    ];
    let find = |t: &str| ashlar_mcp::docs::find(&docs, t).map(|d| d.path.clone());
    assert_eq!(find("review.md").unwrap(), "review.md");
    assert_eq!(find("docs://a/skills/review.md").unwrap(), "a/skills/review.md");
    assert_eq!(find("sound").unwrap(), "b/skills/sound.md");
    assert_eq!(find("skills/sound.md").unwrap(), "b/skills/sound.md");
    let e = find("skills/review.md").unwrap_err();
    assert!(e.contains("a/skills/review.md") && e.contains("b/skills/review.md"), "{e}");
    assert!(find("nope").is_err());
}

#[test]
fn the_first_document_is_the_entry_point() {
    let mut s = server_with_docs();
    let init = rpc(&mut s, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}));
    let text = init["result"]["instructions"].as_str().unwrap();
    assert!(text.contains("`list_docs`") && text.contains("`guide.md`"), "{text}");
    let res = rpc(&mut s, json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list"}));
    let guide = res["result"]["resources"].as_array().unwrap().iter().find(|r| r["uri"] == "docs://guide.md").cloned().unwrap();
    assert_eq!(guide["annotations"]["priority"], 1.0);
}

#[cfg(unix)]
#[test]
fn a_docs_dir_skips_symbolic_links() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.md"), "# Secret").unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("page.md"), "# Page").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("linked")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.md"), dir.path().join("secret.md")).unwrap();
    let docs = ashlar_mcp::docs::load_docs_from_dir(dir.path());
    let paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths, ["page.md"]);
}
