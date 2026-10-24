//! The HTTP providers against a loopback server that follows each API.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use ashlar_assistant::testing::{HttpReply, MockServer, anthropic_stream, anthropic_text};
use ashlar_assistant::*;
use serde_json::json;

fn conv() -> Conversation {
    let mut c = Conversation::new();
    c.push_user_text("hello");
    c
}

fn send(p: &mut dyn Provider, model: &str) -> Result<Response, Error> {
    let c = conv();
    let tools = [ToolSpec { name: "execute".into(), description: "Run".into(), input_schema: json!({"type": "object"}), strict: false }];
    let r = Request {
        model,
        system: "sys",
        tools: &tools,
        messages: c.messages(),
        max_tokens: 1000,
        effort: Some(Effort::Low),
        thinking: Thinking::Summarized,
    };
    p.send(&r, &mut |_| {}, &Cancel::new())
}

#[test]
fn anthropic_key_headers_and_body() {
    let s = MockServer::start(|_| HttpReply::sse(anthropic_text("claude-opus-5-5", "Hi!"))).unwrap();
    let mut p = AnthropicApi::new(ApiKey::new("sk-test").unwrap()).base_url(s.url());
    let r = send(&mut p, "claude-opus-5-5").unwrap();
    assert_eq!(r.message.text(), "Hi!");
    let req = &s.requests()[0];
    assert_eq!(req.path, "/v1/messages");
    assert_eq!(req.header("x-api-key"), Some("sk-test"));
    assert_eq!(req.header("anthropic-version"), Some("2026-10-21"));
    assert_eq!(req.header("anthropic-beta"), Some("server-side-fallback-2026-07-01"));
    let b = req.json();
    assert_eq!(b["fallbacks"], "default");
    assert_eq!(b["output_config"]["effort"], "low");
    assert!(b.get("temperature").is_none());
    // no fallbacks where the model has none
    send(&mut p, "claude-haiku-5-5").unwrap();
    assert!(s.requests()[1].header("anthropic-beta").is_none());
}

#[test]
fn anthropic_errors() {
    let s = MockServer::start(|r| match r.json()["model"].as_str() {
        Some("bad-key") => HttpReply::json(401, &json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}})),
        Some("busy") => HttpReply::json(429, &json!({"type": "error", "error": {"type": "rate_limit_error", "message": "Slow down."}}))
            .header("retry-after", "12"),
        _ => HttpReply::json(529, &json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}})),
    })
    .unwrap();
    let mut p = AnthropicApi::new(ApiKey::new("k").unwrap()).base_url(s.url());
    assert_eq!(send(&mut p, "bad-key").unwrap_err().code(), "invalid_key");
    let e = send(&mut p, "busy").unwrap_err();
    assert_eq!(e, Error::RateLimited { retry_after_secs: Some(12), message: "Slow down.".into() });
    assert_eq!(e.to_string(), "Slow down. Try again in 12 s.");
    assert_eq!(send(&mut p, "x").unwrap_err().code(), "upstream_error");
}

#[test]
fn offline() {
    // a port nothing listens on
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut p = AnthropicApi::new(ApiKey::new("k").unwrap()).base_url(format!("http://127.0.0.1:{port}"));
    let e = send(&mut p, "m").unwrap_err();
    assert_eq!(e.code(), "offline");
    assert!(e.to_string().starts_with("Can't reach"));
}

struct Tokens;
impl TokenSource for Tokens {
    fn token(&self) -> Result<String, Error> {
        Ok("old-token".into())
    }
    fn refresh(&self, refused: &str) -> Result<String, Error> {
        assert_eq!(refused, "old-token");
        Ok("new-token".into())
    }
}

