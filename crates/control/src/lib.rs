//! Agent control port: a hardened loopback JSON-lines server, its client, and the standard method
//! set every app answers.
//!
//! ```text
//!  agent / MCP / CLI ──TCP 127.0.0.1──▶ Server (connection threads) ──Request──▶ app thread
//!                                                                          │ handle(req, dispatch, ui, info)
//!                      ◀────────── {"id","ok","result"|"error"} ◀──────────┘
//! ```
//!
//! The server never touches app state: it parses, authenticates and queues [`Request`]s; the app
//! thread answers them with [`handle`] (standard `app.*`/`engine.*` methods from any
//! [`ashlar_command::Dispatch`], everything else from the app's [`UiHandler`]). The protocol is
//! documented in `docs/control-protocol.md`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod client;
mod explain;
pub mod key;
mod lines;
mod methods;
mod protocol;
mod server;

pub use client::{Client, MAX_REPLY};
pub use explain::{explain, explain_reply};
pub use lines::{DEFAULT_MAX_LINE, serve_stream};
pub use methods::{AppInfo, DEBUG_METHODS, MAX_BATCH, NoUi, STANDARD_METHODS, UI_METHODS, UiHandler, answer, handle, matches_query};
pub use protocol::{PROTOCOL_VERSION, Reply, Request};
pub use server::{Config, Server, Wake};

use serde_json::Value;

/// Why a control-port operation failed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ControlError {
    /// Binding, connecting, reading or writing failed.
    #[error("{0}")]
    Io(String),
    /// The key could not be made, written or read.
    #[error("control key: {0}")]
    Key(String),
    /// The app refused the key (it restarted with a new one, or this is someone else's port).
    #[error("unauthorized: the app refused the control key")]
    Unauthorized,
    /// The peer did not speak the protocol.
    #[error("bad reply from the app: {0}")]
    Protocol(String),
    /// The app answered with an error.
    #[error("{error}")]
    Remote { error: String, detail: Option<Value> },
}
