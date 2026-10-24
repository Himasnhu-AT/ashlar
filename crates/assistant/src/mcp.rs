//! An app's MCP server (ashlar-mcp) as the assistant's tools, in process: the same tools,
//! schemas and command path an outside agent gets, without a transport. Text results are passed
//! on; images are described, not sent.

use ashlar_mcp::McpServer;
use serde_json::{Value, json};

use crate::agent::{ToolHost, ToolOutput};
use crate::provider::Cancel;
use crate::types::{ToolCall, ToolSpec};

type Filter = Box<dyn Fn(&str) -> bool>;
type ApprovalFn = Box<dyn FnMut(&ToolCall) -> Option<String>>;

/// The tools of an [`McpServer`].
pub struct McpTools {
    server: McpServer,
    next_id: u64,
    initialized: bool,
    keep: Filter,
    approval: ApprovalFn,
}

impl McpTools {
    pub fn new(server: McpServer) -> Self {
        Self { server, next_id: 1, initialized: false, keep: Box::new(|_| true), approval: Box::new(|_| None) }
    }

    /// Offer only the tools `keep` accepts (by name).
    pub fn only(mut self, keep: impl Fn(&str) -> bool + 'static) -> Self {
        self.keep = Box::new(keep);
        self
    }

    /// Which calls need the user's approval, and why.
    pub fn approval(mut self, f: impl FnMut(&ToolCall) -> Option<String> + 'static) -> Self {
        self.approval = Box::new(f);
        self
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        if !self.initialized {
            self.initialized = true;
            let init = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "assistant", "version": env!("CARGO_PKG_VERSION")}
            }});
            let _ = self.server.handle(init);
            let _ = self.server.handle(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let reply =
            self.server.handle(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).ok_or("the tool server gave no answer")?;
        if let Some(e) = reply.get("error") {
            return Err(e["message"].as_str().unwrap_or("the tool server refused the call").to_string());
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }
}

/// The text of an MCP tool result (`content` items).
pub fn result_text(result: &Value) -> String {
    let mut parts = Vec::new();
    for item in result["content"].as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("text") => parts.push(item["text"].as_str().unwrap_or_default().to_string()),
            Some("image") => parts.push(format!("[an image ({}) was returned; it is not shown here]", item["mimeType"].as_str().unwrap_or("image"))),
            Some("resource") => parts.push(item["resource"]["text"].as_str().unwrap_or("[a resource]").to_string()),
            Some(other) => parts.push(format!("[{other} content]")),
            None => {}
        }
    }
    if parts.is_empty()
        && let Some(s) = result.get("structuredContent")
    {
        parts.push(s.to_string());
    }
    parts.join("\n")
}

impl ToolHost for McpTools {
    fn tools(&mut self) -> Vec<ToolSpec> {
        let list = match self.request("tools/list", json!({})) {
            Ok(v) => v,
            Err(e) => {
                log::warn!(target: "assistant", "tools/list: {e}");
                return Vec::new();
            }
        };
        list["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                let name = t["name"].as_str()?;
                if !(self.keep)(name) {
                    return None;
                }
                let description = match (t["title"].as_str(), t["description"].as_str()) {
                    (Some(title), Some(d)) if !d.starts_with(title) => format!("{title}. {d}"),
                    (_, Some(d)) => d.to_string(),
                    (Some(title), None) => title.to_string(),
                    _ => String::new(),
                };
                let schema = if t["inputSchema"].is_object() { t["inputSchema"].clone() } else { json!({"type": "object"}) };
                Some(ToolSpec { name: name.to_string(), description, input_schema: schema, strict: false })
            })
            .collect()
    }

    fn needs_approval(&mut self, call: &ToolCall) -> Option<String> {
        (self.approval)(call)
    }

    fn call(&mut self, call: &ToolCall, cancel: &Cancel) -> ToolOutput {
        if cancel.is_cancelled() {
            return ToolOutput::error("The user stopped the assistant before this ran.");
        }
        match self.request("tools/call", json!({"name": call.name, "arguments": call.input})) {
            Ok(r) => ToolOutput { content: result_text(&r), is_error: r["isError"].as_bool().unwrap_or(false) },
            Err(e) => ToolOutput::error(e),
        }
    }
}
