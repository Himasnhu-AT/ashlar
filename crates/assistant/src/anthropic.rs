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
use crate::types::{Block, Message, Native, Role, StopReason, StreamEvent, Thinking, ToolCall, Usage, is_thinking_block};

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

/// The most images one request carries (the newest; older ones are named in text). The API takes 100.
pub const MAX_IMAGES: usize = 40;

fn user_content(m: &Message, skip_images: &mut usize) -> Vec<Value> {
    m.blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text { text } if !text.is_empty() => Some(json!({"type": "text", "text": text})),
            Block::Mention(m) => Some(json!({"type": "text", "text": m.prompt_text()})),
            Block::ToolResult(r) => {
                let text = if r.content.is_empty() { "(no output)" } else { r.content.as_str() };
                let content = if r.images.is_empty() {
                    json!(text)
                } else {
                    let mut parts = vec![json!({"type": "text", "text": text})];
                    for img in &r.images {
                        // the oldest go first, offloaded ones (older still) always
                        if *skip_images > 0 || img.is_offloaded() {
                            *skip_images = skip_images.saturating_sub(1);
                            parts.push(json!({"type": "text", "text": "[an earlier image, left out to keep the request small]"}));
                        } else {
                            parts.push(json!({"type": "image", "source": {"type": "base64", "media_type": img.media_type, "data": img.data}}));
                        }
                    }
                    Value::Array(parts)
                };
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

/// An assistant message as sent. Its native content is replayed as it came; with `binding`, its
/// thinking blocks only while the message was produced after the same prefix (the API refuses
/// thinking whose system prompt, tools or earlier messages changed since), else they are left out.
fn assistant_content(m: &Message, binding: Option<&str>) -> Vec<Value> {
    if let Some(n) = m.native.as_ref().filter(|n| n.format == FORMAT)
        && let Some(a) = n.content.as_array()
    {
        let keep = binding.is_none_or(|b| n.binding.as_deref() == Some(b));
        let content: Vec<Value> = if keep { a.clone() } else { a.iter().filter(|b| !is_thinking_block(b)).cloned().collect() };
        if !content.is_empty() {
            return content;
        }
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

/// The `messages` array, every native turn replayed as it came.
pub fn messages(list: &[Message]) -> Vec<Value> {
    render(list, None)
}

/// The `messages` array; with a fingerprint of the system prompt and tools, thinking blocks whose
/// binding no longer matches are left out (see [`assistant_content`]).
fn render(list: &[Message], mut fp: Option<&mut Fingerprint>) -> Vec<Value> {
    let total: usize = list.iter().flat_map(|m| m.tool_results_iter()).map(|r| r.images.len()).sum();
    let mut skip = total.saturating_sub(MAX_IMAGES);
    let mut out = Vec::with_capacity(list.len());
    for m in list {
        let (role, content) = match m.role {
            Role::User => ("user", user_content(m, &mut skip)),
            Role::Assistant => ("assistant", assistant_content(m, fp.as_deref().map(Fingerprint::hex).as_deref())),
        };
        if content.is_empty() {
            continue;
        }
        if let Some(f) = fp.as_deref_mut() {
            f.message(role, &content);
        }
        out.push(json!({"role": role, "content": content}));
    }
    out
}

/// A running FNV-1a hash of a request prefix as sent, thinking blocks left out (earlier thinking
/// is not part of the prefix a thinking block is bound to).
struct Fingerprint(u64);

impl Fingerprint {
    fn new(system: &str, tools: &[Value]) -> Self {
        let mut f = Fingerprint(0xcbf2_9ce4_8422_2325);
        f.value(&json!(system));
        f.value(&Value::Array(tools.to_vec()));
        f
    }

    fn bytes(&mut self, b: &[u8]) {
        for x in b {
            self.0 ^= u64::from(*x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn value(&mut self, v: &Value) {
        // writing into the hash cannot fail
        let _ = serde_json::to_writer(&mut *self, v);
        self.bytes(&[0]);
    }

    fn message(&mut self, role: &str, content: &[Value]) {
        self.bytes(role.as_bytes());
        self.bytes(&[0]);
        for b in content.iter().filter(|b| !is_thinking_block(b)) {
            self.value(b);
        }
        self.bytes(&[1]);
    }

    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

impl std::io::Write for Fingerprint {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The request body (always streamed).
pub fn request_body(r: &Request<'_>, opts: BodyOptions) -> Value {
    request_parts(r, opts).0
}

/// The request body and the binding of the reply's thinking blocks ([`Native::binding`]): a
/// fingerprint of the prefix the reply follows. Thinking blocks of earlier turns go in only while
/// their binding still matches, so a reopened chat whose system prompt, tools or saved messages
/// changed loses that reasoning instead of being refused.
pub fn request_parts(r: &Request<'_>, opts: BodyOptions) -> (Value, String) {
    let mut body = Map::new();
    body.insert("model".into(), json!(r.model));
    body.insert("max_tokens".into(), json!(r.max_tokens.max(1)));
    if !r.system.is_empty() {
        body.insert("system".into(), json!([{"type": "text", "text": r.system, "cache_control": {"type": "ephemeral"}}]));
    }
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
    let mut fp = Fingerprint::new(r.system, &tools);
    body.insert("messages".into(), Value::Array(render(r.messages, Some(&mut fp))));
    // Cache the conversation so far, not only the system prompt: every step of a tool loop re-sends
    // all earlier tool results, and this puts the cache breakpoint on the last block each time.
    body.insert("cache_control".into(), json!({"type": "ephemeral"}));
    if !tools.is_empty() {
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
    (Value::Object(body), fp.hex())
}

/// Mark a reply's thinking with the binding [`request_parts`] returned.
pub fn bind(mut r: Response, binding: String) -> Response {
    if let Some(n) = r.message.native.as_mut().filter(|n| n.format == FORMAT) {
        n.binding = Some(binding);
    }
    r
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
            native: Some(Native { format: FORMAT.into(), model: model.clone(), content: Value::Array(native), binding: None }),
        };
        Ok(Response { message, stop, usage: self.usage, model, max_tokens_cap: None })
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
            let (body, binding) = request_parts(request, BodyOptions { fallbacks });
            let body = serde_json::to_vec(&body).map_err(|e| Error::Other(e.to_string()))?;
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
            read_stream(reply.body, request.model, events, cancel, "Anthropic").map(|r| bind(r, binding)).map_err(|e| match &id {
                Some(id) if !matches!(e, Error::Cancelled) => e.with_detail(&format!("Request ID: {id}")),
                _ => e,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Response;
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
        assert_eq!(b["cache_control"]["type"], "ephemeral", "the conversation is cached too");
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

    /// A reply with a thinking block, bound to the request it answered.
    fn bound_reply(r: &Request<'_>) -> Message {
        let (_, binding) = request_parts(r, BodyOptions::default());
        let content = json!([{"type": "thinking", "thinking": "", "signature": "s1"}, {"type": "text", "text": "Red."}]);
        let message = Message {
            role: Role::Assistant,
            blocks: vec![Block::Text { text: "Red.".into() }],
            native: Some(Native { format: FORMAT.into(), model: r.model.into(), content, binding: None }),
        };
        bind(Response { message, stop: StopReason::EndTurn, usage: Usage::default(), model: r.model.into(), max_tokens_cap: None }, binding).message
    }

    #[test]
    fn thinking_is_replayed_only_after_the_prefix_it_was_bound_to() {
        let tools = [ToolSpec { name: "execute".into(), description: "Run".into(), input_schema: json!({"type": "object"}), strict: false }];
        let first = [Message::user_text("what colour?")];
        let r = Request {
            model: "claude-opus-5-5",
            system: "sys",
            tools: &tools,
            messages: &first,
            max_tokens: 16000,
            effort: None,
            thinking: Thinking::Default,
        };
        let reply = bound_reply(&r);
        let next = vec![first[0].clone(), reply.clone(), Message::user_text("why?")];
        let thinking = |b: &Value| b["messages"][1]["content"].as_array().unwrap().iter().any(is_thinking_block);
        // the same conversation: the reasoning goes back, and the next reply's binding follows on
        let (b, next_binding) = request_parts(&Request { messages: &next, ..r }, BodyOptions::default());
        assert!(thinking(&b), "{b}");
        assert_ne!(Some(next_binding.as_str()), reply.native.as_ref().unwrap().binding.as_deref());
        // a new system prompt, other tools, an earlier message changed (a result cut on save): left out
        assert!(!thinking(&request_body(&Request { system: "sys v2", messages: &next, ..r }, BodyOptions::default())));
        assert!(!thinking(&request_body(&Request { tools: &[], messages: &next, ..r }, BodyOptions::default())));
        let mut edited = next.clone();
        edited[0] = Message::user_text("what colour is it?");
        let b = request_body(&Request { messages: &edited, ..r }, BodyOptions::default());
        assert!(!thinking(&b));
        assert_eq!(b["messages"][1]["content"], json!([{"type": "text", "text": "Red."}]), "the text stays");
        // a message saved before bindings existed
        let mut legacy = next.clone();
        if let Some(n) = legacy[1].native.as_mut() {
            n.binding = None;
        }
        assert!(!thinking(&request_body(&Request { messages: &legacy, ..r }, BodyOptions::default())));
        // the binding survives a save and load
        let back: Message = serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
        assert!(thinking(&request_body(&Request { messages: &[first[0].clone(), back, Message::user_text("why?")], ..r }, BodyOptions::default())));
        // the plain renderer replays as it came
        assert!(messages(&legacy)[1]["content"].as_array().unwrap().iter().any(is_thinking_block));
    }

    #[test]
    fn foreign_turns_are_rebuilt_and_ids_mapped() {
        let call = ToolCall { id: "call:1".into(), name: "t".into(), input: json!({"a": 1}), invalid_input: None };
        let list = [
            Message::user_text("go"),
            Message { role: Role::Assistant, blocks: vec![Block::Thinking { text: "hm".into() }, Block::ToolCall(call)], native: None },
            Message::tool_results(vec![ToolResult { call_id: "call:1".into(), content: String::new(), is_error: true, ..Default::default() }]),
        ];
        let m = messages(&list);
        assert_eq!(m[1]["content"], json!([{"type": "tool_use", "id": "call_1", "name": "t", "input": {"a": 1}}]));
        assert_eq!(m[2]["content"][0], json!({"type": "tool_result", "tool_use_id": "call_1", "content": "(no output)", "is_error": true}));
    }

    #[test]
    fn images_go_in_the_tool_result_and_only_the_newest_are_sent() {
        let img = |d: &str| crate::types::Image { media_type: "image/png".into(), data: d.into() };
        let mut list = vec![Message::user_text("look")];
        for i in 0..(MAX_IMAGES + 2) {
            list.push(Message::tool_results(vec![ToolResult {
                call_id: format!("c{i}"),
                content: "frame".into(),
                images: vec![img(&format!("d{i}"))],
                ..Default::default()
            }]));
        }
        let m = messages(&list);
        let first = &m[1]["content"][0]["content"];
        assert_eq!(first[0], json!({"type": "text", "text": "frame"}));
        assert_eq!(first[1]["type"], "text", "the oldest images are named, not sent");
        let last = &m[MAX_IMAGES + 2]["content"][0]["content"];
        assert_eq!(
            last[1],
            json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": format!("d{}", MAX_IMAGES + 1)}})
        );
        let sent = m.iter().filter(|v| v["content"][0]["content"][1]["type"] == "image").count();
        assert_eq!(sent, MAX_IMAGES);
        // a result without images keeps the plain string form
        let plain = messages(&[Message::tool_results(vec![ToolResult { call_id: "x".into(), content: "ok".into(), ..Default::default() }])]);
        assert_eq!(plain[0]["content"][0]["content"], "ok");
    }
}
