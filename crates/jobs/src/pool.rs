//! A bounded pool of worker threads (or rayon's global pool with the `rayon` feature).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};

pub(crate) type Task = Box<dyn FnOnce() + Send>;

#[derive(Default)]
#[cfg_attr(feature = "rayon", allow(dead_code))]
struct Shared {
    queue: Mutex<Queue>,
    cv: Condvar,
    shutdown: AtomicBool,
}

#[derive(Default)]
#[cfg_attr(feature = "rayon", allow(dead_code))]
struct Queue {
    tasks: VecDeque<Task>,
    idle: usize,
}

#[cfg_attr(feature = "rayon", allow(dead_code))]
pub(crate) struct Pool {
    shared: Arc<Shared>,
    threads: usize,
    max: usize,
}

impl Pool {
    pub(crate) fn new(max: usize) -> Self {
        Self { shared: Arc::default(), threads: 0, max: max.max(1) }
    }

    pub(crate) fn workers(&self) -> usize {
        if cfg!(feature = "rayon") { rayon_threads() } else { self.max }
    }

    /// Run `task` on a worker. Threads are started on demand up to the bound; if no thread can be
    /// started at all, the task runs inline (late is better than never).
    pub(crate) fn submit(&mut self, task: Task) {
        #[cfg(feature = "rayon")]
        {
            rayon::spawn(task);
        }
        #[cfg(not(feature = "rayon"))]
        {
            let need_thread = {
                let mut q = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
                q.tasks.push_back(task);
                q.tasks.len() > q.idle && self.threads < self.max
            };
            if need_thread {
                let shared = self.shared.clone();
                let n = self.threads;
                match std::thread::Builder::new().name(format!("ashlar-job-{n}")).spawn(move || worker(&shared)) {
                    Ok(_) => self.threads += 1,
                    Err(e) => {
                        log::warn!("could not start a job thread: {e}");
                        if self.threads == 0 {
                            let task = self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner).tasks.pop_back();
                            if let Some(task) = task {
                                task();
                            }
                            return;
                        }
                    }
                }
            }
            self.shared.cv.notify_one();
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // Workers finish the task they are on (jobs are cancelled by `Jobs`) and exit; queued
        // tasks are dropped. Nothing is joined: dropping never blocks the UI thread.
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner).tasks.clear();
        self.shared.cv.notify_all();
    }
}

#[cfg_attr(feature = "rayon", allow(dead_code))]
fn worker(shared: &Shared) {
    loop {
        let task = {
            let mut q = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if shared.shutdown.load(Ordering::Acquire) {
                    return;
                }
                if let Some(t) = q.tasks.pop_front() {
                    break t;
                }
                q.idle += 1;
                q = shared.cv.wait(q).unwrap_or_else(PoisonError::into_inner);
                q.idle = q.idle.saturating_sub(1);
            }
        };
        // tasks catch their own panics; this keeps the worker alive if one does not
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(task));
    }
}

#[cfg(feature = "rayon")]
fn rayon_threads() -> usize {
    rayon::current_num_threads()
}

#[cfg(not(feature = "rayon"))]
fn rayon_threads() -> usize {
    0
}
