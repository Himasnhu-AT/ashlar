#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! MCP 2026-07-28: no `initialize`; each request names its version and client capabilities in
//! `_meta`, `server/discover` describes the server, and list, read and call results carry
//! `resultType` (lists and reads also `ttlMs` and `cacheScope`). Sessions on an older revision
//! through `initialize` keep the old shapes.

mod support;

use ashlar_mcp::{Backend, McpServer, ServerOptions};
use serde_json::{Value, json};
use support::Counter;

fn server() -> McpServer {
    McpServer::new(Backend::Headless(Box::new(Counter::default())), ServerOptions::new("counter", "0.1.0").title("Counter").list_ttl_ms(60_000))
}

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

fn ask(s: &mut McpServer, id: u64, method: &str, mut params: Value) -> Value {
    params["_meta"] = params.get("_meta").cloned().unwrap_or_else(meta);
    let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
    serde_json::from_str(&s.handle_line(&line).unwrap()).unwrap()
}

#[test]
fn discover_describes_the_server_without_a_handshake() {
    let mut s = server();
    let r = ask(&mut s, 1, "server/discover", json!({}));
    let r = &r["result"];
    assert!(r["supportedVersions"].as_array().unwrap().contains(&json!("2026-07-28")), "{r}");
    assert_eq!((r["resultType"].as_str(), r["cacheScope"].as_str()), (Some("complete"), Some("private")));
    assert!(r["ttlMs"].is_u64() && r["capabilities"]["tools"].is_object() && r["instructions"].is_string(), "{r}");
    assert_eq!(r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "counter");
}

#[test]
fn modern_results_carry_the_result_type_and_cache_hints() {
    let mut s = server();
    for (id, method, params, cacheable) in [
        (1, "tools/list", json!({}), true),
        (2, "resources/list", json!({}), true),
        (3, "resources/read", json!({"uri": "app://commands"}), true),
        (4, "tools/call", json!({"name": "execute", "arguments": {"command": "count.get"}}), false),
    ] {
        let r = ask(&mut s, id, method, params);
        let r = &r["result"];
        assert_eq!(r["resultType"], "complete", "{method}: {r}");
        assert_eq!(r.get("ttlMs").is_some(), cacheable, "{method}: {r}");
        if cacheable {
            assert!(matches!(r["cacheScope"].as_str(), Some("public" | "private")), "{method}: {r}");
        }
    }
    assert_eq!(ask(&mut s, 5, "tools/list", json!({}))["result"]["ttlMs"], 60_000, "lists use the configured ttl");
    assert_eq!(ask(&mut s, 6, "resources/read", json!({"uri": "app://commands"}))["result"]["ttlMs"], 0, "reads are always fresh");
}

#[test]
fn modern_requests_need_their_metadata_and_a_supported_version() {
    let mut s = server();
    let mut m = meta();
    m.as_object_mut().unwrap().remove("io.modelcontextprotocol/clientCapabilities");
    let r = ask(&mut s, 1, "tools/list", json!({"_meta": m}));
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let mut m = meta();
    m["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
    let r = ask(&mut s, 2, "tools/list", json!({"_meta": m}));
    assert_eq!(r["error"]["code"], -32022, "{r}");
    assert_eq!(r["error"]["data"]["requested"], "2099-01-01");
    assert!(r["error"]["data"]["supported"].as_array().unwrap().contains(&json!("2026-07-28")));
    let r = ask(&mut s, 3, "server/discover", json!({"_meta": {}}));
    assert_eq!(r["error"]["code"], -32602, "discover needs the metadata too: {r}");
}

#[test]
fn handshake_sessions_keep_the_old_shapes_and_a_newer_client_falls_back() {
    let mut s = server();
    let init = |v: &str| {
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": v, "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}}).to_string()
    };
    let r: Value = serde_json::from_str(&s.handle_line(&init("2026-07-28")).unwrap()).unwrap();
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18", "the handshake answers a revision that has one");
    for (id, method, params) in
        [(2, "tools/list", json!({})), (3, "resources/list", json!({})), (4, "resources/read", json!({"uri": "app://commands"}))]
    {
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        let r: Value = serde_json::from_str(&s.handle_line(&line).unwrap()).unwrap();
        assert!(r["result"].is_object() && r["result"].get("resultType").is_none() && r["result"].get("ttlMs").is_none(), "{method}: {r}");
    }
}
