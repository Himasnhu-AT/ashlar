//! The agent loop: send the conversation, stream the reply, run the tools it asks for (after the
//! user approves the ones the app marks), send the results back, until the model ends its turn.
//!
//! The conversation stays valid for the next request whatever happens: every tool call gets a
//! result (an error result when it was refused, declined, cancelled or not run), all results of
//! one reply go back in one message, and nothing earlier is ever changed.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;
use crate::provider::{Cancel, Provider, Request};
use crate::schema;
use crate::types::{Conversation, Effort, Message, StopReason, StreamEvent, Thinking, ToolCall, ToolResult, ToolSpec, Usage, price};

/// How a turn runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentConfig {
    pub model: String,
    /// Keep it the same for the whole conversation (it is cached).
    pub system: String,
    pub max_tokens: u32,
    pub effort: Option<Effort>,
    pub thinking: Thinking,
    /// Most requests in one turn (each tool round is one).
    pub max_steps: u32,
}

impl AgentConfig {
    /// Chat defaults: 16 000 output tokens, effort `medium`, thinking summaries shown, 24 steps.
    pub fn new(model: impl Into<String>, system: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            system: system.into(),
            max_tokens: 16_000,
            effort: Some(Effort::Medium),
            thinking: Thinking::Summarized,
            max_steps: 24,
        }
    }
}

/// What a tool returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: false }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self { content: content.into(), is_error: true }
    }
}

/// The app's side: the tools and which calls need the user's approval.
pub trait ToolHost {
    /// The tools, in a stable order.
    fn tools(&mut self) -> Vec<ToolSpec>;

    /// Why this call needs the user's approval (shown to them), `None` when it can just run.
    fn needs_approval(&mut self, call: &ToolCall) -> Option<String> {
        let _ = call;
        None
    }

    /// Run a call whose arguments matched the tool's schema.
    fn call(&mut self, call: &ToolCall, cancel: &Cancel) -> ToolOutput;
}

/// The user's answer to an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Approval {
    Allow,
    Deny,
}

/// What happened, for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// A request starts (`step` counts from 1).
    Step {
        step: u32,
    },
    Stream(StreamEvent),
    /// A message joined the conversation (the reply, then its tool results).
    Appended(Message),
    ToolStarted(ToolCall),
    ToolFinished {
        call_id: String,
        output: ToolOutput,
    },
    ApprovalRequested {
        call: ToolCall,
        reason: String,
    },
    ApprovalAnswered {
        call_id: String,
        approval: Approval,
    },
    /// One request's tokens and, when the model's price is known, its cost.
    StepUsage {
        usage: Usage,
        cost_usd: Option<f64>,
    },
}

/// The UI's side: it hears every event and answers approval requests.
pub trait Observer {
    fn event(&mut self, e: AgentEvent);

    /// Block until the user answers (or `cancel` is set: then [`Approval::Deny`]).
    fn approve(&mut self, call: &ToolCall, reason: &str, cancel: &Cancel) -> Approval;
}

/// Why a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEnd {
    /// The model finished its answer.
    Done,
    Cancelled,
    /// [`AgentConfig::max_steps`] requests were made.
    MaxSteps,
    /// The reply hit the token limit (its tool calls were not run). `capped_by_server`: the
    /// server had lowered the limit to this because the account's AI allowance is nearly used up.
    MaxTokens {
        capped_by_server: Option<u32>,
    },
    /// The model declined.
    Refusal {
        explanation: Option<String>,
    },
    Error(Error),
}

impl TurnEnd {
    /// A line for the user, `None` for a normal end.
    pub fn message(&self) -> Option<String> {
        match self {
            TurnEnd::Done => None,
            TurnEnd::Cancelled => Some("Stopped.".into()),
            TurnEnd::MaxSteps => Some("Stopped after the most steps one message may take. Send another message to continue.".into()),
            TurnEnd::MaxTokens { capped_by_server: Some(_) } => {
                Some("The reply was shortened because your AI allowance is nearly used up. Add your own API key to keep going.".into())
            }
            TurnEnd::MaxTokens { capped_by_server: None } => {
                Some("The reply was cut off at its length limit. Ask for a shorter answer or continue.".into())
            }
            TurnEnd::Refusal { explanation } => Some(match explanation {
                Some(e) if !e.is_empty() => format!("The model declined this request: {e}"),
                _ => "The model declined this request. Try rephrasing it.".into(),
            }),
            TurnEnd::Error(e) => Some(e.to_string()),
        }
    }
}

