//! The seam between the two halves of the SDK.
//!
//! The pipeline (queue, spool, transport, session, consent) implements [`Sink`]. The signal modules
//! (crash, hang, perf, sys, logs) only ever talk to a `dyn Sink`, so they can be tested with
//! `testing::MemorySink` and never touch the network or the spool directly.

use std::path::Path;

use tracer_proto::{ItemBody, Signal};

/// The `log` target of the few lines the SDK itself logs, to tell a host why something is not happening.
/// The SDK's own logger passes these on but never keeps them as breadcrumbs.
pub(crate) const LOG_TARGET: &str = "tracer_sdk";

pub(crate) trait Sink: Send + Sync + 'static {
    /// Queues an item for the current session. Never blocks; drops and counts when the queue is full.
    /// Does nothing when the item's signal is not allowed.
    fn submit(&self, signal: Signal, body: ItemBody);

    /// Writes an item to disk on the calling thread before returning. For the crash path, where the
    /// process may not live long enough for the worker thread to run. Must not panic.
    fn submit_now(&self, signal: Signal, body: ItemBody);

    /// True when the host enabled this signal and the end user has not switched it off.
    fn allows(&self, signal: Signal) -> bool;

    /// The directory of this process's session, inside the SDK's own directory. Files a signal module
    /// saves for a later launch to find, such as recent log lines, go here: if this process dies,
    /// whichever process adopts the directory gets them together with the rest of the session. The SDK's
    /// directory itself is shared by every process of the host, so nothing per-process belongs there.
    fn session_dir(&self) -> &Path;

    /// A few plain `key: value` lines about the install and the running session (ids, start time, running
    /// totals), for the local report.
    fn session_summary(&self) -> String;

    /// Records that something was thrown away, for the `drops` item.
    fn count_drop(&self, reason: &'static str);

    /// Records that a panic was seen, so the session ends as `crashed` unless the process exits cleanly.
    fn mark_panicked(&self);

    /// Records one handled error against the session.
    fn count_error(&self);
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use tracer_proto::{ItemBody, Signal};

    use super::Sink;

    /// A sink that keeps everything in memory. `denied` lists signals to treat as switched off.
    pub(crate) struct MemorySink {
        pub items: Mutex<Vec<(Signal, ItemBody, bool)>>,
        pub drops: Mutex<Vec<&'static str>>,
        pub denied: Mutex<Vec<Signal>>,
        pub panicked: AtomicBool,
        pub errors: AtomicU32,
        pub dir: PathBuf,
    }

    impl MemorySink {
        pub(crate) fn new(dir: impl Into<PathBuf>) -> Self {
            Self {
                items: Mutex::new(Vec::new()),
                drops: Mutex::new(Vec::new()),
                denied: Mutex::new(Vec::new()),
                panicked: AtomicBool::new(false),
                errors: AtomicU32::new(0),
                dir: dir.into(),
            }
        }

        pub(crate) fn bodies(&self) -> Vec<ItemBody> {
            self.items.lock().map(|items| items.iter().map(|(_, body, _)| body.clone()).collect()).unwrap_or_default()
        }

        fn push(&self, signal: Signal, body: ItemBody, now: bool) {
            if self.allows(signal)
                && let Ok(mut items) = self.items.lock()
            {
                items.push((signal, body, now));
            }
        }
    }

    impl Sink for MemorySink {
        fn submit(&self, signal: Signal, body: ItemBody) {
            self.push(signal, body, false);
        }

        fn submit_now(&self, signal: Signal, body: ItemBody) {
            self.push(signal, body, true);
        }

        fn allows(&self, signal: Signal) -> bool {
            self.denied.lock().map(|denied| !denied.contains(&signal)).unwrap_or(false)
        }

        fn session_dir(&self) -> &Path {
            &self.dir
        }

        fn session_summary(&self) -> String {
            "install_id: test-install\nsession_id: test-session\n".to_owned()
        }

        fn count_drop(&self, reason: &'static str) {
            if let Ok(mut drops) = self.drops.lock() {
                drops.push(reason);
            }
        }

        fn mark_panicked(&self) {
            self.panicked.store(true, Ordering::Relaxed);
        }

        fn count_error(&self) {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}
