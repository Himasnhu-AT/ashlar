//! The OpenAI-compatible Chat Completions format (`POST {base}/chat/completions`), served by
//! OpenAI, OpenRouter and local model servers (Ollama, LM Studio, llama.cpp…). Tool calls map to
//! `tool_calls` / `role: "tool"` messages; reasoning text some servers stream
//! (`reasoning_content`, `reasoning`) is shown but not replayed.
//!
//! The base URL is the part before `/chat/completions` (`https://api.openai.com/v1`,
//! `http://localhost:11434/v1`); a URL that already ends in `/chat/completions` is used as is.

use serde_json::{Map, Value, json};

use crate::error::{Error, from_http};
use crate::provider::{Cancel, Request, Response};
use crate::sse;
use crate::types::{Block, Message, Native, Role, StopReason, StreamEvent, ToolCall, Usage};

/// [`Native::format`] of this wire format.
pub const FORMAT: &str = "openai";
/// OpenAI's API.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
/// Most tool calls accepted in one reply.
const MAX_CALLS: usize = 256;

/// The endpoint for a base URL.
pub fn endpoint(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") { b.to_string() } else { format!("{b}/chat/completions") }
}

/// The `messages` array, the system prompt first.
pub fn messages(system: &str, list: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    if !system.is_empty() {
        out.push(json!({"role": "system", "content": system}));
    }
    for m in list {
        match m.role {
            Role::User => {
                let mut text = String::new();
                let mut images = Vec::new();
                for b in &m.blocks {
                    match b {
                        Block::ToolResult(r) => {
                            let content = if r.is_error { format!("Error: {}", r.content) } else { r.content.clone() };
                            out.push(json!({"role": "tool", "tool_call_id": r.call_id, "content": content}));
                            images.extend(
                                r.images
                                    .iter()
                                    .map(|i| json!({"type": "image_url", "image_url": {"url": format!("data:{};base64,{}", i.media_type, i.data)}})),
                            );
                        }
                        Block::Text { text: t } => {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                        _ => {}
                    }
                }
                if !images.is_empty() {
                    // tool messages carry text only: the pictures follow in a user message
                    let mut parts = vec![json!({"type": "text", "text": "Images returned by the tool calls above:"})];
                    parts.extend(images);
                    out.push(json!({"role": "user", "content": parts}));
                }
                if !text.is_empty() {
                    out.push(json!({"role": "user", "content": text}));
                }
            }
            Role::Assistant => {
                let text = m.text();
                let calls: Vec<Value> = m
                    .tool_calls()
                    .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": c.input.to_string()}}))
                    .collect();
                if text.is_empty() && calls.is_empty() {
                    continue;
                }
                let mut v = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
                if !calls.is_empty() {
                    v["tool_calls"] = Value::Array(calls);
                }
                out.push(v);
            }
        }
    }
    out
}

/// The request body (streamed, with the usage in the last chunk). No token limit is sent: servers
/// disagree on its name, and each has a sensible default.
pub fn request_body(r: &Request<'_>) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(r.model));
    body.insert("messages".into(), Value::Array(messages(r.system, r.messages)));
    if !r.tools.is_empty() {
        let tools: Vec<Value> = r
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}}))
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        body.insert("tool_choice".into(), json!("auto"));
    }
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    Value::Object(body)
}

#[derive(Debug, Default)]
struct Call {
    id: String,
    name: String,
    args: String,
    started: bool,
}

