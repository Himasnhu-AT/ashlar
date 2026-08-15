//! Tracer server: ingest API, storage, rollups, dashboard and reports in one binary.
//!
//! The binary is a thin wrapper over this library so integration tests and the dashboard can call in.
//!
//! - [`config`]: settings from `TRACER_*` environment variables.
//! - [`store`]: the [`store::Store`] trait and its SQLite implementation.
//! - [`ingest`]: pure validation, drift correction and fingerprinting.
//! - [`http`]: the axum router ([`http::router`], [`http::api_router`]), shared [`http::AppState`], rate limiter.
//! - [`jobs`]: rollups, retention, stale-session closing.
//! - [`cli`](crate::cli): subcommands.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod cli;
pub mod config;
pub mod http;
pub mod ingest;
pub mod jobs;
pub mod logging;
pub mod store;

pub use store::sqlite::MIGRATIONS;

/// The server clock in Unix milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}
