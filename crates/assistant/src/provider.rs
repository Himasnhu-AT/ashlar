//! The provider seam: one streamed request, from the neutral model to a wire format and back.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::Error;
use crate::types::{Effort, Message, StopReason, StreamEvent, Thinking, ToolSpec, Usage};

/// One request to a model.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub model: &'a str,
    /// Kept byte-for-byte stable across a conversation so the provider can cache it.
    pub system: &'a str,
    /// In a fixed order, for the same reason.
    pub tools: &'a [ToolSpec],
    pub messages: &'a [Message],
    pub max_tokens: u32,
    pub effort: Option<Effort>,
    pub thinking: Thinking,
}

/// A finished reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// The assistant message, with its native content for replay.
    pub message: Message,
    pub stop: StopReason,
    pub usage: Usage,
    /// The model that answered (a fallback model may answer for the one asked).
    pub model: String,
    /// The output limit a server lowered this request to (an account backend's
    /// `x-ai-max-tokens`, when the account's allowance can't cover the one asked); a reply that
    /// then stops at `max_tokens` was shortened by it.
    pub max_tokens_cap: Option<u32>,
}

/// Stops a request or a turn. Clones share the flag.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A model behind some API. Implementations stream the reply, calling `events` as pieces arrive,
/// return the assembled message, and give up with [`Error::Cancelled`] soon after `cancel` is set.
pub trait Provider: Send {
    /// A short id (`anthropic`, `openai`, `account`, `mock`).
    fn id(&self) -> &str;

    fn send(&mut self, request: &Request<'_>, events: &mut dyn FnMut(StreamEvent), cancel: &Cancel) -> Result<Response, Error>;
}
