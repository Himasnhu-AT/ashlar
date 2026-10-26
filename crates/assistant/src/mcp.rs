//! An app's MCP server (ashlar-mcp) as the assistant's tools, in process: the same tools,
//! schemas and command path an outside agent gets, without a transport. Text and image results are
//! passed on (images to models that take them; an image too large to send is described instead).

use ashlar_mcp::McpServer;
use serde_json::{Value, json};

use crate::agent::{ToolHost, ToolOutput};
use crate::provider::Cancel;
use crate::types::{Image, ToolCall, ToolSpec};

/// The largest image passed on, as base64 text (about 3.7 MB of image; the API takes 5 MB).
pub const MAX_IMAGE_BASE64: usize = 5_000_000;
const IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

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
            Some("image") => parts.push(match result_image(item) {
                Some(_) => "[image attached]".to_string(),
                None => format!(
                    "[an image ({}) was returned that is too large or of a kind that can't be shown]",
                    item["mimeType"].as_str().unwrap_or("image")
                ),
            }),
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

/// An MCP `image` content item as an [`Image`], when it can be sent (a known type, not too large).
fn result_image(item: &Value) -> Option<Image> {
    let media_type = item["mimeType"].as_str().filter(|t| IMAGE_TYPES.contains(t))?;
    let data = item["data"].as_str().filter(|d| !d.is_empty() && d.len() <= MAX_IMAGE_BASE64)?;
    Some(Image { media_type: media_type.to_string(), data: data.to_string() })
}

/// The images of an MCP tool result that can be sent.
pub fn result_images(result: &Value) -> Vec<Image> {
    result["content"].as_array().into_iter().flatten().filter(|i| i["type"] == "image").filter_map(result_image).collect()
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
            Ok(r) => ToolOutput { content: result_text(&r), is_error: r["isError"].as_bool().unwrap_or(false), images: result_images(&r) },
            Err(e) => ToolOutput::error(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_passed_on_when_they_can_be_sent() {
        let r = json!({"content": [
            {"type": "image", "data": "iVBOR", "mimeType": "image/png"},
            {"type": "image", "data": "x", "mimeType": "image/tiff"},
            {"type": "text", "text": "frame 1.0 s"}
        ]});
        let imgs = result_images(&r);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].media_type, "image/png");
        let t = result_text(&r);
        assert!(t.contains("[image attached]") && t.contains("can't be shown") && t.contains("frame 1.0 s"), "{t}");
    }
}
