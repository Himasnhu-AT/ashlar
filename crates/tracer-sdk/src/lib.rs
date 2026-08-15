//! Tracer SDK: crash, performance and usage telemetry for desktop apps.
//!
//! The host decides what is tracked. It declares its events with [`events!`], picks what to listen to on
//! the [`Builder`], and feeds the SDK from its own loop. Nothing here depends on a UI toolkit or an async
//! runtime, and no call on a [`Tracer`] blocks on the network or writes to stdout.
//!
//! See `docs/integration.md` for a walkthrough and `docs/privacy.md` for every field that is collected.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// Skeleton only: remove once the pipeline and signal modules are filled in.
#![allow(dead_code)]

pub mod catalog;
mod client;
mod config;
mod pipeline;
mod signals;
mod sink;
pub mod transport;

pub use catalog::{Event, EventCategory, Prop};
pub use client::{Builder, Tracer};
pub use config::{Config, LoggerMode};
pub use signals::rt::{Counter, Gauge};
pub use tracer_proto as proto;
pub use tracer_proto::{ConsentCategory, Gpu, ProcessKind, Signal};
