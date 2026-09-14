//! An MCP server (Model Context Protocol, JSON-RPC 2.0 over stdio) generated from an app's
//! commands, for any backend:
//!
//! - [`Backend::Headless`]: an in-process session (any [`ashlar_command::Dispatch`]);
//! - [`Backend::Remote`]: a running app through its control port (adds screenshots and UI input);
//!   [`Backend::App`] the same, connected on first use so the server can start before the app;
//! - [`Backend::Subprocess`]: a child process serving the control protocol on stdio, so a command
//!   that crashes takes down the child, not the server.
//!
//! Tools are one fixed vocabulary (`list_commands`, `describe_command`, `execute`, `batch`, plus
//! `screenshot` and `ui_*` when the backend answers them), optionally one tool per command
//! ([`ToolMode::PerCommand`]). See `docs/control-protocol.md`.
//!
//! ```no_run
//! # use ashlar_mcp::{Backend, McpServer, ServerOptions};
//! # fn session() -> Box<dyn ashlar_command::Dispatch> { unimplemented!() }
//! let opts = ServerOptions::new("notes", "0.1.0").instructions("A note-taking app.");
//! let mut server = McpServer::new(Backend::Headless(session()), opts);
//! server.serve(std::io::BufReader::new(std::io::stdin()), std::io::stdout())?;
//! # Ok::<(), std::io::Error>(())
//! ```
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod backend;
mod calls;
pub mod docs;
mod server;
pub mod tools;

pub use backend::{AppPort, Backend, Subprocess};
pub use calls::{PendingCall, ToolCx, ToolOutcome};
pub use docs::Library;
pub use server::{LATEST_PROTOCOL_VERSION, MAX_MESSAGE, McpServer, PROTOCOL_VERSION, SUPPORTED_VERSIONS, ServerOptions};
pub use tools::{Access, ToolMode, ToolResult};