#[test]
fn account_backend_contract() {
    let s = MockServer::start(|r| {
        let auth = r.header("authorization").unwrap_or_default().to_string();
        if auth != "Bearer new-token" {
            return HttpReply::json(401, &json!({"error": "unauthorized", "message": "expired"}));
        }
        match (r.method.as_str(), r.path.as_str(), r.json()["model"].as_str()) {
            ("GET", "/v1/ai/status", _) => HttpReply::json(
                200,
                &json!({"available": true, "reason": null, "plan": "pro", "models": [{"id": "claude-opus-5-5", "label": "Opus", "default": true}], "default_model": "claude-opus-5-5", "limits": {"requests_per_day": 200, "requests_left_today": 199}}),
            ),
            ("POST", "/v1/ai/messages", Some("free-user")) => HttpReply::json(403, &json!({"error": "pro_required", "message": "AI is part of Pro. Add your own API key in Settings ▸ AI to use it now."})),
            ("POST", "/v1/ai/messages", Some("spent")) => HttpReply::json(402, &json!({"error": "budget_exhausted", "message": "The studio's AI budget is used up."})),
            ("POST", "/v1/ai/messages", Some("busy")) => HttpReply::json(429, &json!({"error": "rate_limited", "message": "Too many requests."})).header("retry-after", "120"),
            ("POST", "/v1/ai/messages", Some(m)) => HttpReply::sse(anthropic_stream(m, &[json!({"type": "tool_use", "id": "toolu_1", "name": "execute", "input": {"id": "sequence.new"}})], "tool_use")),
            _ => HttpReply::json(404, &json!({"error": "not_found", "message": "no"})),
        }
    })
    .unwrap();
    let mut b = AccountBackend::new(s.url(), "the account server", Arc::new(Tokens));
    let st = b.status().unwrap();
    assert!(st.available);
    assert_eq!(st.model(), Some("claude-opus-5-5"));
    let r = send(&mut b, "claude-opus-5-5").unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.message.tool_calls().next().unwrap().input, json!({"id": "sequence.new"}));
    let posted = s.requests().into_iter().rfind(|r| r.method == "POST").unwrap();
    let body = posted.json();
    assert!(body.get("fallbacks").is_none(), "the backend adds fallbacks itself");
    assert_eq!(body["stream"], true);
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");

    let e = send(&mut b, "free-user").unwrap_err();
    assert_eq!(e.code(), "plan_required");
    assert_eq!(e.to_string(), "AI is part of Pro. Add your own API key in Settings ▸ AI to use it now.");
    assert_eq!(send(&mut b, "spent").unwrap_err().code(), "budget_exhausted");
    assert_eq!(send(&mut b, "busy").unwrap_err().to_string(), "Too many requests. Try again in 2 min.");
}

#[test]
fn signed_out_account() {
    let s = MockServer::start(|_| HttpReply::json(500, &json!({}))).unwrap();
    let tokens: Arc<dyn TokenSource> = Arc::new(|| -> Result<String, Error> { Err(Error::SignInRequired) });
    let mut b = AccountBackend::new(s.url(), "the account server", tokens);
    assert_eq!(send(&mut b, "m").unwrap_err(), Error::SignInRequired);
    assert!(s.requests().is_empty());
}

