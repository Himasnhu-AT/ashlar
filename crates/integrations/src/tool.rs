//! Tools as an MCP server lists them, results as it returns them, and the names an assistant sees.

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The longest tool name model providers accept.
pub const MAX_TOOL_NAME: usize = 64;
/// Between an integration's id and its tool's name in the name the assistant sees.
pub const SEPARATOR: &str = "__";
/// The most text kept from one tool result.
pub const MAX_RESULT_TEXT: usize = 200_000;

/// One tool of a server (`tools/list`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tool {
    /// The server's own name for it.
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    /// JSON Schema of the arguments (always an object schema).
    pub input_schema: Value,
    /// The server says it changes nothing (`annotations.readOnlyHint`).
    pub read_only: bool,
    /// The server says it may delete or overwrite (`annotations.destructiveHint`, true when absent and not read-only).
    pub destructive: bool,
    /// The server says it reaches outside the machine (`annotations.openWorldHint`).
    pub open_world: bool,
}

impl Tool {
    /// One entry of `tools/list`; `None` when it has no usable name.
    pub fn from_mcp(v: &Value) -> Option<Tool> {
        let name = v.get("name").and_then(Value::as_str)?.trim();
        if name.is_empty() || name.len() > 128 {
            return None;
        }
        let ann = v.get("annotations");
        let hint = |k: &str| ann.and_then(|a| a.get(k)).and_then(Value::as_bool);
        let read_only = hint("readOnlyHint").unwrap_or(false);
        let mut schema = v.get("inputSchema").cloned().filter(Value::is_object).unwrap_or_else(|| json!({"type": "object"}));
        if let Some(obj) = schema.as_object_mut() {
            obj.entry("type").or_insert_with(|| json!("object"));
        }
        let text = |k: &str, max: usize| v.get(k).and_then(Value::as_str).map(|s| s.chars().take(max).collect::<String>());
        Some(Tool {
            name: name.to_string(),
            title: text("title", 200).or_else(|| ann.and_then(|a| a.get("title")).and_then(Value::as_str).map(str::to_string)),
            description: text("description", 10_000).unwrap_or_default(),
            input_schema: schema,
            read_only,
            destructive: hint("destructiveHint").unwrap_or(!read_only),
            open_world: hint("openWorldHint").unwrap_or(false),
        })
    }
}

/// One argument of a prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PromptArg {
    pub name: String,
    pub description: String,
    pub required: bool,
}

/// One prompt of a server (`prompts/list`): a saved request the person can run as a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prompt {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub arguments: Vec<PromptArg>,
}

/// The most arguments read for one prompt.
const MAX_PROMPT_ARGS: usize = 16;

impl Prompt {
    /// One entry of `prompts/list`; `None` without a usable name.
    pub fn from_mcp(v: &Value) -> Option<Prompt> {
        let name = v.get("name").and_then(Value::as_str)?.trim();
        if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
            return None;
        }
        let text = |v: &Value, k: &str, max: usize| v.get(k).and_then(Value::as_str).map(|s| s.chars().take(max).collect::<String>());
        let arguments = v
            .get("arguments")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .take(MAX_PROMPT_ARGS)
                    .filter_map(|x| {
                        let n = text(x, "name", 64).filter(|n| !n.trim().is_empty())?;
                        Some(PromptArg {
                            name: n,
                            description: text(x, "description", 500).unwrap_or_default(),
                            required: x.get("required").and_then(Value::as_bool).unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Prompt {
            name: name.to_string(),
            title: text(v, "title", 200),
            description: text(v, "description", 2_000).unwrap_or_default(),
            arguments,
        })
    }

    /// The arguments as `prompts/get` takes them from what the person typed after the command:
    /// one argument takes all of it; several take one word each in order, the last the rest.
    pub fn arguments_from(&self, typed: &str) -> serde_json::Map<String, Value> {
        let mut out = serde_json::Map::new();
        let typed = typed.trim();
        match self.arguments.len() {
            0 => {}
            1 => {
                if let Some(a) = self.arguments.first()
                    && !typed.is_empty()
                {
                    out.insert(a.name.clone(), json!(typed));
                }
            }
            n => {
                let mut rest = typed;
                for (i, a) in self.arguments.iter().enumerate() {
                    let rest_trim = rest.trim_start();
                    if rest_trim.is_empty() {
                        break;
                    }
                    let value = if i + 1 == n {
                        rest = "";
                        rest_trim
                    } else {
                        let end = rest_trim.find(char::is_whitespace).unwrap_or(rest_trim.len());
                        let (w, r) = rest_trim.split_at(end);
                        rest = r;
                        w
                    };
                    out.insert(a.name.clone(), json!(value));
                }
            }
        }
        out
    }

    /// `[a] [b]` for a command list.
    pub fn args_hint(&self) -> Option<String> {
        (!self.arguments.is_empty()).then(|| {
            self.arguments.iter().map(|a| if a.required { format!("<{}>", a.name) } else { format!("[{}]", a.name) }).collect::<Vec<_>>().join(" ")
        })
    }
}

/// The text of a `prompts/get` result: each message's text, the assistant's marked as such.
pub fn prompt_text(result: &Value) -> Option<String> {
    let messages = result.get("messages").and_then(Value::as_array)?;
    let mut parts: Vec<String> = Vec::new();
    for m in messages.iter().take(50) {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
        let c = m.get("content");
        let text = match c {
            Some(Value::Array(blocks)) => blocks.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
            Some(b) => b
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| b.get("resource").and_then(|r| r.get("text")).and_then(Value::as_str))
                .unwrap_or_default()
                .to_string(),
            None => String::new(),
        };
        if text.trim().is_empty() {
            continue;
        }
        parts.push(if role == "assistant" { format!("(assistant) {text}") } else { text });
    }
    let all = parts.join("\n\n");
    (!all.trim().is_empty()).then(|| all.chars().take(MAX_RESULT_TEXT).collect())
}

/// One resource of a server (`resources/list`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Resource {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: Option<String>,
}

