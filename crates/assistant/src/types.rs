//! The neutral model every provider converts to and from: messages and their blocks, tools,
//! stream events, usage and why a reply stopped.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Who wrote a message. Tool results travel in a user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// A tool the model asked to run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    /// The provider's id for the call; the result names it.
    pub id: String,
    pub name: String,
    /// The arguments (a JSON object).
    pub input: Value,
    /// Set when the arguments the model streamed were not valid JSON: the call is answered with
    /// this error instead of being run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_input: Option<String>,
}

/// The answer to one [`ToolCall`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
}

/// One piece of a message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    /// The model's reasoning, as far as the provider shows it (a summary, or empty). For display:
    /// what is replayed is the message's [`Native`] content.
    Thinking {
        text: String,
    },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

/// A message exactly as one provider's wire format returned it, replayed unchanged to providers
/// of the same format (thinking blocks and their signatures, fallback markers, anything this
/// crate does not model). Other formats rebuild the message from its blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Native {
    /// The wire format: [`crate::anthropic::FORMAT`] or [`crate::openai::FORMAT`].
    pub format: String,
    /// The model that wrote it.
    pub model: String,
    /// The message's content in that format (for Anthropic, the `content` array).
    pub content: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self { role: Role::User, blocks: vec![Block::Text { text: text.into() }], native: None }
    }

    pub fn tool_results(results: Vec<ToolResult>) -> Self {
        Self { role: Role::User, blocks: results.into_iter().map(Block::ToolResult).collect(), native: None }
    }

    /// The text blocks joined.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            if let Block::Text { text } = b {
                out.push_str(text);
            }
        }
        out
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.blocks.iter().filter_map(|b| if let Block::ToolCall(c) = b { Some(c) } else { None })
    }

    pub fn tool_results_iter(&self) -> impl Iterator<Item = &ToolResult> {
        self.blocks.iter().filter_map(|b| if let Block::ToolResult(r) = b { Some(r) } else { None })
    }
}

/// A conversation: append-only, so a provider that caches the prefix (and checks that earlier
/// turns were not edited) sees the same bytes every time. There is no way to change or remove a
/// message once added; start a new conversation instead.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    messages: Vec<Message>,
}

impl Conversation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn last(&self) -> Option<&Message> {
        self.messages.last()
    }

    pub fn push(&mut self, m: Message) {
        self.messages.push(m);
    }

    pub fn push_user_text(&mut self, text: impl Into<String>) {
        self.push(Message::user_text(text));
    }

    /// The tool calls of the last assistant message that have no result yet.
    pub fn unanswered_calls(&self) -> Vec<&ToolCall> {
        let Some(i) = self.messages.iter().rposition(|m| m.role == Role::Assistant) else { return Vec::new() };
        let answered: Vec<&str> =
            self.messages.get(i + 1..).unwrap_or_default().iter().flat_map(|m| m.tool_results_iter()).map(|r| r.call_id.as_str()).collect();
        self.messages.get(i).map(|m| m.tool_calls().filter(|c| !answered.contains(&c.id.as_str())).collect()).unwrap_or_default()
    }
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments (`{"type":"object",…}`).
    pub input_schema: Value,
    /// Ask the provider to guarantee arguments that match the schema (Anthropic `strict`). Only
    /// for schemas within the provider's strict subset (closed objects, every property listed).
    #[serde(default)]
    pub strict: bool,
}

/// How much the model thinks before answering (Anthropic `output_config.effort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "low" => Effort::Low,
            "medium" => Effort::Medium,
            "high" => Effort::High,
            "xhigh" => Effort::Xhigh,
            "max" => Effort::Max,
            _ => return None,
        })
    }
}

/// Whether the request asks for the model's reasoning to be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Thinking {
    /// Send no thinking setting (the model's default: adaptive where thinking is always on).
    #[default]
    Default,
    /// Adaptive thinking with summaries shown (Anthropic `display: "summarized"`).
    Summarized,
}

