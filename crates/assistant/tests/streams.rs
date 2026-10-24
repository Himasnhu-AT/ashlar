//! Recorded streams of both wire formats, parsed into the neutral model.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ashlar_assistant::anthropic::{self, BodyOptions};
use ashlar_assistant::{Block, Cancel, Conversation, Error, Message, Request, StopReason, StreamEvent, Thinking, ToolResult, openai};
use serde_json::json;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn anthropic(name: &str) -> (Result<ashlar_assistant::Response, Error>, Vec<StreamEvent>) {
    let mut events = Vec::new();
    let r = anthropic::read_stream(fixture(name).as_slice(), "claude-opus-5-5", &mut |e| events.push(e), &Cancel::new(), "Anthropic");
    (r, events)
}

#[test]
fn anthropic_text() {
    let (r, events) = anthropic("anthropic_text.sse");
    let r = r.unwrap();
    assert_eq!(r.message.text(), "Hello, world.");
    assert_eq!(r.stop, StopReason::EndTurn);
    assert_eq!((r.usage.input_tokens, r.usage.output_tokens, r.usage.cache_write_tokens), (25, 12, 1800));
    let text: String = events.iter().filter_map(|e| if let StreamEvent::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect();
    assert_eq!(text, "Hello, world.");
    assert_eq!(r.message.native.as_ref().unwrap().content, json!([{"type": "text", "text": "Hello, world."}]));
}

#[test]
fn anthropic_tool_use_with_thinking_is_replayed_verbatim() {
    let (r, events) = anthropic("anthropic_tool_use.sse");
    let r = r.unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    let calls: Vec<_> = r.message.tool_calls().collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].input, json!({"id": "sequence.new", "params": {"name": "Intro"}}));
    assert!(matches!(&r.message.blocks[0], Block::Thinking { text } if text == "The user wants a new sequence."));
    assert!(events.contains(&StreamEvent::ToolCallStart { id: "toolu_01A".into(), name: "execute".into() }));
    assert!(events.contains(&StreamEvent::ToolCallEnd { id: "toolu_01A".into() }));
    assert_eq!(r.usage.cache_read_tokens, 1800);

    // the next request carries the assistant turn exactly as it came, signature included
    let native = r.message.native.clone().unwrap().content;
    let mut c = Conversation::new();
    c.push_user_text("make a sequence");
    c.push(r.message);
    c.push(Message::tool_results(vec![ToolResult { call_id: "toolu_01A".into(), content: "{\"id\":\"s1\"}".into(), is_error: false }]));
    let req = Request {
        model: "claude-opus-5-5",
        system: "s",
        tools: &[],
        messages: c.messages(),
        max_tokens: 100,
        effort: None,
        thinking: Thinking::Summarized,
    };
    let body = anthropic::request_body(&req, BodyOptions::default());
    assert_eq!(body["messages"][1]["content"], native);
    assert_eq!(body["messages"][1]["content"][0]["signature"], "EqQBCgIYAhIM1gbcDa9GJwZA2b3hGgxBdjrkzLoky3dl1pkiMOYds");
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "toolu_01A");
}

#[test]
fn anthropic_parallel_tool_use() {
    let r = anthropic("anthropic_parallel.sse").0.unwrap();
    let calls: Vec<_> = r.message.tool_calls().map(|c| (c.id.as_str(), c.input.clone())).collect();
    assert_eq!(calls, [("toolu_A", json!({})), ("toolu_B", json!({"id": "title.add"}))]);
    assert_eq!(r.model, "claude-haiku-5-5");
}

#[test]
fn anthropic_refusal_keeps_the_redacted_block_and_says_why() {
    let r = anthropic("anthropic_refusal.sse").0.unwrap();
    assert_eq!(r.stop, StopReason::Refusal { explanation: Some("This request was declined.".into()) });
    let native = r.message.native.unwrap().content;
    assert_eq!(native[0]["type"], "redacted_thinking");
}

#[test]
fn anthropic_error_frame_is_an_error() {
    let (r, events) = anthropic("anthropic_error.sse");
    assert_eq!(r.unwrap_err().code(), "upstream_error");
    assert!(events.contains(&StreamEvent::TextDelta("Partial".into())));
}

