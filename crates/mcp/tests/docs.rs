#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The documentation tools and resources over a library.

use ashlar_command::{CommandError, CommandInfo, Dispatch};
use ashlar_mcp::{Backend, Library, McpServer, ServerOptions};
use serde_json::{Value, json};

struct Empty;

impl Dispatch for Empty {
    fn commands(&self) -> Vec<CommandInfo> {
        vec![]
    }
    fn execute(&mut self, _id: &str, _params: Value) -> Result<Value, CommandError> {
        Ok(json!({}))
    }
}

const ROOT: &str = "---\ntitle: Tester docs\ndescription: All of it\n---\nThe library overview.\n";
const CUT: &str = "---\ntitle: Cut\ndescription: Read when cutting\ntags: [edit]\n---\n# Cut\n\nSplit the clip.\n";
const EASES: &str = "---\ntitle: Eases\ndescription: Curves\n---\n# Eases\n";

fn server(lib: Library) -> McpServer {
    McpServer::new(Backend::Headless(Box::new(Empty)), ServerOptions::new("tester", "0.1.0").instructions("Test server.")).docs(lib)
}

fn library() -> Library {
    Library::from_files([("_category.md", ROOT), ("skills/cut.md", CUT), ("reference/motion/eases.md", EASES)])
}

fn rpc(s: &mut McpServer, id: u64, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    serde_json::from_str(&s.handle_line(&line).expect("a reply")).unwrap()
}

fn call(s: &mut McpServer, name: &str, args: Value) -> Value {
    rpc(s, 9, "tools/call", json!({"name": name, "arguments": args}))["result"].clone()
}

fn text(r: &Value) -> &str {
    r["content"][0]["text"].as_str().unwrap()
}

#[test]
fn instructions_carry_the_overview_and_the_tools_are_listed() {
    let mut s = server(library());
    let init = rpc(&mut s, 1, "initialize", json!({"protocolVersion": "2025-06-18"}));
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(instructions.starts_with("Test server.\n\nThe library overview.\n\n"), "{instructions}");
    assert!(instructions.contains("`docs_categories`") && instructions.ends_with("Backend: headless session (no window)."), "{instructions}");
    let tools = rpc(&mut s, 2, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    for t in ["docs_categories", "list_docs", "read_doc"] {
        assert!(names.contains(&t), "{names:?}");
    }
    // no library: no tools, and the instructions are as before
    let mut bare = server(Library::new());
    let init = rpc(&mut bare, 1, "initialize", json!({"protocolVersion": "2025-06-18"}));
    assert_eq!(init["result"]["instructions"], "Test server. Backend: headless session (no window).");
    let tools = rpc(&mut bare, 2, "tools/list", json!({}));
    assert!(!tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "list_docs"));
}

#[test]
fn categories_listing_and_reading() {
    let mut s = server(library());
    let tree: Value = serde_json::from_str(text(&call(&mut s, "docs_categories", json!({})))).unwrap();
    assert_eq!((tree["title"].as_str(), tree["total"].as_u64()), (Some("Tester docs"), Some(2)));
    let sub: Value = serde_json::from_str(text(&call(&mut s, "docs_categories", json!({"category": "reference"})))).unwrap();
    assert_eq!(sub["subcategories"][0]["path"], "reference/motion");
    assert_eq!(call(&mut s, "docs_categories", json!({"category": "nope"}))["isError"], true);

    let all: Value = serde_json::from_str(text(&call(&mut s, "list_docs", json!({})))).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);
    let flat: Value = serde_json::from_str(text(&call(&mut s, "list_docs", json!({"category": "reference", "recursive": false})))).unwrap();
    assert_eq!(flat.as_array().unwrap().len(), 0);
    let edit: Value = serde_json::from_str(text(&call(&mut s, "list_docs", json!({"query": "edit"})))).unwrap();
    assert_eq!(
        (edit[0]["path"].as_str(), edit[0]["source"].as_str(), edit[0]["category"].as_str()),
        (Some("skills/cut.md"), Some("built-in"), Some("skills"))
    );
    assert_eq!(call(&mut s, "list_docs", json!({"category": "nope"}))["isError"], true);

    assert_eq!(text(&call(&mut s, "read_doc", json!({"path": "skills/cut.md"}))), CUT);
    let miss = call(&mut s, "read_doc", json!({"path": "cut.md"}));
    assert_eq!(miss["isError"], true);
    assert!(text(&miss).contains("`skills/cut.md`"), "{miss}");
}

#[test]
fn every_document_is_a_resource_read_verbatim() {
    let mut s = server(library());
    let list = rpc(&mut s, 1, "resources/list", json!({}));
    let uris: Vec<&str> = list["result"]["resources"].as_array().unwrap().iter().filter_map(|r| r["uri"].as_str()).collect();
    assert!(uris.contains(&"docs://skills/cut.md") && uris.contains(&"docs://reference/motion/eases.md"), "{uris:?}");
    let read = rpc(&mut s, 2, "resources/read", json!({"uri": "docs://reference/motion/eases.md"}));
    assert_eq!(read["result"]["contents"][0]["text"], EASES);
    assert_eq!(read["result"]["contents"][0]["mimeType"], "text/markdown");
    let missing = rpc(&mut s, 3, "resources/read", json!({"uri": "docs://nope.md"}));
    assert!(missing["error"].is_object(), "{missing}");
}
