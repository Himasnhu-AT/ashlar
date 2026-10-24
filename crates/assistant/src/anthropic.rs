//! The Anthropic Messages wire format (API version [`VERSION`]): request bodies, the streamed
//! reply, and the provider for a user's own key at api.anthropic.com. The account-backend
//! provider ([`crate::account`]) speaks the same format.
//!
//! Requests follow the current model defaults: streaming always; adaptive thinking (or no
//! thinking setting) and `output_config.effort`, never a thinking budget or sampling settings;
//! `tool_choice` `auto`; no prefill; the system prompt as one block with `cache_control`; every
//! tool with `eager_input_streaming` (arguments are parsed strictly and checked against the schema
//! before a tool runs). Each assistant turn is replayed exactly as it came back
//! ([`crate::types::Native`]), thinking blocks and signatures included.

use serde_json::{Map, Value, json};

use crate::error::{Error, from_stream_error};
use crate::provider::{Cancel, Request, Response};
use crate::sse;
use crate::types::{Block, Message, Native, Role, StopReason, StreamEvent, Thinking, ToolCall, Usage};

/// [`Native::format`] of this wire format.
pub const FORMAT: &str = "anthropic";
/// The `anthropic-version` header.
pub const VERSION: &str = "2026-10-21";
/// The public API.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// The beta header of server-side refusal fallbacks (`"fallbacks": "default"`).
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Most content blocks accepted in one reply.
const MAX_BLOCKS: usize = 4096;

/// Models that take server-side refusal fallbacks.
pub fn supports_fallbacks(model: &str) -> bool {
    matches!(model, "claude-opus-5-5" | "claude-opus-5" | "claude-sonnet-5-5" | "claude-fable-5-1")
}

/// What a request body adds beyond the neutral request.
#[derive(Debug, Clone, Copy, Default)]
pub struct BodyOptions {
    /// `"fallbacks": "default"` (send [`FALLBACK_BETA`] with it).
    pub fallbacks: bool,
}

/// A tool-call id as the API accepts it (`[A-Za-z0-9_-]`): ids from another provider are mapped.
fn tool_id(id: &str) -> String {
    let s: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if s.is_empty() { "call".into() } else { s }
}

fn user_content(m: &Message) -> Vec<Value> {
    m.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } if !text.is_empty() => Some(json!({"type": "text", "text": text})),
            Block::ToolResult(r) => {
                let content = if r.content.is_empty() { "(no output)" } else { r.content.as_str() };
                let mut v = json!({"type": "tool_result", "tool_use_id": tool_id(&r.call_id), "content": content});
                if r.is_error {
                    v["is_error"] = json!(true);
                }
                Some(v)
            }
            _ => None,
        })
        .collect()
}

fn assistant_content(m: &Message) -> Vec<Value> {
    if let Some(n) = m.native.as_ref().filter(|n| n.format == FORMAT)
        && let Some(a) = n.content.as_array()
    {
        return a.clone();
    }
    m.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } if !text.is_empty() => Some(json!({"type": "text", "text": text})),
            Block::ToolCall(c) => {
                let input = if c.input.is_object() { c.input.clone() } else { json!({}) };
                Some(json!({"type": "tool_use", "id": tool_id(&c.id), "name": c.name, "input": input}))
            }
            _ => None,
        })
        .collect()
}

/// The `messages` array.
pub fn messages(list: &[Message]) -> Vec<Value> {
    list.iter()
        .filter_map(|m| {
            let (role, content) = match m.role {
                Role::User => ("user", user_content(m)),
                Role::Assistant => ("assistant", assistant_content(m)),
            };
            (!content.is_empty()).then(|| json!({"role": role, "content": content}))
        })
        .collect()
}