#[test]
fn anthropic_fallback_block_is_kept_for_replay() {
    let r = anthropic("anthropic_fallback.sse").0.unwrap();
    assert_eq!(r.message.text(), "Sure.");
    assert_eq!(r.model, "claude-opus-5");
    assert_eq!(r.message.native.unwrap().content[0]["type"], "fallback");
}

#[test]
fn a_cut_stream_is_an_error_not_a_reply() {
    let text = String::from_utf8(fixture("anthropic_text.sse")).unwrap();
    let cut = &text[..text.find("event: message_delta").unwrap()];
    let r = anthropic::read_stream(cut.as_bytes(), "m", &mut |_| {}, &Cancel::new(), "Anthropic");
    assert_eq!(r.unwrap_err().code(), "offline");
}

#[test]
fn invalid_tool_json_is_flagged_not_run() {
    let broken = [
        r#"event: message_start
data: {"type":"message_start","message":{"model":"m","usage":{"input_tokens":1,"output_tokens":1}}}"#,
        r#"event: content_block_start
data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"t1","name":"x","input":{}}}"#,
        r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"a\": tru"}}"#,
        r#"event: content_block_stop
data: {"type":"content_block_stop","index":0}"#,
        r#"event: message_delta
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}"#,
        r#"event: message_stop
data: {"type":"message_stop"}"#,
    ]
    .join("\n\n");
    let r = anthropic::read_stream(broken.as_bytes(), "m", &mut |_| {}, &Cancel::new(), "Anthropic").unwrap();
    let call = r.message.tool_calls().next().unwrap();
    assert!(call.invalid_input.as_deref().unwrap_or_default().contains("not valid JSON"));
    // the replayed block is well formed
    assert_eq!(r.message.native.unwrap().content[0]["input"], json!({}));
}

fn openai_read(name: &str) -> (Result<ashlar_assistant::Response, Error>, Vec<StreamEvent>) {
    let mut events = Vec::new();
    let r = openai::read_stream(fixture(name).as_slice(), "gpt-test", &mut |e| events.push(e), &Cancel::new(), "server");
    (r, events)
}

#[test]
fn openai_text() {
    let r = openai_read("openai_text.sse").0.unwrap();
    assert_eq!(r.message.text(), "Hi there.");
    assert_eq!(r.stop, StopReason::EndTurn);
    assert_eq!((r.usage.input_tokens, r.usage.cache_read_tokens, r.usage.output_tokens), (30, 20, 3));
}

#[test]
fn openai_parallel_tool_calls() {
    let (r, events) = openai_read("openai_tools.sse");
    let r = r.unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    let calls: Vec<_> = r.message.tool_calls().map(|c| (c.id.as_str(), c.name.as_str(), c.input.clone())).collect();
    assert_eq!(calls, [("call_aaa", "project_inspect", json!({})), ("call_bbb", "describe_command", json!({"id": "title.add"}))]);
    assert!(matches!(&r.message.blocks[0], Block::Thinking { text } if text == "Two lookups."));
    assert!(events.iter().any(|e| matches!(e, StreamEvent::ThinkingDelta(_))));
}

#[test]
fn openai_error_chunk() {
    let r = openai_read("openai_error.sse").0;
    assert_eq!(r.unwrap_err().code(), "rate_limited");
}

#[test]
fn a_conversation_moves_between_formats() {
    // an Anthropic reply (thinking + a call) replayed to an OpenAI-compatible server: thinking
    // is left out, the call and its result map to tool_calls / role "tool"
    let r = anthropic("anthropic_tool_use.sse").0.unwrap();
    let mut c = Conversation::new();
    c.push_user_text("go");
    c.push(r.message);
    c.push(Message::tool_results(vec![ToolResult { call_id: "toolu_01A".into(), content: "done".into(), is_error: false }]));
    let req = Request { model: "local", system: "", tools: &[], messages: c.messages(), max_tokens: 1, effort: None, thinking: Thinking::Default };
    let b = openai::request_body(&req);
    let m = b["messages"].as_array().unwrap();
    assert_eq!(m.len(), 3);
    assert_eq!(m[1]["content"], "I'll create it.");
    assert_eq!(m[1]["tool_calls"][0]["id"], "toolu_01A");
    assert_eq!(m[2]["role"], "tool");
}