/// How a turn went.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnOutcome {
    pub end: TurnEnd,
    /// All requests' tokens together.
    pub usage: Usage,
    /// Their cost, when every model's price is known.
    pub cost_usd: Option<f64>,
    pub steps: u32,
}

/// Results longer than this many bytes are cut (keeps one tool from filling the context).
pub const MAX_RESULT: usize = 100_000;

fn cut(mut s: String) -> String {
    if s.len() > MAX_RESULT {
        let mut end = MAX_RESULT;
        while !s.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        s.truncate(end);
        s.push_str("\n… (cut: the result was longer than 100 000 bytes)");
    }
    s
}

/// Answer one call: refused input, unknown tool, approval, then the tool.
fn answer(call: &ToolCall, specs: &[ToolSpec], tools: &mut dyn ToolHost, observer: &mut dyn Observer, cancel: &Cancel) -> ToolOutput {
    if let Some(e) = &call.invalid_input {
        return ToolOutput::error(format!("INVALID_JSON: {e}. Call the tool again with valid JSON arguments."));
    }
    let Some(spec) = specs.iter().find(|s| s.name == call.name) else {
        return ToolOutput::error(format!("There is no tool named `{}`.", call.name));
    };
    if let Err(e) = schema::validate(&spec.input_schema, &call.input) {
        return ToolOutput::error(format!("INVALID_ARGUMENTS: {e}. Call the tool again with arguments that match its schema."));
    }
    if let Some(reason) = tools.needs_approval(call) {
        observer.event(AgentEvent::ApprovalRequested { call: call.clone(), reason: reason.clone() });
        let a = observer.approve(call, &reason, cancel);
        observer.event(AgentEvent::ApprovalAnswered { call_id: call.id.clone(), approval: a });
        if a == Approval::Deny {
            return ToolOutput::error(if cancel.is_cancelled() {
                "The user stopped the assistant before this ran."
            } else {
                "The user declined this action. Ask before trying something else."
            });
        }
    }
    if cancel.is_cancelled() {
        return ToolOutput::error("The user stopped the assistant before this ran.");
    }
    observer.event(AgentEvent::ToolStarted(call.clone()));
    let out = tools.call(call, cancel);
    ToolOutput { content: cut(out.content), is_error: out.is_error }
}

/// Run one turn on `conversation`, whose last message is the user's. Appends the replies and tool
/// results to it.
pub fn run_turn(
    provider: &mut dyn Provider,
    tools: &mut dyn ToolHost,
    conversation: &mut Conversation,
    config: &AgentConfig,
    observer: &mut dyn Observer,
    cancel: &Cancel,
) -> TurnOutcome {
    let specs = tools.tools();
    let mut usage = Usage::default();
    let mut cost: Option<f64> = Some(0.0);
    let mut steps = 0u32;
    let end = loop {
        if cancel.is_cancelled() {
            break TurnEnd::Cancelled;
        }
        if steps >= config.max_steps.max(1) {
            break TurnEnd::MaxSteps;
        }
        steps += 1;
        observer.event(AgentEvent::Step { step: steps });
        let request = Request {
            model: &config.model,
            system: &config.system,
            tools: &specs,
            messages: conversation.messages(),
            max_tokens: config.max_tokens,
            effort: config.effort,
            thinking: config.thinking,
        };
        let reply = {
            let mut forward = |e: StreamEvent| observer.event(AgentEvent::Stream(e));
            provider.send(&request, &mut forward, cancel)
        };
        let reply = match reply {
            Ok(r) => r,
            Err(Error::Cancelled) => break TurnEnd::Cancelled,
            Err(e) => break TurnEnd::Error(e),
        };
        usage.add(&reply.usage);
        let step_cost = price(&reply.model).or_else(|| price(&config.model)).map(|p| reply.usage.cost_usd(&p));
        cost = cost.zip(step_cost).map(|(a, b)| a + b);
        observer.event(AgentEvent::StepUsage { usage: reply.usage, cost_usd: step_cost });
        let calls: Vec<ToolCall> = reply.message.tool_calls().cloned().collect();
        conversation.push(reply.message.clone());
        observer.event(AgentEvent::Appended(reply.message));
        // a reply cut off or declined may hold half-written calls: never run them
        let end = match &reply.stop {
            StopReason::MaxTokens => Some(TurnEnd::MaxTokens { capped_by_server: reply.max_tokens_cap }),
            StopReason::Refusal { explanation } => Some(TurnEnd::Refusal { explanation: explanation.clone() }),
            _ => None,
        };
        if calls.is_empty() {
            match (end, &reply.stop) {
                (Some(e), _) => break e,
                (None, StopReason::PauseTurn) => continue,
                (None, _) => break TurnEnd::Done,
            }
        }
        let mut results = Vec::with_capacity(calls.len());
        for call in &calls {
            let out = if let Some(e) = &end {
                ToolOutput::error(match e {
                    TurnEnd::MaxTokens { .. } => "Not run: the reply was cut off at its length limit.",
                    _ => "Not run: the reply was declined.",
                })
            } else if cancel.is_cancelled() {
                ToolOutput::error("The user stopped the assistant before this ran.")
            } else {
                answer(call, &specs, tools, observer, cancel)
            };
            observer.event(AgentEvent::ToolFinished { call_id: call.id.clone(), output: out.clone() });
            results.push(ToolResult { call_id: call.id.clone(), content: out.content, is_error: out.is_error });
        }
        let m = Message::tool_results(results);
        conversation.push(m.clone());
        observer.event(AgentEvent::Appended(m));
        if let Some(e) = end {
            break e;
        }
    };
    TurnOutcome { end, usage, cost_usd: cost, steps }
}