/// Builds a reply from its chunks.
#[derive(Debug, Default)]
pub struct Assembler {
    text: String,
    thinking: String,
    calls: Vec<Call>,
    finish: Option<String>,
    usage: Usage,
    model: String,
    done: bool,
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take one `data:` payload.
    pub fn on_data(&mut self, data: &str, events: &mut dyn FnMut(StreamEvent), service: &str) -> Result<(), Error> {
        let data = data.trim();
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| Error::Protocol(format!("a stream chunk is not JSON: {e}")))?;
        if v.get("error").is_some_and(|e| !e.is_null()) {
            let status = v["error"]["code"].as_u64().and_then(|c| u16::try_from(c).ok()).unwrap_or(500);
            return Err(from_http(status, None, data.as_bytes(), service));
        }
        if let Some(m) = v["model"].as_str() {
            self.model = m.to_string();
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            let cached = u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
            self.usage = Usage {
                input_tokens: n("prompt_tokens").saturating_sub(cached),
                output_tokens: n("completion_tokens"),
                cache_read_tokens: cached,
                cache_write_tokens: 0,
            };
            events(StreamEvent::Usage(self.usage));
        }
        let Some(choice) = v["choices"].as_array().and_then(|c| c.first()) else { return Ok(()) };
        let d = &choice["delta"];
        if let Some(t) = d["content"].as_str().filter(|t| !t.is_empty()) {
            self.text.push_str(t);
            events(StreamEvent::TextDelta(t.to_string()));
        }
        for key in ["reasoning_content", "reasoning"] {
            if let Some(t) = d[key].as_str().filter(|t| !t.is_empty()) {
                self.thinking.push_str(t);
                events(StreamEvent::ThinkingDelta(t.to_string()));
            }
        }
        for tc in d["tool_calls"].as_array().into_iter().flatten() {
            let i = tc["index"].as_u64().and_then(|i| usize::try_from(i).ok()).unwrap_or(self.calls.len().saturating_sub(1));
            if i >= MAX_CALLS {
                return Err(Error::Protocol("too many tool calls".into()));
            }
            while self.calls.len() <= i {
                self.calls.push(Call::default());
            }
            let Some(c) = self.calls.get_mut(i) else { continue };
            if let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty()) {
                c.id = id.to_string();
            }
            if let Some(n) = tc["function"]["name"].as_str() {
                c.name.push_str(n);
            }
            if !c.started && !c.name.is_empty() {
                if c.id.is_empty() {
                    c.id = format!("call_{i}");
                }
                c.started = true;
                events(StreamEvent::ToolCallStart { id: c.id.clone(), name: c.name.clone() });
            }
            if let Some(a) = tc["function"]["arguments"].as_str().filter(|a| !a.is_empty()) {
                if c.args.len().saturating_add(a.len()) > sse::MAX_LINE * 4 {
                    return Err(Error::Protocol("a tool call's arguments are too long".into()));
                }
                c.args.push_str(a);
                events(StreamEvent::ToolCallDelta { id: c.id.clone(), json: a.to_string() });
            }
        }
        if let Some(f) = choice["finish_reason"].as_str() {
            self.finish = Some(f.to_string());
        }
        Ok(())
    }

    pub fn finish(self, requested_model: &str, events: &mut dyn FnMut(StreamEvent)) -> Result<Response, Error> {
        if !self.done && self.finish.is_none() {
            return Err(Error::Offline("the server (the reply ended early)".into()));
        }
        let model = if self.model.is_empty() { requested_model.to_string() } else { self.model };
        let mut blocks = Vec::new();
        if !self.thinking.is_empty() {
            blocks.push(Block::Thinking { text: self.thinking });
        }
        if !self.text.is_empty() {
            blocks.push(Block::Text { text: self.text });
        }
        let mut any_call = false;
        for c in self.calls.into_iter().filter(|c| !c.name.is_empty()) {
            any_call = true;
            events(StreamEvent::ToolCallEnd { id: c.id.clone() });
            let (input, invalid_input) = match c.args.trim() {
                "" => (json!({}), None),
                a => match serde_json::from_str::<Value>(a) {
                    Ok(v) if v.is_object() => (v, None),
                    Ok(_) => (json!({}), Some("the arguments are not a JSON object".to_string())),
                    Err(e) => (json!({}), Some(format!("the arguments are not valid JSON ({e})"))),
                },
            };
            blocks.push(Block::ToolCall(ToolCall { id: c.id, name: c.name, input, invalid_input }));
        }
        let stop = match self.finish.as_deref() {
            // some servers say `stop` after tool calls
            _ if any_call => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal { explanation: None },
            Some("stop") | None => StopReason::EndTurn,
            Some(other) => StopReason::Other { reason: other.to_string() },
        };
        events(StreamEvent::Stop(stop.clone()));
        let native = Native { format: FORMAT.into(), model: model.clone(), content: Value::Null };
        Ok(Response {
            message: Message { role: Role::Assistant, blocks, native: Some(native) },
            stop,
            usage: self.usage,
            model,
            max_tokens_cap: None,
        })
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
        a.on_data(&ev.data, events, service)?;
        if a.done {
            break;
        }
    }
    a.finish(requested_model, events)
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use api::OpenAiCompatible;

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
mod api {
    use super::*;
    use crate::http::{Http, check_url, host};
    use crate::keys::ApiKey;
    use crate::provider::Provider;