#[test]
fn openai_compatible_server() {
    let body = std::fs::read_to_string(format!("{}/tests/fixtures/openai_tools.sse", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let s = MockServer::start(move |r| match r.path.as_str() {
        "/v1/models" => HttpReply::json(200, &json!({"data": [{"id": "llama-test"}, {"id": "gpt-test"}]})),
        _ => HttpReply::sse(body.clone()),
    })
    .unwrap();
    let mut p = OpenAiCompatible::new(&format!("{}/v1", s.url()), None);
    let r = send(&mut p, "llama-test").unwrap();
    assert_eq!(r.message.tool_calls().count(), 2);
    let req = &s.requests()[0];
    assert_eq!(req.path, "/v1/chat/completions");
    assert!(req.header("authorization").is_none());
    assert_eq!(req.json()["tools"][0]["type"], "function");
    // with a key
    let mut p = OpenAiCompatible::new(&format!("{}/v1/", s.url()), ApiKey::new("sk-o"));
    send(&mut p, "x").unwrap();
    assert_eq!(s.requests()[1].header("authorization"), Some("Bearer sk-o"));
    // the Test button lists the models
    let keys = MemoryKeys::new();
    let settings =
        Settings { provider: ProviderKind::OpenAi, openai_base_url: format!("{}/v1", s.url()), openai_model: "x".into(), ..Settings::default() };
    let (line, models) = check_connection(&settings, &keys, None).unwrap();
    assert_eq!(models, ["gpt-test", "llama-test"]);
    assert!(line.starts_with("Connected"));
    // no model chosen
    assert_eq!(send(&mut p, "").unwrap_err().code(), "config");
}

#[test]
fn build_provider_from_settings() {
    let keys = MemoryKeys::new();
    let s = Settings { provider: ProviderKind::Anthropic, ..Settings::default() };
    assert_eq!(build_provider(&s, &keys, None).err(), Some(Error::MissingKey));
    keys.set(keys::ANTHROPIC, &ApiKey::new("sk").unwrap()).unwrap();
    assert_eq!(build_provider(&s, &keys, None).unwrap().id(), "anthropic");
    let s = Settings { provider: ProviderKind::Account, ..Settings::default() };
    assert_eq!(build_provider(&s, &keys, None).err().map(|e| e.code()), Some("unavailable"));
    let a = AccountConfig { base_url: "https://example.com".into(), name: "the server".into(), tokens: Arc::new(Tokens) };
    assert_eq!(build_provider(&s, &keys, Some(&a)).unwrap().id(), "account");
}

#[test]
fn keys_never_go_over_plain_http_to_another_host() {
    let mut p = OpenAiCompatible::new("http://10.1.2.3:8080/v1", ApiKey::new("sk"));
    assert_eq!(send(&mut p, "m").unwrap_err().code(), "config");
}

#[test]
fn account_backend_contract_additions() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let tries = Arc::new(AtomicU32::new(0));
    let t = Arc::clone(&tries);
    let s = MockServer::start(move |r| {
        let rid = ("x-request-id", "air_test123");
        match r.json()["model"].as_str() {
            Some("off") => HttpReply::json(503, &json!({"error": "disabled", "message": "AI is switched off on this server."})).header(rid.0, rid.1),
            Some("broken") => HttpReply::json(502, &json!({"error": "upstream_error", "message": "The AI provider failed. Try again."})).header(rid.0, rid.1),
            Some("bad") => HttpReply::json(400, &json!({"error": "invalid_request", "message": "messages.0.content: text must be non-empty"})).header(rid.0, rid.1),
            Some("huge") => HttpReply::json(413, &json!({"error": "too_large", "message": "too large"})),
            Some("daily") => HttpReply::json(429, &json!({"error": "rate_limited", "message": "You've used today's AI requests."})).header("retry-after", "30000"),
            Some("held") if t.fetch_add(1, Ordering::SeqCst) == 0 => HttpReply::json(429, &json!({"error": "rate_limited", "message": "Busy."})).header("retry-after", "1"),
            Some("fallback") => {
                // the server added fallbacks: a fallback block and usage.iterations
                let mut sse = anthropic_stream("claude-opus-5", &[json!({"type": "fallback", "from": {"model": "claude-opus-5-5"}, "to": {"model": "claude-opus-5"}}), json!({"type": "text", "text": "Done."})], "end_turn");
                sse = sse.replace(r#""usage":{"output_tokens":42}"#, r#""usage":{"output_tokens":42,"iterations":[{"type":"message","input_tokens":10},{"type":"fallback_message","input_tokens":12}]}"#);
                HttpReply::sse(sse).header(rid.0, rid.1)
            }
            Some(m) => HttpReply::sse(anthropic_text(m, "Hi")),
            None => HttpReply::json(400, &json!({})),
        }
    })
    .unwrap();
    let tokens: Arc<dyn TokenSource> = Arc::new(|| -> Result<String, Error> { Ok("t".into()) });
    let mut b = AccountBackend::new(s.url(), "the account server", tokens);
    let e = send(&mut b, "off").unwrap_err();
    assert_eq!(e.to_string(), "AI is switched off on this server. (Request ID: air_test123)");
    assert_eq!(e.code(), "unavailable");
    assert_eq!(send(&mut b, "broken").unwrap_err().to_string(), "The AI provider failed. Try again. (Request ID: air_test123)");
    assert_eq!(
        send(&mut b, "bad").unwrap_err().to_string(),
        "The request was refused: messages.0.content: text must be non-empty (Request ID: air_test123)"
    );
    assert_eq!(send(&mut b, "huge").unwrap_err().code(), "too_large");
    // the daily limit is not waited out; it says when
    let e = send(&mut b, "daily").unwrap_err();
    assert_eq!(e.to_string(), "You've used today's AI requests. Try again in 9 h.");
    // a short wait (budget held by requests in flight) is waited out once
    let r = send(&mut b, "held").unwrap();
    assert_eq!(r.message.text(), "Hi");
    assert_eq!(tries.load(Ordering::SeqCst), 2);
    // fallback blocks and usage.iterations pass through and are replayed verbatim
    let r = send(&mut b, "fallback").unwrap();
    assert_eq!(r.message.text(), "Done.");
    let native = r.message.native.clone().unwrap().content;
    assert_eq!(native[0]["type"], "fallback");
    let mut c = Conversation::new();
    c.push_user_text("x");
    c.push(r.message);
    c.push_user_text("y");
    let req = Request {
        model: "claude-opus-5-5",
        system: "",
        tools: &[],
        messages: c.messages(),
        max_tokens: 10,
        effort: None,
        thinking: Thinking::Default,
    };
    assert_eq!(anthropic::request_body(&req, anthropic::BodyOptions::default())["messages"][1]["content"], native);
}

#[test]
fn combined_tools_route_by_name() {
    let spec = |n: &str| ToolSpec { name: n.into(), description: String::new(), input_schema: json!({"type": "object"}), strict: false };
    let a = FnTools::new(vec![spec("execute"), spec("doc_inspect")], |c| ToolOutput::ok(format!("app {}", c.name)));
    let b = FnTools::new(vec![spec("earshot__synth"), spec("execute")], |c| ToolOutput::ok(format!("ext {}", c.name)))
        .approval(|_| Some("Writes files.".into()));
    let mut t = CombinedTools::new(vec![Box::new(a), Box::new(b)]);
    let names: Vec<_> = t.tools().into_iter().map(|s| s.name).collect();
    assert_eq!(names, ["execute", "doc_inspect", "earshot__synth"]);
    let call = |n: &str| ToolCall { id: "1".into(), name: n.into(), input: json!({}), invalid_input: None };
    assert_eq!(t.call(&call("execute"), &Cancel::new()).content, "app execute");
    assert_eq!(t.call(&call("earshot__synth"), &Cancel::new()).content, "ext earshot__synth");
    assert_eq!(t.needs_approval(&call("earshot__synth")).as_deref(), Some("Writes files."));
    assert!(t.needs_approval(&call("execute")).is_none());
    assert!(t.call(&call("nope"), &Cancel::new()).is_error);
}
