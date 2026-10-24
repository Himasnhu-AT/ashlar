//! The agent loop with a scripted provider.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ashlar_assistant::*;
use serde_json::{Value, json};

fn spec(name: &str, schema: Value) -> ToolSpec {
    ToolSpec { name: name.into(), description: format!("the {name} tool"), input_schema: schema, strict: false }
}

fn tools(log: Arc<Mutex<Vec<String>>>) -> FnTools {
    FnTools::new(
        vec![
            spec("inspect", json!({"type": "object", "properties": {}, "additionalProperties": false})),
            spec("execute", json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"], "additionalProperties": false})),
        ],
        move |c| {
            log.lock().unwrap().push(format!("{} {}", c.name, c.input));
            if c.input["id"] == "fail" { ToolOutput::error("it failed") } else { ToolOutput::ok(format!("ran {}", c.name)) }
        },
    )
    .approval(|c| (c.input["id"] == "app.quit").then(|| "Quits the app".to_string()))
}

#[derive(Default)]
struct Recorder {
    events: Vec<AgentEvent>,
    answer: Option<Approval>,
}

impl Observer for Recorder {
    fn event(&mut self, e: AgentEvent) {
        self.events.push(e);
    }
    fn approve(&mut self, _c: &ToolCall, _r: &str, _cancel: &Cancel) -> Approval {
        self.answer.unwrap_or(Approval::Deny)
    }
}

fn run(provider: &mut MockProvider, conv: &mut Conversation, obs: &mut Recorder, log: Arc<Mutex<Vec<String>>>) -> TurnOutcome {
    let mut t = tools(log);
    let cfg = AgentConfig::new("claude-opus-5-5", "You edit video.");
    run_turn(provider, &mut t, conv, &cfg, obs, &Cancel::new())
}

#[test]
fn parallel_calls_answer_in_one_message() {
    let mut p = MockProvider::new([
        MockReply::tools(&[("a", "inspect", json!({})), ("b", "execute", json!({"id": "fail"})), ("c", "execute", json!({"id": "seq.new"}))]),
        MockReply::text("Done."),
    ]);
    let mut c = Conversation::new();
    c.push_user_text("make a sequence");
    let log = Arc::default();
    let mut obs = Recorder::default();
    let o = run(&mut p, &mut c, &mut obs, Arc::clone(&log));
    assert_eq!(o.end, TurnEnd::Done);
    assert_eq!(o.steps, 2);
    assert_eq!(c.len(), 4);
    let results: Vec<_> = c.messages()[2].tool_results_iter().map(|r| (r.call_id.as_str(), r.is_error)).collect();
    assert_eq!(results, [("a", false), ("b", true), ("c", false)]);
    assert_eq!(c.messages()[3].text(), "Done.");
    // the second request saw the whole conversation, the system prompt and the tools
    let reqs = p.requests();
    assert_eq!(reqs[1].messages.len(), 3);
    assert_eq!(reqs[1].system, "You edit video.");
    assert_eq!(reqs[1].tools, ["inspect", "execute"]);
    assert!(o.cost_usd.unwrap() > 0.0);
    assert_eq!(o.usage.input_tokens, 200);
    assert_eq!(log.lock().unwrap().len(), 3);
}

#[test]
fn bad_arguments_and_unknown_tools_are_errors_without_running() {
    let mut p = MockProvider::new([MockReply::tools(&[("a", "execute", json!({"idd": "x"})), ("b", "nope", json!({}))]), MockReply::text("ok")]);
    let mut c = Conversation::new();
    c.push_user_text("x");
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    run(&mut p, &mut c, &mut Recorder::default(), Arc::clone(&log));
    let r: Vec<_> = c.messages()[2].tool_results_iter().cloned().collect();
    assert!(r[0].is_error && r[0].content.contains("INVALID_ARGUMENTS"));
    assert!(r[1].is_error && r[1].content.contains("no tool named"));
    assert!(log.lock().unwrap().is_empty());
}

#[test]
fn approvals() {
    for (answer, ran) in [(Approval::Allow, true), (Approval::Deny, false)] {
        let mut p = MockProvider::new([MockReply::tool("q", "execute", json!({"id": "app.quit"}), Some("Quitting.")), MockReply::text("ok")]);
        let mut c = Conversation::new();
        c.push_user_text("quit");
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let mut obs = Recorder { answer: Some(answer), ..Recorder::default() };
        run(&mut p, &mut c, &mut obs, Arc::clone(&log));
        assert_eq!(!log.lock().unwrap().is_empty(), ran);
        assert!(obs.events.iter().any(|e| matches!(e, AgentEvent::ApprovalRequested { reason, .. } if reason == "Quits the app")));
        let r = c.messages()[2].tool_results_iter().next().unwrap();
        assert_eq!(r.is_error, !ran);
        if !ran {
            assert!(r.content.contains("declined"));
        }
    }
}

#[test]
fn refusal_and_max_tokens_never_run_tools() {
    for (stop, end) in [
        (StopReason::Refusal { explanation: None }, TurnEnd::Refusal { explanation: None }),
        (StopReason::MaxTokens, TurnEnd::MaxTokens { capped_by_server: None }),
    ] {
        let call = Block::ToolCall(ToolCall { id: "t".into(), name: "execute".into(), input: json!({"id": "x"}), invalid_input: None });
        let mut p = MockProvider::new([MockReply::Reply { blocks: vec![call], stop, usage: Usage::default() }]);
        let mut c = Conversation::new();
        c.push_user_text("x");
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let o = run(&mut p, &mut c, &mut Recorder::default(), Arc::clone(&log));
        assert_eq!(o.end, end);
        assert!(log.lock().unwrap().is_empty());
        // still answered, so the next request is valid
        assert!(c.unanswered_calls().is_empty());
        assert!(o.end.message().is_some());
    }
}

#[test]
fn errors_end_the_turn_with_a_readable_message() {
    let mut p = MockProvider::new([MockReply::Fail(Error::PlanRequired("AI is part of Pro.".into()))]);
    let mut c = Conversation::new();
    c.push_user_text("x");
    let o = run(&mut p, &mut c, &mut Recorder::default(), Arc::default());
    assert_eq!(o.end.message().unwrap(), "AI is part of Pro.");
    assert_eq!(c.len(), 1);
}

#[test]
fn max_steps() {
    let mut p = MockProvider::new((0..5).map(|i| MockReply::tool(&format!("t{i}"), "inspect", json!({}), None)));
    let mut c = Conversation::new();
    c.push_user_text("loop");
    let mut t = tools(Arc::default());
    let cfg = AgentConfig { max_steps: 3, ..AgentConfig::new("m", "s") };
    let o = run_turn(&mut p, &mut t, &mut c, &cfg, &mut Recorder::default(), &Cancel::new());
    assert_eq!(o.end, TurnEnd::MaxSteps);
    assert_eq!(o.steps, 3);
    assert!(c.unanswered_calls().is_empty());
    assert_eq!(o.cost_usd, None);
}

#[test]
fn worker_streams_asks_and_finishes() {
    let mut p = MockProvider::new([MockReply::tool("q", "execute", json!({"id": "app.quit"}), Some("I'll quit.")), MockReply::text("Bye.")]);
    p.delay = Duration::from_millis(1);
    let mut c = Conversation::new();
    c.push_user_text("quit");
    let wakes = Arc::new(Mutex::new(0u32));
    let w = Arc::clone(&wakes);
    let mut h = spawn_turn(
        Box::new(p),
        || Box::new(tools(Arc::default())),
        c,
        AgentConfig::new("claude-haiku-5-5", "s"),
        Arc::new(move || *w.lock().unwrap() += 1),
    );
    let mut text = String::new();
    let finished = loop {
        match h.wait(Duration::from_secs(5)).expect("an event") {
            WorkerEvent::Agent(AgentEvent::Stream(StreamEvent::TextDelta(t))) => text.push_str(&t),
            WorkerEvent::Agent(AgentEvent::ApprovalRequested { call, .. }) => h.answer(&call.id, Approval::Allow),
            WorkerEvent::Finished { conversation, outcome } => break (conversation, outcome),
            _ => {}
        }
    };
    assert_eq!(finished.1.end, TurnEnd::Done);
    assert_eq!(finished.0.len(), 4);
    assert_eq!(text, "I'll quit.Bye.");
    assert!(*wakes.lock().unwrap() > 3);
}

#[test]
fn worker_cancel_stops_a_hanging_request() {
    let p = MockProvider::new([MockReply::Hang]);
    let mut c = Conversation::new();
    c.push_user_text("x");
    let mut h = spawn_turn(Box::new(p), || Box::new(tools(Arc::default())), c, AgentConfig::new("m", "s"), Arc::new(|| {}));
    std::thread::sleep(Duration::from_millis(50));
    h.cancel();
    let end = loop {
        if let Some(WorkerEvent::Finished { outcome, conversation }) = h.wait(Duration::from_secs(5)) {
            assert_eq!(conversation.len(), 1);
            break outcome.end;
        }
    };
    assert_eq!(end, TurnEnd::Cancelled);
}

#[test]
fn worker_cancel_while_waiting_for_approval_answers_the_call() {
    let p = MockProvider::new([MockReply::tool("q", "execute", json!({"id": "app.quit"}), None)]);
    let mut c = Conversation::new();
    c.push_user_text("x");
    let mut h = spawn_turn(Box::new(p), || Box::new(tools(Arc::default())), c, AgentConfig::new("m", "s"), Arc::new(|| {}));
    let (conv, end) = loop {
        match h.wait(Duration::from_secs(5)).expect("an event") {
            WorkerEvent::Agent(AgentEvent::ApprovalRequested { .. }) => h.cancel(),
            WorkerEvent::Finished { conversation, outcome } => break (conversation, outcome.end),
            _ => {}
        }
    };
    assert_eq!(end, TurnEnd::Cancelled);
    assert!(conv.unanswered_calls().is_empty());
}

#[test]
fn a_reply_the_server_cut_short_says_why() {
    let mut p = MockProvider::new([MockReply::CutByServer { text: "Part of an answer".into(), cap: 1024 }]);
    let mut c = Conversation::new();
    c.push_user_text("x");
    let o = run(&mut p, &mut c, &mut Recorder::default(), Arc::default());
    assert_eq!(o.end, TurnEnd::MaxTokens { capped_by_server: Some(1024) });
    assert!(o.end.message().unwrap_or_default().contains("AI allowance is nearly used up"));
}