    /// A Chat Completions server, with an optional key (local servers need none).
    pub struct OpenAiCompatible {
        url: String,
        key: Option<ApiKey>,
        name: String,
        http: Http,
    }

    impl OpenAiCompatible {
        pub fn new(base_url: &str, key: Option<ApiKey>) -> Self {
            let url = endpoint(base_url);
            let name = host(&url).unwrap_or_else(|| "the model server".into());
            Self { url, key, name, http: Http::new() }
        }
    }

    impl Provider for OpenAiCompatible {
        fn id(&self) -> &str {
            "openai"
        }

        fn send(&mut self, request: &Request<'_>, events: &mut dyn FnMut(StreamEvent), cancel: &Cancel) -> Result<Response, Error> {
            check_url(&self.url, self.key.is_some())?;
            if request.model.trim().is_empty() {
                return Err(Error::Config("Choose a model for the OpenAI-compatible server in the assistant's settings.".into()));
            }
            let body = serde_json::to_vec(&request_body(request)).map_err(|e| Error::Other(e.to_string()))?;
            let auth = self.key.as_ref().map(|k| format!("Bearer {}", k.expose()));
            let mut headers = vec![("accept", "text/event-stream")];
            if let Some(a) = &auth {
                headers.push(("authorization", a));
            }
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut reply = self.http.post(&self.url, &headers, body, &self.name)?;
            if reply.status != 200 {
                let (s, after) = (reply.status, reply.retry_after);
                let e = from_http(s, after, &reply.error_body(), &self.name);
                return Err(reply.detail(e));
            }
            read_stream(reply.body, request.model, events, cancel, &self.name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Thinking, ToolResult, ToolSpec};

    #[test]
    fn endpoints() {
        assert_eq!(endpoint("https://api.openai.com/v1/"), "https://api.openai.com/v1/chat/completions");
        assert_eq!(endpoint("http://localhost:1234/v1/chat/completions"), "http://localhost:1234/v1/chat/completions");
    }

    #[test]
    fn tool_turns_map_both_ways() {
        let call = ToolCall { id: "toolu_1".into(), name: "execute".into(), input: json!({"id": "x"}), invalid_input: None };
        let list = [
            Message::user_text("go"),
            Message { role: Role::Assistant, blocks: vec![Block::Text { text: "On it.".into() }, Block::ToolCall(call)], native: None },
            Message::tool_results(vec![ToolResult { call_id: "toolu_1".into(), content: "bad".into(), is_error: true, ..Default::default() }]),
        ];
        let tools = [ToolSpec { name: "execute".into(), description: "d".into(), input_schema: json!({"type": "object"}), strict: false }];
        let r = Request { model: "m", system: "sys", tools: &tools, messages: &list, max_tokens: 100, effort: None, thinking: Thinking::Default };
        let b = request_body(&r);
        let m = &b["messages"];
        assert_eq!(m[0], json!({"role": "system", "content": "sys"}));
        assert_eq!(m[2]["tool_calls"][0]["function"]["arguments"], "{\"id\":\"x\"}");
        assert_eq!(m[3], json!({"role": "tool", "tool_call_id": "toolu_1", "content": "Error: bad"}));
        assert_eq!(b["tools"][0]["function"]["parameters"]["type"], "object");
        for k in ["temperature", "max_tokens", "top_p"] {
            assert!(b.get(k).is_none());
        }
    }

    #[test]
    fn tool_images_follow_in_a_user_message() {
        let img = crate::types::Image { media_type: "image/jpeg".into(), data: "QUJD".into() };
        let m = messages(
            "",
            &[Message::tool_results(vec![ToolResult { call_id: "t1".into(), content: "frame".into(), images: vec![img], ..Default::default() }])],
        );
        assert_eq!(m[0]["role"], "tool");
        assert_eq!(m[1]["role"], "user");
        assert_eq!(m[1]["content"][1]["image_url"]["url"], "data:image/jpeg;base64,QUJD");
    }
}