/// Resources whose URI starts with this are skills (`skill://<name>`).
pub const SKILL_SCHEME: &str = "skill://";

impl Resource {
    pub fn from_mcp(v: &Value) -> Option<Resource> {
        let uri = v.get("uri").and_then(Value::as_str)?.trim();
        if uri.is_empty() || uri.len() > 1_000 {
            return None;
        }
        let text = |k: &str, max: usize| v.get(k).and_then(Value::as_str).map(|s| s.chars().take(max).collect::<String>());
        Some(Resource {
            uri: uri.to_string(),
            name: text("name", 200).unwrap_or_default(),
            description: text("description", 2_000).unwrap_or_default(),
            mime_type: text("mimeType", 100),
        })
    }

    /// The skill's name when this resource is one (`skill://<name>`).
    pub fn skill_name(&self) -> Option<&str> {
        self.uri.strip_prefix(SKILL_SCHEME).map(|n| n.trim_matches('/')).filter(|n| !n.is_empty() && !n.contains('/'))
    }
}

/// The text of a `resources/read` result (its text contents joined).
pub fn resource_text(result: &Value) -> Option<String> {
    let contents = result.get("contents").and_then(Value::as_array)?;
    let all: Vec<&str> = contents.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect();
    (!all.is_empty()).then(|| all.join("\n\n").chars().take(MAX_RESULT_TEXT).collect())
}

/// One block of a tool result.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Content {
    Text {
        text: String,
    },
    /// Base64 data.
    Image {
        mime_type: String,
        data: String,
    },
    /// Audio, resources and anything newer, as sent.
    Other {
        value: Value,
    },
}

/// What a tool call returned.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CallOutput {
    pub content: Vec<Content>,
    /// The tool failed (the text says why).
    pub is_error: bool,
}

impl CallOutput {
    /// A failure with this message.
    pub fn error(message: impl Into<String>) -> CallOutput {
        CallOutput { content: vec![Content::Text { text: message.into() }], is_error: true }
    }

    /// The `result` of `tools/call`; `None` without a content array.
    pub fn from_mcp(result: &Value) -> Option<CallOutput> {
        let blocks = result.get("content").and_then(Value::as_array)?;
        let content = blocks
            .iter()
            .map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => Content::Text { text: b.get("text").and_then(Value::as_str).unwrap_or_default().to_string() },
                Some("image") => Content::Image {
                    mime_type: b.get("mimeType").and_then(Value::as_str).unwrap_or("image/png").to_string(),
                    data: b.get("data").and_then(Value::as_str).unwrap_or_default().to_string(),
                },
                _ => Content::Other { value: b.clone() },
            })
            .collect();
        Some(CallOutput { content, is_error: result.get("isError").and_then(Value::as_bool).unwrap_or(false) })
    }

    /// The result as text for a model: the text blocks, a line for each image or other block, capped at
    /// [`MAX_RESULT_TEXT`] characters.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for c in &self.content {
            if !out.is_empty() {
                out.push('\n');
            }
            match c {
                Content::Text { text } => out.push_str(text),
                Content::Image { mime_type, data } => out.push_str(&format!("[image {mime_type}, {} KB]", data.len() * 3 / 4 / 1024)),
                Content::Other { value } => {
                    let kind = value.get("type").and_then(Value::as_str).unwrap_or("content");
                    out.push_str(&format!("[{kind}]"));
                }
            }
        }
        if out.chars().count() > MAX_RESULT_TEXT {
            out = out.chars().take(MAX_RESULT_TEXT).collect();
            out.push_str("\n[truncated]");
        }
        out
    }
}

