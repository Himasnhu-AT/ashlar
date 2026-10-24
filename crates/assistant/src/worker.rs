//! A turn on a background thread, for a UI: events come back through [`TurnHandle::poll`] (with
//! a wake-up call so an idle UI repaints), approvals go in through [`TurnHandle::answer`], and
//! [`TurnHandle::cancel`] stops it. The tools are made on the worker thread, so a tool host need
//! not be `Send`.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use crate::agent::{AgentConfig, AgentEvent, Approval, Observer, ToolHost, TurnEnd, TurnOutcome, run_turn};
use crate::error::Error;
use crate::provider::{Cancel, Provider};
use crate::types::{Conversation, ToolCall, Usage};

/// What the worker sends back.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerEvent {
    Agent(AgentEvent),
    /// The turn ended: the conversation with this turn's messages added, and how it went.
    Finished {
        conversation: Conversation,
        outcome: TurnOutcome,
    },
}

/// Called after each event is queued (repaint the UI).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A running turn.
pub struct TurnHandle {
    rx: Receiver<WorkerEvent>,
    approvals: Sender<(String, Approval)>,
    cancel: Cancel,
    finished: bool,
}

impl TurnHandle {
    /// The events queued since the last poll.
    pub fn poll(&mut self) -> Vec<WorkerEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.rx.try_recv() {
            if matches!(e, WorkerEvent::Finished { .. }) {
                self.finished = true;
            }
            out.push(e);
        }
        out
    }

    /// Wait up to `timeout` for the next event (tests and scripts).
    pub fn wait(&mut self, timeout: Duration) -> Option<WorkerEvent> {
        let e = self.rx.recv_timeout(timeout).ok()?;
        if matches!(e, WorkerEvent::Finished { .. }) {
            self.finished = true;
        }
        Some(e)
    }

    /// Answer the approval request for `call_id`.
    pub fn answer(&self, call_id: &str, approval: Approval) {
        let _ = self.approvals.send((call_id.to_string(), approval));
    }

    /// Stop: the request in flight is dropped, pending approvals are denied, tools not yet run
    /// get "stopped" results.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Whether `Finished` has been polled.
    pub fn is_finished(&self) -> bool {
        self.finished
    }
}

struct Forward {
    tx: Sender<WorkerEvent>,
    approvals: Receiver<(String, Approval)>,
    wake: Wake,
}

impl Forward {
    fn send(&self, e: WorkerEvent) {
        let _ = self.tx.send(e);
        (self.wake)();
    }
}

impl Observer for Forward {
    fn event(&mut self, e: AgentEvent) {
        self.send(WorkerEvent::Agent(e));
    }

    fn approve(&mut self, call: &ToolCall, _reason: &str, cancel: &Cancel) -> Approval {
        loop {
            if cancel.is_cancelled() {
                return Approval::Deny;
            }
            match self.approvals.recv_timeout(Duration::from_millis(100)) {
                Ok((id, a)) if id == call.id => return a,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Approval::Deny,
            }
        }
    }
}

/// Run one turn on a new thread. `conversation` must end with the user's message. A panic in the
/// provider or a tool ends the turn with an error instead of the app.
pub fn spawn_turn(
    mut provider: Box<dyn Provider>,
    make_tools: impl FnOnce() -> Box<dyn ToolHost> + Send + 'static,
    mut conversation: Conversation,
    config: AgentConfig,
    wake: Wake,
) -> TurnHandle {
    let (tx, rx) = channel();
    let (atx, arx) = channel();
    let cancel = Cancel::new();
    let c = cancel.clone();
    let backup = conversation.clone();
    let spawned = std::thread::Builder::new().name("assistant-turn".into()).spawn(move || {
        let mut fwd = Forward { tx, approvals: arx, wake };
        let before = conversation.clone();
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let mut tools = make_tools();
            run_turn(provider.as_mut(), tools.as_mut(), &mut conversation, &config, &mut fwd, &c)
        }));
        let (conversation, outcome) = match r {
            Ok(o) => (conversation, o),
            Err(_) => {
                log::error!(target: "assistant", "a turn panicked; the conversation is kept as it was before it");
                let o = TurnOutcome {
                    end: TurnEnd::Error(Error::Other("Something went wrong inside the assistant. Try again.".into())),
                    usage: Usage::default(),
                    cost_usd: None,
                    steps: 0,
                };
                (before, o)
            }
        };
        fwd.send(WorkerEvent::Finished { conversation, outcome });
    });
    let (rx, finished) = match spawned {
        Ok(_) => {
            drop(backup);
            (rx, false)
        }
        Err(e) => {
            // no thread: report it as the turn's end
            let (tx2, rx2) = channel();
            let o = TurnOutcome {
                end: TurnEnd::Error(Error::Other(format!("could not start the assistant: {e}"))),
                usage: Usage::default(),
                cost_usd: None,
                steps: 0,
            };
            let _ = tx2.send(WorkerEvent::Finished { conversation: backup, outcome: o });
            (rx2, false)
        }
    };
    TurnHandle { rx, approvals: atx, cancel, finished }
}
