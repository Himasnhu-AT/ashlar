//! Queue, spool, session, consent and sending. Implements [`Sink`] for the signal modules.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tracer_proto::{ConsentCategory, ItemBody, Signal};

use crate::config::Config;
use crate::signals::Signals;
use crate::sink::Sink;
use crate::transport::Transport;

pub(crate) struct Pipeline {
    dir: PathBuf,
}

impl Pipeline {
    /// Starts the worker thread and opens the session. `transport` overrides the built-in HTTP client.
    pub(crate) fn start(config: &Config, _transport: Option<Box<dyn Transport>>) -> Arc<Self> {
        Arc::new(Self { dir: config.data_dir.join("tracer") })
    }

    /// Hands the pipeline the signal modules so its worker can call [`Signals::tick`] about once a second.
    pub(crate) fn attach(&self, _signals: Arc<Signals>) {}

    pub(crate) fn track(&self, _name: &'static str, _category: &'static str, _dynamic: bool, _props: &[(&'static str, crate::Prop)]) {}

    pub(crate) fn set_focused(&self, _focused: bool) {}

    pub(crate) fn note_input(&self) {}

    pub(crate) fn consent(&self, _category: ConsentCategory) -> bool {
        true
    }

    pub(crate) fn set_consent(&self, _category: ConsentCategory, _allowed: bool) {}

    pub(crate) fn notice_pending(&self) -> bool {
        false
    }

    pub(crate) fn notice_shown(&self) {}

    pub(crate) fn forget(&self) {}

    pub(crate) fn install_id(&self) -> String {
        String::new()
    }

    pub(crate) fn session_id(&self) -> String {
        String::new()
    }

    /// Sends what is waiting and returns when done or when `timeout` passes. True when nothing is left.
    pub(crate) fn flush_blocking(&self, _timeout: Duration) -> bool {
        true
    }

    /// Ends the session and stops the worker. Called once, from the last [`crate::Tracer`] guard.
    pub(crate) fn shutdown(&self) {}
}

impl Sink for Pipeline {
    fn submit(&self, _signal: Signal, _body: ItemBody) {}

    fn submit_now(&self, _signal: Signal, _body: ItemBody) {}

    fn allows(&self, _signal: Signal) -> bool {
        false
    }

    fn data_dir(&self) -> &Path {
        &self.dir
    }

    fn count_drop(&self, _reason: &'static str) {}

    fn mark_panicked(&self) {}

    fn count_error(&self) {}
}
