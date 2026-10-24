//! An MCP server as the tool host: the agent runs commands through it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use ashlar_assistant::mcp::McpTools;
use ashlar_assistant::*;
use ashlar_command::{CommandError, CommandInfo, Dispatch};
use ashlar_mcp::{Backend, McpServer, ServerOptions};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct App(Arc<Mutex<Vec<String>>>);

impl Dispatch for App {
    fn commands(&self) -> Vec<CommandInfo> {
        ["title.add", "app.quit"]
            .iter()
            .map(|id| CommandInfo {
                id: (*id).into(),
                label: (*id).into(),
                menu: vec![],
                shortcut: None,
                params_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}}),
                params_text: None,
                enabled: true,
                disabled_reason: None,
                mutates: true,
            })
            .collect()
    }

    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        self.0.lock().unwrap().push(format!("{id} {params}"));
        Ok(json!({"ran": id}))
    }
}

#[test]
fn the_agent_runs_commands_through_mcp() {
    let app = App::default();
    let server = McpServer::new(Backend::Headless(Box::new(app.clone())), ServerOptions::new("app", "1"));
    let mut tools =
        McpTools::new(server).only(|n| !n.starts_with("ui_")).approval(|c| (c.input["command"] == "app.quit").then(|| "Quits".to_string()));
    let specs = tools.tools();
    let names: Vec<_> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"execute") && names.contains(&"list_commands"), "{names:?}");

    let mut p = MockProvider::new([
        MockReply::tools(&[
            ("a", "list_commands", json!({})),
            ("b", "execute", json!({"command": "title.add", "params": {"text": "Hello"}})),
            ("c", "execute", json!({"command": "app.quit"})),
        ]),
        MockReply::text("Added the title."),
    ]);
    let mut c = Conversation::new();
    c.push_user_text("add a title");
    let o = run_turn(&mut p, &mut tools, &mut c, &AgentConfig::new("m", "s"), &mut DenyAll, &Cancel::new());
    assert_eq!(o.end, TurnEnd::Done);
    let results: Vec<_> = c.messages()[2].tool_results_iter().cloned().collect();
    assert!(results[0].content.contains("title.add"), "{}", results[0].content);
    assert!(!results[1].is_error, "{}", results[1].content);
    assert!(results[2].is_error, "quit was declined");
    assert_eq!(*app.0.lock().unwrap(), ["title.add {\"text\":\"Hello\"}"]);
}
