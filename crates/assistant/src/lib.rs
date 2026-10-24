//! An in-app AI assistant harness that does not depend on one model or vendor.
//!
//! - [`types`]: the neutral model: [`Conversation`] (append-only), [`Message`] and its
//!   [`Block`]s (text, thinking, tool call, tool result; a provider's own content kept verbatim in
//!   [`Native`] for replay), [`ToolSpec`], [`StreamEvent`], [`Usage`] and [`price`].
//! - [`Provider`]: one streamed request. Three ship (feature `http`, desktop):
//!   [`AccountBackend`] (an app's own backend holding the provider key, Anthropic wire format,
//!   the user's access token from a [`TokenSource`]), [`AnthropicApi`] (the user's key) and
//!   [`OpenAiCompatible`] (`/chat/completions` at any base URL, key optional). [`MockProvider`]
//!   scripts replies for tests.
//! - [`run_turn`]: the agent loop over a [`ToolHost`] the app implements: send, stream, check
//!   the stop reason, validate arguments against the tool's schema, ask the [`Observer`] to
//!   approve the calls the host marks, run them, send every result back in one message, repeat
//!   until the model ends its turn, up to a step limit; cancellable; usage and cost per step.
//!   [`spawn_turn`] runs it on a thread for a UI.
//! - [`Settings`] (which provider and model, as an app saves it), [`build_provider`] and
//!   [`check_connection`] (a settings page's Test button, no tokens spent).
//! - [`keys`]: API keys in the OS keychain (feature `keychain`) or in memory; never printed.
//! - [`mcp::McpTools`] (feature `mcp`): an app's MCP server as the tool host, in process.
//!
//! Errors are [`Error`]s whose `Display` is a sentence for the user (sign-in needed, the plan
//! does not include AI, the budget is used up, rate limited with the wait, the key was refused,
//! offline…).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod account;
pub mod agent;
pub mod anthropic;
pub mod error;
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
mod http;
pub mod keys;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod mock;
pub mod openai;
pub mod provider;
pub mod schema;
pub mod settings;
pub mod sse;
#[cfg(feature = "testing")]
pub mod testing;
pub mod types;
pub mod worker;

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use account::AccountBackend;
pub use account::{AccountLimits, AccountModel, AccountStatus, TokenSource};
pub use agent::{AgentConfig, AgentEvent, Approval, CombinedTools, DenyAll, FnTools, Observer, ToolHost, ToolOutput, TurnEnd, TurnOutcome, run_turn};
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use anthropic::AnthropicApi;
pub use error::Error;
pub use keys::{ApiKey, KeyStore, MemoryKeys};
pub use mock::{MockProvider, MockReply};
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use openai::OpenAiCompatible;
pub use provider::{Cancel, Provider, Request, Response};
pub use settings::{AccountConfig, ProviderKind, Settings};
#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use settings::{build_provider, check_connection};
pub use types::{
    Block, Conversation, Effort, Message, Native, Role, StopReason, StreamEvent, Thinking, ToolCall, ToolResult, ToolSpec, Usage, price,
};
pub use worker::{TurnHandle, Wake, WorkerEvent, spawn_turn};
