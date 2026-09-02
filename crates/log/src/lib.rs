//! Headless logging and crash safety, for windowed apps and command-line tools alike.
//!
//! | Module | |
//! |---|---|
//! | [`filter`] | `env_logger`-style directives (`warn,my_crate=debug,wgpu=off`), longest-prefix match |
//! | [`logging`] | one `log` backend: stderr (optional) + rotating file + in-memory ring |
//! | [`crash`] | panic hook (chains to the previous one), crash logs, last panic, launch guard |
//!
//! Nothing here ever writes to stdout, so a CLI that prints results there, or an MCP server that
//! speaks JSON-RPC on it, can log freely. Depends only on `log` and std; builds for
//! `wasm32-unknown-unknown` (no file or thread there: the ring and an extra output still work).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod crash;
pub mod filter;
pub mod logging;

pub use filter::{Filter, filter_from_env};
pub use logging::{LogLine, Logger};

/// Seconds since 1970 (0 on the web, where the clock is not available to std).
pub(crate) fn unix_now() -> f64 {
    #[cfg(not(target_arch = "wasm32"))]
    return std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    #[cfg(target_arch = "wasm32")]
    0.0
}