/// The request body (always streamed).
pub fn request_body(r: &Request<'_>, opts: BodyOptions) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(r.model));
    body.insert("max_tokens".into(), json!(r.max_tokens.max(1)));
    if !r.system.is_empty() {
        body.insert("system".into(), json!([{"type": "text", "text": r.system, "cache_control": {"type": "ephemeral"}}]));
    }
    body.insert("messages".into(), Value::Array(messages(r.messages)));
    if !r.tools.is_empty() {
        let tools: Vec<Value> = r
            .tools
            .iter()
            .map(|t| {
                let mut v = json!({"name": t.name, "description": t.description, "input_schema": t.input_schema, "eager_input_streaming": true});
                if t.strict {
                    v["strict"] = json!(true);
                }
                v
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        body.insert("tool_choice".into(), json!({"type": "auto"}));
    }
    if r.thinking == Thinking::Summarized {
        body.insert("thinking".into(), json!({"type": "adaptive", "display": "summarized"}));
    }
    if let Some(e) = r.effort {
        body.insert("output_config".into(), json!({"effort": e.as_str()}));
    }
    body.insert("stream".into(), json!(true));
    if opts.fallbacks {
        body.insert("fallbacks".into(), json!("default"));
    }
    Value::Object(body)
}

fn usage_from(v: &Value, u: &mut Usage) {
    let n = |k: &str| v.get(k).and_then(Value::as_u64);
    if let Some(x) = n("input_tokens") {
        u.input_tokens = x;
    }
    if let Some(x) = n("output_tokens") {
        u.output_tokens = x;
    }
    if let Some(x) = n("cache_read_input_tokens") {
        u.cache_read_tokens = x;
    }
    if let Some(x) = n("cache_creation_input_tokens") {
        u.cache_write_tokens = x;
    }
}

/// Builds a reply from its stream events.
#[derive(Debug, Default)]
pub struct Assembler {
    blocks: Vec<Value>,
    /// The arguments' JSON text of each tool-use block so far.
    partial: Vec<Option<String>>,
    usage: Usage,
    stop: Option<String>,
    explanation: Option<String>,
    model: String,
    done: bool,
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    fn block_id(&self, i: usize) -> String {
        self.blocks.get(i).and_then(|b| b["id"].as_str()).unwrap_or_default().to_string()
    }

    /// Take one event; `service` names the server in errors.
    pub fn on_event(&mut self, data: &str, events: &mut dyn FnMut(StreamEvent), service: &str) -> Result<(), Error> {
        if data.trim().is_empty() {
            return Ok(());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| Error::Protocol(format!("a stream event is not JSON: {e}")))?;
        let index = || v["index"].as_u64().and_then(|i| usize::try_from(i).ok());
        match v["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let m = &v["message"];
                self.model = m["model"].as_str().unwrap_or_default().to_string();
                usage_from(&m["usage"], &mut self.usage);
                events(StreamEvent::Usage(self.usage));
            }
            "content_block_start" => {
                let Some(i) = index().filter(|&i| i < MAX_BLOCKS) else { return Err(Error::Protocol("a block index is out of range".into())) };
                while self.blocks.len() <= i {
                    self.blocks.push(Value::Null);
                    self.partial.push(None);
                }
                let block = v["content_block"].clone();
                let kind = block["type"].as_str().unwrap_or_default().to_string();
                if let (Some(slot), Some(p)) = (self.blocks.get_mut(i), self.partial.get_mut(i)) {
                    *slot = block;
                    if kind == "tool_use" || kind == "server_tool_use" {
                        *p = Some(String::new());
                    }
                }
                match kind.as_str() {
                    "tool_use" => {
                        let name = self.blocks.get(i).and_then(|b| b["name"].as_str()).unwrap_or_default().to_string();
                        events(StreamEvent::ToolCallStart { id: self.block_id(i), name });
                    }
                    "text" => {
                        let t = self.blocks.get(i).and_then(|b| b["text"].as_str()).unwrap_or_default().to_string();
                        if !t.is_empty() {
                            events(StreamEvent::TextDelta(t));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let Some(i) = index() else { return Err(Error::Protocol("a delta has no index".into())) };
                let d = &v["delta"];
                let id = self.block_id(i);
                let Some(block) = self.blocks.get_mut(i) else { return Err(Error::Protocol("a delta for a block that did not start".into())) };
                let append = |block: &mut Value, key: &str, s: &str| {
                    let mut cur = block[key].as_str().unwrap_or_default().to_string();
                    cur.push_str(s);
                    block[key] = json!(cur);
                };
                match d["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let s = d["text"].as_str().unwrap_or_default();
                        append(block, "text", s);
                        events(StreamEvent::TextDelta(s.to_string()));
                    }
                    "thinking_delta" => {
                        let s = d["thinking"].as_str().unwrap_or_default();
                        append(block, "thinking", s);
                        events(StreamEvent::ThinkingDelta(s.to_string()));
                    }
                    "signature_delta" => {
                        block["signature"] = d["signature"].clone();
                    }
                    "citations_delta" => {
                        if !block["citations"].is_array() {
                            block["citations"] = json!([]);
                        }
                        if let Some(a) = block["citations"].as_array_mut() {
                            a.push(d["citation"].clone());
                        }
                    }
                    "input_json_delta" => {
                        let s = d["partial_json"].as_str().unwrap_or_default();
                        if let Some(Some(p)) = self.partial.get_mut(i) {
                            if p.len().saturating_add(s.len()) > sse::MAX_LINE * 4 {
                                return Err(Error::Protocol("a tool call's arguments are too long".into()));
                            }
                            p.push_str(s);
                        }
                        if !id.is_empty() {
                            events(StreamEvent::ToolCallDelta { id, json: s.to_string() });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(i) = index() else { return Ok(()) };
                let id = self.block_id(i);
                if let (Some(block), Some(p)) = (self.blocks.get_mut(i), self.partial.get_mut(i))
                    && let Some(text) = p.take()
                {
                    let is_tool = block["type"] == "tool_use";
                    match parse_input(&text) {
                        Ok(input) => block["input"] = input,
                        Err(e) => {
                            block["input"] = json!({});
                            // kept beside the block, never sent: see `finish`
                            block["__invalid_input"] = json!(e);
                        }
                    }
                    if is_tool {
                        events(StreamEvent::ToolCallEnd { id });
                    }
                }
            }
            "message_delta" => {
                let d = &v["delta"];
                if let Some(s) = d["stop_reason"].as_str() {
                    self.stop = Some(s.to_string());
                }
                let details = if d["stop_details"].is_object() { &d["stop_details"] } else { &v["stop_details"] };
                if let Some(x) = details["explanation"].as_str() {
                    self.explanation = Some(x.to_string());
                }
                usage_from(&v["usage"], &mut self.usage);
                events(StreamEvent::Usage(self.usage));
            }
            "message_stop" => self.done = true,
            "error" => return Err(from_stream_error(&v, service)),
            _ => {}
        }
        Ok(())
    }

    /// The finished reply; an error when the stream ended before `message_stop`.
    pub fn finish(self, requested_model: &str, events: &mut dyn FnMut(StreamEvent)) -> Result<Response, Error> {
        if !self.done {
            return Err(Error::Offline("the server (the reply ended early)".into()));
        }
        let model = if self.model.is_empty() { requested_model.to_string() } else { self.model };
        let mut native = Vec::new();
        let mut blocks = Vec::new();
        for mut b in self.blocks {
            let invalid = b.as_object_mut().and_then(|o| o.remove("__invalid_input")).and_then(|v| v.as_str().map(str::to_string));
            match b["type"].as_str().unwrap_or_default() {
                // a block that never started (a gap in the indices) or an empty text block
                "" => continue,
                "text" if b["text"].as_str().is_none_or(str::is_empty) => continue,
                "text" => blocks.push(Block::Text { text: b["text"].as_str().unwrap_or_default().to_string() }),
                "thinking" => {
                    let text = b["thinking"].as_str().unwrap_or_default().to_string();
                    if !text.is_empty() {
                        blocks.push(Block::Thinking { text });
                    }
                }
                "tool_use" => blocks.push(Block::ToolCall(ToolCall {
                    id: b["id"].as_str().unwrap_or_default().to_string(),
                    name: b["name"].as_str().unwrap_or_default().to_string(),
                    input: b["input"].clone(),
                    invalid_input: invalid,
                })),
                _ => {}
            }
            native.push(b);
        }
        let stop = StopReason::from_anthropic(self.stop.as_deref().unwrap_or("end_turn"), self.explanation);
        events(StreamEvent::Stop(stop.clone()));
        let message = Message {
            role: Role::Assistant,
            blocks,
            native: Some(Native { format: FORMAT.into(), model: model.clone(), content: Value::Array(native) }),
        };
        Ok(Response { message, stop, usage: self.usage, model })
    }
}

/// Tool arguments, parsed strictly (an empty text is no arguments).
fn parse_input(text: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str::<Value>(text) {
        Ok(v) if v.is_object() => Ok(v),
        Ok(_) => Err("the arguments are not a JSON object".into()),
        Err(e) => Err(format!("the arguments are not valid JSON ({e})")),
    }
}

/// Read a whole streamed reply.
pub fn read_stream(
    body: impl std::io::Read,
    requested_model: &str,
    events: &mut dyn FnMut(StreamEvent),
    cancel: &Cancel,
    service: &str,
) -> Result<Response, Error> {
    let mut reader = sse::Reader::new(body);
    let mut a = Assembler::new();
    while let Some(ev) = reader.next_event(cancel)? {
        a.on_event(&ev.data, events, service)?;
        if a.done {
            break;
        }
    }
    a.finish(requested_model, events)
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use api::AnthropicApi;

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
mod api {
    use super::*;
    use crate::error::from_http;
    use crate::http::{Http, check_url};
    use crate::keys::ApiKey;
    use crate::provider::Provider;
    use crate::types::StreamEvent;

    /// Anthropic's API with the user's own key.
    pub struct AnthropicApi {
        key: ApiKey,
        base_url: String,
        fallbacks: bool,
        http: Http,
    }

    impl AnthropicApi {
        pub fn new(key: ApiKey) -> Self {
            Self { key, base_url: DEFAULT_BASE_URL.into(), fallbacks: true, http: Http::new() }
        }

        /// Another server with the same API (a proxy, a test server).
        pub fn base_url(mut self, url: impl Into<String>) -> Self {
            self.base_url = url.into().trim_end_matches('/').to_string();
            self
        }

        /// Server-side refusal fallbacks on the models that take them (on by default).
        pub fn fallbacks(mut self, on: bool) -> Self {
            self.fallbacks = on;
            self
        }
    }

    impl Provider for AnthropicApi {
        fn id(&self) -> &str {
            "anthropic"
        }

        fn send(&mut self, request: &Request<'_>, events: &mut dyn FnMut(StreamEvent), cancel: &Cancel) -> Result<Response, Error> {
            let url = format!("{}/v1/messages", self.base_url);
            check_url(&url, true)?;
            let fallbacks = self.fallbacks && supports_fallbacks(request.model);
            let body = serde_json::to_vec(&request_body(request, BodyOptions { fallbacks })).map_err(|e| Error::Other(e.to_string()))?;
            let mut headers = vec![("x-api-key", self.key.expose()), ("anthropic-version", VERSION), ("accept", "text/event-stream")];
            if fallbacks {
                headers.push(("anthropic-beta", FALLBACK_BETA));
            }
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut reply = self.http.post(&url, &headers, body, "Anthropic")?;
            if reply.status != 200 {
                let (status, after) = (reply.status, reply.retry_after);
                let e = from_http(status, after, &reply.error_body(), "Anthropic");
                return Err(reply.detail(e));
            }
            let id = reply.request_id.clone();
            read_stream(reply.body, request.model, events, cancel, "Anthropic").map_err(|e| match &id {
                Some(id) if !matches!(e, Error::Cancelled) => e.with_detail(&format!("Request ID: {id}")),
                _ => e,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Effort, ToolResult, ToolSpec};

    #[test]
    fn body_follows_the_defaults() {
        let tools = [ToolSpec { name: "execute".into(), description: "Run".into(), input_schema: json!({"type": "object"}), strict: false }];
        let msgs = [Message::user_text("hi")];
        let r = Request {
            model: "claude-opus-5-5",
            system: "sys",
            tools: &tools,
            messages: &msgs,
            max_tokens: 16000,
            effort: Some(Effort::Medium),
            thinking: Thinking::Default,
        };
        let b = request_body(&r, BodyOptions::default());
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(b["tool_choice"]["type"], "auto");
        assert_eq!(b["tools"][0]["eager_input_streaming"], true);
        assert_eq!(b["output_config"]["effort"], "medium");
        assert_eq!(b["stream"], true);
        for k in ["thinking", "temperature", "top_p", "top_k", "fallbacks"] {
            assert!(b.get(k).is_none(), "{k}");
        }
        let b = request_body(&Request { thinking: Thinking::Summarized, tools: &[], ..r }, BodyOptions { fallbacks: true });
        assert_eq!(b["thinking"], json!({"type": "adaptive", "display": "summarized"}));
        assert_eq!(b["fallbacks"], "default");
        assert!(b.get("tools").is_none() && b.get("tool_choice").is_none());
    }

    #[test]
    fn foreign_turns_are_rebuilt_and_ids_mapped() {
        let call = ToolCall { id: "call:1".into(), name: "t".into(), input: json!({"a": 1}), invalid_input: None };
        let list = [
            Message::user_text("go"),
            Message { role: Role::Assistant, blocks: vec![Block::Thinking { text: "hm".into() }, Block::ToolCall(call)], native: None },
            Message::tool_results(vec![ToolResult { call_id: "call:1".into(), content: String::new(), is_error: true }]),
        ];
        let m = messages(&list);
        assert_eq!(m[1]["content"], json!([{"type": "tool_use", "id": "call_1", "name": "t", "input": {"a": 1}}]));
        assert_eq!(m[2]["content"][0], json!({"type": "tool_result", "tool_use_id": "call_1", "content": "(no output)", "is_error": true}));
    }
}
