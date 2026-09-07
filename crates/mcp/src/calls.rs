//! What an app's own tools and resources work with: [`ToolCx`] (the backend, progress and
//! cancellation of the call) and [`ToolOutcome`] (a result now, or a [`PendingCall`] the server
//! polls while it answers other requests).

use std::sync::mpsc::Sender;

use ashlar_control::{AppInfo, Reply};
use serde_json::{Value, json};

use crate::backend::Backend;
use crate::tools::ToolResult;

/// The context of one tool call or resource read.
pub struct ToolCx<'a> {
    backend: &'a mut Backend,
    app: &'a AppInfo,
    notify: Option<&'a Sender<String>>,
    token: Option<&'a Value>,
    cancelled: bool,
    last: f64,
}

impl<'a> ToolCx<'a> {
    pub(crate) fn new(
        backend: &'a mut Backend,
        app: &'a AppInfo,
        notify: Option<&'a Sender<String>>,
        token: Option<&'a Value>,
        cancelled: bool,
    ) -> Self {
        Self { backend, app, notify, token, cancelled, last: f64::NEG_INFINITY }
    }

    /// Call a control method of the backend (`engine.execute`, `ui.screenshot`, `jobs.list`…).
    pub fn call(&mut self, method: &str, params: Value) -> Reply {
        self.backend.call(method, params, self.app)
    }

    /// Run a command: `engine.execute {command, params}`.
    pub fn execute(&mut self, command: &str, params: Value) -> Reply {
        self.call("engine.execute", json!({"command": command, "params": params}))
    }

    /// The backend itself (an in-process session, a running app or a worker process).
    pub fn backend(&mut self) -> &mut Backend {
        self.backend
    }

    /// Whether the client cancelled this call. A pending call is also told through
    /// [`PendingCall::cancel`].
    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Whether the client asked for progress (the call carried a `progressToken`).
    pub fn wants_progress(&self) -> bool {
        self.token.is_some() && self.notify.is_some()
    }

    /// Send `notifications/progress` when the client asked for it. Progress only goes up: a
    /// value at or below the last one is not sent. `total` and `message` are optional.
    pub fn progress(&mut self, done: f64, total: Option<f64>, message: &str) {
        let (Some(token), Some(tx)) = (self.token, self.notify) else { return };
        if !done.is_finite() || done <= self.last {
            return;
        }
        self.last = done;
        let mut p = json!({"progressToken": token, "progress": done});
        if let Some(t) = total.filter(|t| t.is_finite() && *t > 0.0) {
            p["total"] = json!(t);
        }
        if !message.is_empty() {
            p["message"] = json!(message);
        }
        let _ = tx.send(json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": p}).to_string());
    }

    pub(crate) fn last_progress(&self) -> f64 {
        self.last
    }

    pub(crate) fn set_last_progress(&mut self, v: f64) {
        self.last = v;
    }
}

/// A tool call that finishes later (an export that runs as a job). The server polls it about
/// every 100 ms while it keeps answering other requests; the response goes out when `poll`
/// returns a result.
pub trait PendingCall {
    /// Check on the call: `Some` when it is done.
    fn poll(&mut self, cx: &mut ToolCx<'_>) -> Option<ToolResult>;

    /// The client cancelled it: stop the work and clean up (no response is sent).
    fn cancel(&mut self, cx: &mut ToolCx<'_>) {
        let _ = cx;
    }
}

/// What a tool handler returns.
pub enum ToolOutcome {
    Done(ToolResult),
    Pending(Box<dyn PendingCall>),
}

impl From<ToolResult> for ToolOutcome {
    fn from(r: ToolResult) -> Self {
        ToolOutcome::Done(r)
    }
}

impl ToolOutcome {
    /// A call the server polls until it is done.
    pub fn pending(call: impl PendingCall + 'static) -> Self {
        ToolOutcome::Pending(Box::new(call))
    }
}
