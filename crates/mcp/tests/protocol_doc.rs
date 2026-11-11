#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `docs/control-protocol.md` lists every MCP tool with its arguments, exactly as the code defines
//! them: `name {required, optional?}`.

mod support;

use std::collections::BTreeMap;
use std::path::Path;

use ashlar_mcp::{Backend, Library, McpServer, ServerOptions, tools};
use serde_json::Value;
use support::Counter;

/// `name {a, b?}` for one tool definition (`?` when not required); `name` alone with no arguments.
fn signature(t: &Value) -> (String, Vec<String>) {
    let name = t["name"].as_str().unwrap().to_string();
    let required: Vec<&str> = t["inputSchema"]["required"].as_array().map(|r| r.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    // A schema with no properties takes any keys (`ui_set`): written `{…}`.
    let mut args: Vec<String> = t["inputSchema"]["properties"]
        .as_object()
        .map(|p| p.keys().map(|k| if required.contains(&k.as_str()) { k.clone() } else { format!("{k}?") }).collect())
        .unwrap_or_else(|| vec!["…".into()]);
    args.sort();
    (name, args)
}

/// Every tool the server can list: the fixed ones, every passthrough tool, the documentation tools.
fn from_code() -> BTreeMap<String, Vec<String>> {
    let dir = tempfile::tempdir().unwrap();
    let mut server = McpServer::new(Backend::Headless(Box::new(Counter::default())), ServerOptions::new("t", "0"))
        .docs(Library::from_files([("a.md", "---\ntitle: A\ndescription: a\n---\nA\n")]))
        .user_docs(dir.path());
    let r: Value = serde_json::from_str(&server.handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap()).unwrap();
    let listed = r["result"]["tools"].as_array().unwrap().clone();
    tools::core_tools().iter().chain(tools::ui_tools(&tools::all_ui_methods()).iter()).chain(listed.iter()).map(signature).collect()
}

/// The `name {args}` code spans in the first column of the "## MCP tools" table.
fn from_doc() -> BTreeMap<String, Vec<String>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/control-protocol.md");
    let text = std::fs::read_to_string(path).unwrap();
    let section = text.split("\n## MCP tools\n").nth(1).unwrap().split("\n## ").next().unwrap();
    let mut out = BTreeMap::new();
    for row in section.lines().filter(|l| l.starts_with("| `")) {
        let cell = row.split('|').nth(1).unwrap();
        for span in cell.split('`').skip(1).step_by(2) {
            let (name, args) = match span.split_once(' ') {
                Some((n, a)) => (n, a.trim().trim_start_matches('{').trim_end_matches('}')),
                None => (span, ""),
            };
            let mut args: Vec<String> = args.split(',').map(str::trim).filter(|a| !a.is_empty()).map(String::from).collect();
            args.sort();
            assert!(out.insert(name.to_string(), args).is_none(), "{name} is listed twice");
        }
    }
    out
}

#[test]
fn the_mcp_tools_table_matches_the_code() {
    let (code, doc) = (from_code(), from_doc());
    let show = |m: &BTreeMap<String, Vec<String>>| m.iter().map(|(n, a)| format!("{n} {{{}}}", a.join(", "))).collect::<Vec<_>>().join("\n");
    assert_eq!(doc, code, "docs/control-protocol.md \"## MCP tools\" should list exactly:\n{}", show(&code));
}
