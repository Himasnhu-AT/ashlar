#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The MCP debugger tools (`ui_snapshot`, `ui_fill`, …) against the demo app over a real control
//! port.

use ashlar_control::Client;
use ashlar_debug::demo;
use ashlar_mcp::{Backend, McpServer, ServerOptions};
use serde_json::{Value, json};

fn call(server: &mut McpServer, tool: &str, args: Value) -> Value {
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": tool, "arguments": args}});
    let r: Value = serde_json::from_str(&server.handle_line(&msg.to_string()).unwrap()).unwrap();
    r["result"].clone()
}

fn text(r: &Value, i: usize) -> String {
    r["content"][i]["text"].as_str().unwrap_or_default().to_string()
}

#[test]
fn debugger_tools_drive_the_app_by_refs() {
    let dir = std::env::temp_dir().join(format!("ashlar-debug-mcp-{}", std::process::id()));
    let app = demo::start(0, &dir).unwrap();
    let client = Client::connect(app.port, &dir).unwrap();
    let mut server = McpServer::new(Backend::Remote(client), ServerOptions::new("demo", "0.1.0"));
    let list: Value = serde_json::from_str(&server.handle_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap()).unwrap();
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    for t in [
        "ui_snapshot",
        "ui_hover",
        "ui_focus",
        "ui_fill",
        "ui_press",
        "ui_wait",
        "ui_get",
        "ui_scroll_into_view",
        "ui_highlight",
        "screenshot",
        "ui_click",
    ] {
        assert!(names.contains(&t), "{t} missing from {names:?}");
    }

    let r = call(&mut server, "ui_snapshot", json!({"interactive": true}));
    assert_eq!(r["isError"], false);
    assert!(text(&r, 0).starts_with("- textbox \"Title\" [ref=e2]\n- button \"Save\" [ref=e3]\n"), "{r}");
    assert!(text(&r, 0).ends_with("\n(snapshot 1)"), "{r}");
    assert_eq!(r["content"].as_array().unwrap().len(), 1, "no element JSON unless asked");
    let r = call(&mut server, "ui_snapshot", json!({"scope": "id=items", "elements": true}));
    let els: Value = serde_json::from_str(&text(&r, 1)).unwrap();
    assert_eq!(els["elements"].as_array().unwrap().len(), 4);

    assert_eq!(call(&mut server, "ui_fill", json!({"target": "@e2", "text": "Notes", "submit": true}))["isError"], false);
    let r = call(&mut server, "ui_get", json!({"target": {"id": "status"}, "what": "text"}));
    assert_eq!(text(&r, 0), r#"{"value":"Saved: Notes"}"#);
    assert_eq!(call(&mut server, "ui_click", json!({"target": "e6"}))["isError"], false);
    let r = call(&mut server, "ui_wait", json!({"for": "text", "text": "Loaded", "timeout_ms": 5000}));
    assert_eq!(r["isError"], false, "{r}");
    for (tool, args) in [
        ("ui_hover", json!({"target": "@e3"})),
        ("ui_focus", json!({"target": "@e2"})),
        ("ui_press", json!({"keys": "Mod+S"})),
        ("ui_scroll_into_view", json!({"target": "@e12"})),
        ("ui_highlight", json!({"target": "@e3", "ms": 100})),
    ] {
        let r = call(&mut server, tool, args);
        assert_eq!(r["isError"], false, "{tool}: {r}");
    }

    let r = call(&mut server, "screenshot", json!({"annotate": true, "highlight": "@e3"}));
    assert_eq!(r["content"][0]["type"], "image");
    let info: Value = serde_json::from_str(&text(&r, 1)).unwrap();
    assert_eq!(info["width"], 360);
    assert_eq!(info["annotations"][1], json!({"ref": "e3", "rect": [224.0, 16.0, 56.0, 24.0]}));
    let r = call(&mut server, "screenshot", json!({"target": "@e3"}));
    assert_eq!(serde_json::from_str::<Value>(&text(&r, 1)).unwrap(), json!({"width": 56, "height": 24}));

    // `raise: false` reaches the app (it never steals focus to paint).
    let r = call(&mut server, "screenshot", json!({"raise": false}));
    assert_eq!(r["isError"], false);
    let stats = Client::connect(app.port, &dir).unwrap().call("app.stats", json!({})).unwrap();
    assert_eq!(stats["screenshot_raise"], false);

    // Mistakes come back as tool errors, never protocol errors.
    let r = call(&mut server, "ui_click", json!({"target": "Save"}));
    assert_eq!(r["isError"], true);
    assert!(text(&r, 0).contains("not a target"));
    let r = call(&mut server, "ui_click", json!({"target": "@e99"}));
    assert_eq!(r["isError"], true);
    assert!(text(&r, 0).starts_with("stale_ref"), "{r}");
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