/// Why a reply ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    /// The provider paused a long turn; sending the conversation again continues it.
    PauseTurn,
    /// The model declined (a safety classifier). `explanation` is the provider's, when given.
    Refusal {
        explanation: Option<String>,
    },
    Other {
        reason: String,
    },
}

impl StopReason {
    pub fn from_anthropic(s: &str, explanation: Option<String>) -> Self {
        match s {
            "end_turn" => StopReason::EndTurn,
            "tool_use" => StopReason::ToolUse,
            "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
            "stop_sequence" => StopReason::StopSequence,
            "pause_turn" => StopReason::PauseTurn,
            "refusal" => StopReason::Refusal { explanation },
            other => StopReason::Other { reason: other.to_string() },
        }
    }
}

/// Tokens one request used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens not read from or written to a cache.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens = self.input_tokens.saturating_add(o.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(o.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(o.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(o.cache_write_tokens);
    }

    /// Every input token, cached or not.
    pub fn total_input(&self) -> u64 {
        self.input_tokens.saturating_add(self.cache_read_tokens).saturating_add(self.cache_write_tokens)
    }

    /// What these tokens cost at `price`, in US dollars.
    pub fn cost_usd(&self, price: &Price) -> f64 {
        let m = 1_000_000.0;
        self.input_tokens as f64 / m * price.input
            + self.output_tokens as f64 / m * price.output
            + self.cache_read_tokens as f64 / m * price.cache_read
            + self.cache_write_tokens as f64 / m * price.cache_write
    }
}

/// US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// A 5-minute cache write.
    pub cache_write: f64,
}

/// The list price of a model this crate knows (Anthropic first-party rates); `None` for others.
pub fn price(model: &str) -> Option<Price> {
    let p = |input: f64, output: f64, cache_read: f64| Price { input, output, cache_read, cache_write: input * 1.25 };
    Some(match model {
        "claude-opus-5-5" => p(4.0, 20.0, 0.20),
        "claude-sonnet-5-5" => p(2.0, 10.0, 0.20),
        "claude-haiku-5-5" => p(0.10, 0.50, 0.01),
        "claude-fable-5-1" | "claude-fable-5" => p(10.0, 50.0, 1.0),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => p(5.0, 25.0, 0.50),
        "claude-sonnet-5" => p(2.0, 10.0, 0.20),
        "claude-sonnet-4-6" => p(3.0, 15.0, 0.30),
        "claude-haiku-4-5" => p(1.0, 5.0, 0.10),
        _ => return None,
    })
}

/// What a provider reports while a reply streams in.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ThinkingDelta(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    /// A piece of the arguments' JSON text.
    ToolCallDelta {
        id: String,
        json: String,
    },
    ToolCallEnd {
        id: String,
    },
    /// The usage so far (each report replaces the last).
    Usage(Usage),
    Stop(StopReason),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unanswered_calls_are_the_last_assistant_turns_without_results() {
        let mut c = Conversation::new();
        c.push_user_text("hi");
        let call = |id: &str| Block::ToolCall(ToolCall { id: id.into(), name: "t".into(), input: json!({}), invalid_input: None });
        c.push(Message { role: Role::Assistant, blocks: vec![call("a"), call("b")], native: None });
        assert_eq!(c.unanswered_calls().len(), 2);
        c.push(Message::tool_results(vec![ToolResult { call_id: "a".into(), content: "ok".into(), is_error: false }]));
        assert_eq!(c.unanswered_calls().iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn cost_of_a_turn() {
        let u = Usage { input_tokens: 1_000_000, output_tokens: 100_000, cache_read_tokens: 2_000_000, cache_write_tokens: 0 };
        let c = u.cost_usd(&price("claude-opus-5-5").unwrap_or(Price { input: 0.0, output: 0.0, cache_read: 0.0, cache_write: 0.0 }));
        assert!((c - (4.0 + 2.0 + 0.4)).abs() < 1e-9);
        assert!(price("some-local-model").is_none());
    }
}
