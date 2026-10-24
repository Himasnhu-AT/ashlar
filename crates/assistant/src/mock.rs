//! A scripted provider for tests (the crate's and apps'): each request takes the next reply from
//! a script, streams it as a real provider would, and is recorded for assertions.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use crate::error::Error;
use crate::provider::{Cancel, Provider, Request, Response};
use crate::types::{Block, Message, Role, StopReason, StreamEvent, ToolCall, Usage};

/// One scripted reply.
#[derive(Debug, Clone)]
pub enum MockReply {
    /// A reply with these blocks (text and tool calls) and stop reason.
    Reply {
        blocks: Vec<Block>,
        stop: StopReason,
        usage: Usage,
    },
    Fail(Error),
    /// Stream "…" until cancelled (tests of Stop).
    Hang,
    /// A reply the server cut short: it lowered the output limit to `cap` and the reply stopped
    /// at `max_tokens`.
    CutByServer {
        text: String,
        cap: u32,
    },
}

impl MockReply {
    pub fn text(t: impl Into<String>) -> Self {
        MockReply::Reply {
            blocks: vec![Block::Text { text: t.into() }],
            stop: StopReason::EndTurn,
            usage: Usage { input_tokens: 100, output_tokens: 20, ..Usage::default() },
        }
    }

    /// One tool call (with an optional sentence before it).
    pub fn tool(id: &str, name: &str, input: Value, text: Option<&str>) -> Self {
        let mut blocks = Vec::new();
        if let Some(t) = text {
            blocks.push(Block::Text { text: t.into() });
        }
        blocks.push(Block::ToolCall(ToolCall { id: id.into(), name: name.into(), input, invalid_input: None }));
        MockReply::Reply { blocks, stop: StopReason::ToolUse, usage: Usage { input_tokens: 100, output_tokens: 30, ..Usage::default() } }
    }

    /// Several calls in one reply.
    pub fn tools(calls: &[(&str, &str, Value)]) -> Self {
        let blocks = calls
            .iter()
            .map(|(id, name, input)| Block::ToolCall(ToolCall { id: (*id).into(), name: (*name).into(), input: input.clone(), invalid_input: None }))
            .collect();
        MockReply::Reply { blocks, stop: StopReason::ToolUse, usage: Usage { input_tokens: 100, output_tokens: 30, ..Usage::default() } }
    }
}

/// What a request carried.
#[derive(Debug, Clone, PartialEq)]
pub struct Recorded {
    pub model: String,
    pub system: String,
    pub tools: Vec<String>,
    pub messages: Vec<Message>,
}

/// The scripted provider. Clones share the script and the record.
#[derive(Clone, Default)]
pub struct MockProvider {
    script: Arc<Mutex<VecDeque<MockReply>>>,
    record: Arc<Mutex<Vec<Recorded>>>,
    /// Pause between streamed pieces.
    pub delay: Duration,
}

impl MockProvider {
    pub fn new(replies: impl IntoIterator<Item = MockReply>) -> Self {
        Self { script: Arc::new(Mutex::new(replies.into_iter().collect())), ..Self::default() }
    }

    /// Add replies to the end of the script.
    pub fn push(&self, r: MockReply) {
        self.script.lock().unwrap_or_else(PoisonError::into_inner).push_back(r);
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Provider for MockProvider {
    fn id(&self) -> &str {
        "mock"
    }

    fn send(&mut self, r: &Request<'_>, events: &mut dyn FnMut(StreamEvent), cancel: &Cancel) -> Result<Response, Error> {
        self.record.lock().unwrap_or_else(PoisonError::into_inner).push(Recorded {
            model: r.model.into(),
            system: r.system.into(),
            tools: r.tools.iter().map(|t| t.name.clone()).collect(),
            messages: r.messages.to_vec(),
        });
        let next = self.script.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
        let pause = |d: Duration| {
            if !d.is_zero() {
                std::thread::sleep(d);
            }
        };
        match next {
            None => Err(Error::Other("the mock provider has no more replies".into())),
            Some(MockReply::Fail(e)) => Err(e),
            Some(MockReply::CutByServer { text, cap }) => {
                events(StreamEvent::TextDelta(text.clone()));
                let usage = Usage { input_tokens: 100, output_tokens: u64::from(cap), ..Usage::default() };
                let message = Message { role: Role::Assistant, blocks: vec![Block::Text { text }], native: None };
                Ok(Response { message, stop: StopReason::MaxTokens, usage, model: r.model.into(), max_tokens_cap: Some(cap) })
            }
            Some(MockReply::Hang) => loop {
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                events(StreamEvent::TextDelta("…".into()));
                std::thread::sleep(Duration::from_millis(20));
            },
            Some(MockReply::Reply { blocks, stop, usage }) => {
                for b in &blocks {
                    if cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    match b {
                        Block::Text { text } => {
                            for w in text.split_inclusive(' ') {
                                events(StreamEvent::TextDelta(w.into()));
                                pause(self.delay);
                            }
                        }
                        Block::Thinking { text } => events(StreamEvent::ThinkingDelta(text.clone())),
                        Block::ToolCall(c) => {
                            events(StreamEvent::ToolCallStart { id: c.id.clone(), name: c.name.clone() });
                            events(StreamEvent::ToolCallDelta { id: c.id.clone(), json: c.input.to_string() });
                            events(StreamEvent::ToolCallEnd { id: c.id.clone() });
                            pause(self.delay);
                        }
                        Block::ToolResult(_) => {}
                    }
                }
                events(StreamEvent::Usage(usage));
                events(StreamEvent::Stop(stop.clone()));
                Ok(Response {
                    message: Message { role: Role::Assistant, blocks, native: None },
                    stop,
                    usage,
                    model: r.model.into(),
                    max_tokens_cap: None,
                })
            }
        }
    }
}
