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

fn with_prompts() -> McpServer {
    let lib = Library::from_files([
        ("_category.md", ROOT),
        ("skills/_category.md", "---\ntitle: Guides\ndescription: Whole jobs\n---\nPick one.\n"),
        ("skills/cut.md", CUT),
        ("skills/deep/trim.md", "---\ntitle: Trim\ndescription: Read when trimming\n---\nTrim it. See [cut](../cut.md).\n"),
        ("reference/motion/eases.md", EASES),
    ]);
    server(lib).prompts("skills")
}

#[test]
fn a_category_is_served_as_prompts() {
    let mut s = with_prompts();
    let init = rpc(&mut s, 1, "initialize", json!({"protocolVersion": "2025-06-18"}));
    assert_eq!(init["result"]["capabilities"]["prompts"], json!({"listChanged": false}));
    let list = rpc(&mut s, 2, "prompts/list", json!({}));
    let prompts = list["result"]["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 2, "{list}");
    assert_eq!(
        (prompts[0]["name"].as_str(), prompts[0]["title"].as_str(), prompts[0]["description"].as_str()),
        (Some("cut"), Some("Cut"), Some("Read when cutting"))
    );
    assert_eq!(prompts[1]["name"], "deep/trim");
    assert_eq!(prompts[0]["arguments"], json!([{"name": "brief", "description": prompts[0]["arguments"][0]["description"], "required": false}]));

    let got = rpc(&mut s, 3, "prompts/get", json!({"name": "cut", "arguments": {"brief": "Cut the interview to 60 s"}}));
    let r = &got["result"];
    assert_eq!(r["description"], "Read when cutting");
    assert_eq!((r["messages"][0]["role"].as_str(), r["messages"][0]["content"]["type"].as_str()), (Some("user"), Some("text")));
    let text = r["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("(`skills/cut.md`)") && text.contains("# Cut\n\nSplit the clip."), "{text}");
    assert!(!text.contains("title: Cut"), "the frontmatter is left out: {text}");
    assert!(text.ends_with("## The brief\n\nCut the interview to 60 s\n"), "{text}");

    let nested = rpc(&mut s, 4, "prompts/get", json!({"name": "deep/trim"}));
    let text = nested["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("relative to `skills/deep/`") && text.contains("No brief was given"), "{text}");
}

#[test]
fn prompt_errors_say_what_would_work() {
    let mut s = with_prompts();
    let miss = rpc(&mut s, 1, "prompts/get", json!({"name": "cutt"}));
    assert_eq!(miss["error"]["code"], -32602);
    assert!(miss["error"]["message"].as_str().unwrap().contains("did you mean `cut`"), "{miss}");
    let far = rpc(&mut s, 2, "prompts/get", json!({"name": "zzzzzz"}));
    assert!(far["error"]["message"].as_str().unwrap().contains("cut, deep/trim"), "{far}");
    let arg = rpc(&mut s, 3, "prompts/get", json!({"name": "cut", "arguments": {"topic": "x"}}));
    assert!(arg["error"]["message"].as_str().unwrap().contains("unknown argument `topic`"), "{arg}");
    let typed = rpc(&mut s, 4, "prompts/get", json!({"name": "cut", "arguments": {"brief": 3}}));
    assert_eq!(typed["error"]["code"], -32602);
    assert_eq!(rpc(&mut s, 5, "prompts/get", json!({}))["error"]["code"], -32602);
    // no prompts registered, or an empty category: no capability and no methods
    let mut bare = server(library());
    assert!(rpc(&mut bare, 6, "initialize", json!({"protocolVersion": "2025-06-18"}))["result"]["capabilities"].get("prompts").is_none());
    assert_eq!(rpc(&mut bare, 7, "prompts/list", json!({}))["error"]["code"], -32601);
    let mut empty = server(library()).prompts("nothing-here");
    assert_eq!(rpc(&mut empty, 8, "prompts/get", json!({"name": "cut"}))["error"]["code"], -32601);
}

#[test]
fn newer_revisions_get_cache_hints_on_prompts() {
    let mut s = with_prompts();
    let meta = json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}});
    let list = rpc(&mut s, 1, "prompts/list", json!({"_meta": meta}));
    assert_eq!(list["result"]["resultType"], "complete");
    assert!(list["result"].get("ttlMs").is_some(), "{list}");
    let got = rpc(&mut s, 2, "prompts/get", json!({"_meta": meta, "name": "cut"}));
    assert_eq!(got["result"]["resultType"], "complete");
}

