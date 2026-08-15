//! The seam between the two halves of the SDK.
//!
//! The pipeline (queue, spool, transport, session, consent) implements [`Sink`]. The signal modules
//! (crash, hang, perf, sys, logs) only ever talk to a `dyn Sink`, so they can be tested with
//! [`testing::MemorySink`] and never touch the network or the spool directly.

use std::path::Path;

use tracer_proto::{ItemBody, Signal};

pub(crate) trait Sink: Send + Sync + 'static {
    /// Queues an item for the current session. Never blocks; drops and counts when the queue is full.
    /// Does nothing when the item's signal is not allowed.
    fn submit(&self, signal: Signal, body: ItemBody);

    /// Writes an item to disk on the calling thread before returning. For the crash path, where the
    /// process may not live long enough for the worker thread to run. Must not panic.
    fn submit_now(&self, signal: Signal, body: ItemBody);

    /// True when the host enabled this signal and the end user has not switched it off.
    fn allows(&self, signal: Signal) -> bool;

    /// The SDK's own directory inside the host's data directory.
    fn data_dir(&self) -> &Path;

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

        fn data_dir(&self) -> &Path {
            &self.dir
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