/// An observer that approves nothing and ignores events (scripts, tests).
pub struct DenyAll;

impl Observer for DenyAll {
    fn event(&mut self, _e: AgentEvent) {}
    fn approve(&mut self, _call: &ToolCall, _reason: &str, _cancel: &Cancel) -> Approval {
        Approval::Deny
    }
}

/// Tools from closures (small apps and tests).
pub struct FnTools {
    specs: Vec<ToolSpec>,
    run: Box<dyn FnMut(&ToolCall) -> ToolOutput + Send>,
    approval: Box<dyn FnMut(&ToolCall) -> Option<String> + Send>,
}

impl FnTools {
    pub fn new(specs: Vec<ToolSpec>, run: impl FnMut(&ToolCall) -> ToolOutput + Send + 'static) -> Self {
        Self { specs, run: Box::new(run), approval: Box::new(|_| None) }
    }

    pub fn approval(mut self, f: impl FnMut(&ToolCall) -> Option<String> + Send + 'static) -> Self {
        self.approval = Box::new(f);
        self
    }
}

impl ToolHost for FnTools {
    fn tools(&mut self) -> Vec<ToolSpec> {
        self.specs.clone()
    }

    fn needs_approval(&mut self, call: &ToolCall) -> Option<String> {
        (self.approval)(call)
    }

    fn call(&mut self, call: &ToolCall, _cancel: &Cancel) -> ToolOutput {
        (self.run)(call)
    }
}

/// Several tool hosts as one: the app's own tools and, say, external integrations. `tools()`
/// lists each host's tools in host order (keep each host's order stable, for caching); a call and
/// its approval go to the host that listed the name (a name listed twice keeps the first host).
pub struct CombinedTools {
    hosts: Vec<Box<dyn ToolHost>>,
    /// Tool name → host index, from the last `tools()`.
    owner: std::collections::HashMap<String, usize>,
}

impl CombinedTools {
    pub fn new(hosts: Vec<Box<dyn ToolHost>>) -> Self {
        Self { hosts, owner: std::collections::HashMap::new() }
    }

    fn host(&mut self, name: &str) -> Option<&mut Box<dyn ToolHost>> {
        let i = *self.owner.get(name)?;
        self.hosts.get_mut(i)
    }
}

impl ToolHost for CombinedTools {
    fn tools(&mut self) -> Vec<ToolSpec> {
        self.owner.clear();
        let mut out = Vec::new();
        for (i, h) in self.hosts.iter_mut().enumerate() {
            for t in h.tools() {
                if !self.owner.contains_key(&t.name) {
                    self.owner.insert(t.name.clone(), i);
                    out.push(t);
                }
            }
        }
        out
    }

    fn needs_approval(&mut self, call: &ToolCall) -> Option<String> {
        self.host(&call.name)?.needs_approval(call)
    }

    fn call(&mut self, call: &ToolCall, cancel: &Cancel) -> ToolOutput {
        match self.host(&call.name) {
            Some(h) => h.call(call, cancel),
            None => ToolOutput::error(format!("There is no tool named `{}`.", call.name)),
        }
    }
}

/// A short form of a call's arguments for a one-line display (`{"id":"seq.new","params":{…}}`).
pub fn short_args(input: &Value, max: usize) -> String {
    let s = input.to_string();
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}
