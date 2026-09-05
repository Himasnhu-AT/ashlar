//! ashlar runtime: the [`App`] builder, the frame scheduler, crash guard, logging, telemetry and
//! agent control.
//!
//! An app declares its document type and [`Format`](ashlar_store::Format), its commands
//! (`fn(&mut Session<D>, &Value)`) and a view; [`App::run`] opens the window and gives it undo and
//! redo (one step per top-level command), save / open / autosave and crash recovery, menus, a
//! keymap editor and command palette, a crash guard and log files, the agent control port, and
//! telemetry when enabled. [`App::cli`] / [`cli_main`] serve the same commands as a CLI and MCP
//! server; [`App::headless`] and [`Driver`] run them in tests.
//!
//! | Module | |
//! |---|---|
//! | [`session`] | `Session<D>`: the state commands run on; the host glue (one undo step per command, failed commands change nothing) |
//! | [`runtime`] | `Runtime<D>`: one frame (control, jobs, keys, menus, view, dialogs, autosave), repaint policy, `Cx` for views |
//! | [`driver`] | `Driver`: a headless, virtual-clock event loop for tests and benches |
//! | [`crash`] | panic hook, crash logs, last panic, launch guard (from `ashlar-log`) |
//! | [`logging`] | stderr + rotating file + in-memory ring (`app.logs`), directive filters (from `ashlar-log`) |
//! | [`stats`] | frame-time p50/p95/max (`app.stats`) |
//! | [`wake`] | frame requests with a reason, frames and requests per reason (`app.stats` → `wakes`; from `ashlar-ui`) |
//! | [`telemetry`] | tracer-sdk hook points (feature `telemetry`) |
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod app;
mod builtins;
mod control;
pub mod driver;
#[cfg(all(feature = "native-menu", any(target_os = "macos", target_os = "windows")))]
mod native_menu;
#[cfg(not(target_arch = "wasm32"))]
mod offscreen;
pub mod runtime;
pub mod session;
pub mod stats;
pub mod telemetry;

/// Logging and crash safety live in the headless `ashlar-log` crate (a CLI can use them without
/// the window); re-exported here unchanged.
pub use ashlar_log::{crash, logging};
/// The frame scheduler lives in `ashlar-ui` (it needs only egui, so hosts without the runtime use
/// it too); re-exported here unchanged.
pub use ashlar_ui::wake;

pub use app::{App, Error, Headless, SetupFn, ViewFn, cli_main};
pub use driver::Driver;
pub use runtime::{Cx, FilePicker, Pick, Runtime};
pub use session::{Document, Effect, Session};
pub use stats::{FrameStats, StatsSummary};

/// Methods the windowed runtime answers on the control port besides the standard ones.
pub fn control_methods() -> Vec<String> {
    use ashlar_control::UiHandler;
    control::UiQueue::default().methods()
}