#[test]
fn the_users_pages_are_written_only_inside_their_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("agent-docs");
    let lib = Library::from_files([
        ("_category.md", ROOT),
        ("skills/cut.md", CUT),
        ("memory/_category.md", "---\ntitle: Memory\ndescription: The person's notes\n---\nRead these first.\n"),
    ]);
    let mut s = server(lib).user_docs(&dir);
    let tools = rpc(&mut s, 1, "tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    for t in ["save_doc", "append_doc", "delete_doc"] {
        assert!(names.contains(&t), "{names:?}");
    }
    // the memory category shows while it is empty
    let tree: Value = serde_json::from_str(text(&call(&mut s, "docs_categories", json!({})))).unwrap();
    assert!(tree["subcategories"].as_array().unwrap().iter().any(|c| c["path"] == "memory" && c["total"] == 0), "{tree}");
    assert_eq!(text(&call(&mut s, "list_docs", json!({"category": "memory"}))).trim(), "[]");

    let style = "---\ntitle: Editing style\ndescription: Read before cutting\ntags: [memory]\n---\nHard cuts.\n";
    let saved = call(&mut s, "save_doc", json!({"path": "memory/style.md", "content": style}));
    assert_ne!(saved["isError"], true, "{saved}");
    let r: Value = serde_json::from_str(text(&saved)).unwrap();
    assert_eq!((r["path"].as_str(), r["done"].as_str()), (Some("memory/style.md"), Some("created")));
    // listed, searchable and read at once
    let listed: Value = serde_json::from_str(text(&call(&mut s, "list_docs", json!({"category": "memory"})))).unwrap();
    assert_eq!((listed[0]["path"].as_str(), listed[0]["source"].as_str()), (Some("memory/style.md"), Some("yours")));
    assert!(text(&call(&mut s, "read_doc", json!({"path": "memory/style.md"}))).contains("updated: "));
    call(&mut s, "append_doc", json!({"path": "memory/style.md", "text": "No wipes."}));
    assert!(text(&call(&mut s, "read_doc", json!({"path": "memory/style.md"}))).ends_with("Hard cuts.\n\nNo wipes.\n"));

    for (args, why) in [
        (json!({"path": "../escape.md", "content": style}), "not allowed"),
        (json!({"path": "/tmp/x.md", "content": style}), "relative"),
        (json!({"path": "memory/x.txt", "content": style}), ".md"),
        (json!({"path": "memory/x.md", "content": "no frontmatter"}), "title"),
        (json!({"path": "skills/cut.md", "content": style}), "built-in"),
    ] {
        let r = call(&mut s, "save_doc", args.clone());
        assert!(r["isError"] == true && text(&r).contains(why), "{args}: {r}");
    }
    assert!(!tmp.path().join("escape.md").exists());
    let replaced = call(&mut s, "save_doc", json!({"path": "skills/cut.md", "content": style, "replace_builtin": true}));
    assert_ne!(replaced["isError"], true, "{replaced}");
    assert!(text(&call(&mut s, "read_doc", json!({"path": "skills/cut.md"}))).contains("Hard cuts."));
    call(&mut s, "delete_doc", json!({"path": "skills/cut.md"}));
    assert_eq!(text(&call(&mut s, "read_doc", json!({"path": "skills/cut.md"}))), CUT);
    assert_eq!(call(&mut s, "delete_doc", json!({"path": "skills/cut.md"}))["isError"], true);

    // without a folder there are no write tools
    let mut bare = server(library());
    let tools = rpc(&mut bare, 1, "tools/list", json!({}));
    assert!(!tools["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "save_doc"));
}