/// The name an assistant sees for `tool` of integration `id`: `<id>__<tool>`, only `[A-Za-z0-9_-]`, at
/// most [`MAX_TOOL_NAME`] characters (a longer one is cut and ends with a hash of the full name, so
/// names stay unique).
pub fn qualified_name(id: &str, tool: &str) -> String {
    let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect::<String>();
    let full = format!("{}{SEPARATOR}{}", clean(id), clean(tool));
    if full.len() <= MAX_TOOL_NAME {
        return full;
    }
    let digest = Sha256::digest(format!("{id}\u{0}{tool}").as_bytes());
    let hash: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{}_{hash}", &full[..MAX_TOOL_NAME - hash.len() - 1])
}

/// The integration id a qualified name starts with, if it has the separator.
pub fn integration_of(qualified: &str) -> Option<&str> {
    qualified.split_once(SEPARATOR).map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_and_resources_from_mcp() {
        let p = Prompt::from_mcp(&json!({"name": "transcribe", "description": "Transcribe a file", "arguments": [{"name": "file", "required": true}, {"name": "language"}]})).unwrap();
        assert_eq!(p.args_hint().as_deref(), Some("<file> [language]"));
        let a = p.arguments_from("/a/b.wav  en gb");
        assert_eq!(a.get("file"), Some(&json!("/a/b.wav")));
        assert_eq!(a.get("language"), Some(&json!("en gb")));
        let one = Prompt::from_mcp(&json!({"name": "ask", "arguments": [{"name": "q"}]})).unwrap();
        assert_eq!(one.arguments_from(" what is this "), serde_json::Map::from_iter([("q".to_string(), json!("what is this"))]));
        assert!(Prompt::from_mcp(&json!({"name": "bad name"})).is_none());
        let text = prompt_text(
            &json!({"messages": [{"role": "user", "content": {"type": "text", "text": "Listen to it"}}, {"role": "assistant", "content": [{"type": "text", "text": "ok"}]}]}),
        );
        assert_eq!(text.as_deref(), Some("Listen to it\n\n(assistant) ok"));
        let r =
            Resource::from_mcp(&json!({"uri": "skill://mixing", "name": "Mixing", "description": "Levels", "mimeType": "text/markdown"})).unwrap();
        assert_eq!(r.skill_name(), Some("mixing"));
        assert_eq!(Resource::from_mcp(&json!({"uri": "file:///x"})).and_then(|r| r.skill_name().map(str::to_string)), None);
        assert_eq!(resource_text(&json!({"contents": [{"uri": "skill://mixing", "text": "# Mixing"}]})).as_deref(), Some("# Mixing"));
    }

    #[test]
    fn tools_parse_with_their_hints() {
        let t = Tool::from_mcp(&json!({
            "name": "describe", "description": "What it sounds like",
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}},
            "annotations": {"readOnlyHint": true, "openWorldHint": false, "title": "listen describe"}
        }))
        .unwrap();
        assert_eq!(t.name, "describe");
        assert!(t.read_only && !t.destructive && !t.open_world);
        assert_eq!(t.title.as_deref(), Some("listen describe"));
        let bare = Tool::from_mcp(&json!({"name": "x"})).unwrap();
        assert_eq!(bare.input_schema, json!({"type": "object"}));
        assert!(!bare.read_only && bare.destructive);
        assert!(Tool::from_mcp(&json!({"name": ""})).is_none());
        assert!(Tool::from_mcp(&json!({})).is_none());
    }

    #[test]
    fn results_become_text() {
        let out = CallOutput::from_mcp(&json!({
            "content": [{"type": "text", "text": "loud"}, {"type": "image", "mimeType": "image/jpeg", "data": "AAAA"}],
            "isError": false
        }))
        .unwrap();
        assert_eq!(out.text(), "loud\n[image image/jpeg, 0 KB]");
        assert!(CallOutput::from_mcp(&json!({})).is_none());
        assert!(CallOutput::error("no").is_error);
    }

    #[test]
    fn names_are_provider_safe_and_unique() {
        assert_eq!(qualified_name("listen", "describe"), "listen__describe");
        assert_eq!(qualified_name("my.tool", "a b/c"), "my_tool__a_b_c");
        let long = "x".repeat(80);
        let a = qualified_name("id", &long);
        let b = qualified_name("id", &format!("{long}y"));
        assert_eq!(a.len(), MAX_TOOL_NAME);
        assert_ne!(a, b);
        assert_eq!(integration_of("listen__describe"), Some("listen"));
        assert_eq!(integration_of("plain"), None);
    }
}
