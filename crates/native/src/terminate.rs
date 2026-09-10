//! Run a hook when the process is asked to stop, then stop as asked. On Unix that is SIGINT
//! (Ctrl-C), SIGTERM and SIGHUP: the first one runs the hook on a thread of its own, after which
//! the signal is delivered again with its default action, so the process ends the way it would
//! have (a shell sees "killed by SIGTERM"). A second signal while the hook runs ends the process at
//! once. For a long-running command-line process (a stdio server) that must write something down
//! before it goes. Always compiled; on other systems [`on_terminate`] does nothing.
//!
//! The signal handler only writes the signal's number to a pipe (async-signal-safe); a thread
//! reads it and runs the hook. Signal masks are not touched, so child processes start as usual.

use std::sync::{Mutex, PoisonError};

/// Called once, with the signal's number, on its own thread.
pub type TerminateHook = Box<dyn FnOnce(i32) + Send>;

static HOOK: Mutex<Option<TerminateHook>> = Mutex::new(None);

/// Install `hook` (replacing an earlier one that has not run). True when the signals are watched:
/// on Unix, once the pipe and the watcher thread exist; false elsewhere.
pub fn on_terminate(hook: impl FnOnce(i32) + Send + 'static) -> bool {
    *HOOK.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(hook));
    imp::watch()
}

/// Remove the hook if it has not run (the signals then end the process as before).
pub fn clear() {
    *HOOK.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

fn run_hook(signal: i32) {
    let hook = HOOK.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(hook) = hook {
        hook(signal);
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
mod imp {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    const SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP];

    static WRITE_END: AtomicI32 = AtomicI32::new(-1);
    static FIRED: AtomicBool = AtomicBool::new(false);
    static WATCHING: OnceLock<bool> = OnceLock::new();

    /// The calling thread's errno. A handler that calls write() must put errno back, or the code
    /// it interrupted may read a value it didn't cause.
    fn errno_ptr() -> *mut libc::c_int {
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
        // SAFETY: returns the calling thread's errno location; always valid.
        unsafe {
            libc::__error()
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
        // SAFETY: returns the calling thread's errno location; always valid.
        unsafe {
            libc::__errno_location()
        }
    }

    extern "C" fn handler(signal: libc::c_int) {
        // SAFETY: reading and writing this thread's errno is async-signal-safe.
        let saved = unsafe { *errno_ptr() };
        handle(signal);
        // SAFETY: as above.
        unsafe { *errno_ptr() = saved };
    }

    fn handle(signal: libc::c_int) {
        if FIRED.swap(true, Ordering::SeqCst) {
            // a second request while the hook runs: stop now
            // SAFETY: signal() and raise() are async-signal-safe; SIG_DFL is a valid disposition.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
            return;
        }
        let byte = signal as u8;
        // SAFETY: write() is async-signal-safe; the buffer is one valid byte on this stack. A
        // failed write (no pipe) is ignored: the watcher never runs and nothing is stopped twice.
        unsafe {
            libc::write(WRITE_END.load(Ordering::SeqCst), (&raw const byte).cast(), 1);
        }
    }

    pub(super) fn watch() -> bool {
        *WATCHING.get_or_init(start)
    }

    fn start() -> bool {
        let mut fds = [-1 as libc::c_int; 2];
        // SAFETY: `fds` is a valid array of two ints for pipe() to fill.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return false;
        }
        for fd in fds {
            // SAFETY: fd is an open descriptor this function just created.
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
        }
        WRITE_END.store(fds[1], Ordering::SeqCst);
        let read_end = fds[0];
        let spawned = std::thread::Builder::new().name("terminate-watch".into()).spawn(move || {
            let mut byte = 0u8;
            loop {
                // SAFETY: reads at most one byte into `byte`, a valid buffer, from an open descriptor.
                let n = unsafe { libc::read(read_end, (&raw mut byte).cast(), 1) };
                if n == 1 {
                    break;
                }
                if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            let signal = libc::c_int::from(byte);
            super::run_hook(signal);
            // then end as the signal would have
            // SAFETY: restoring the default disposition and raising a standard signal.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
        });
        if spawned.is_err() {
            return false;
        }
        for signal in SIGNALS {
            // SAFETY: a zeroed sigaction with a valid handler, an empty mask and SA_RESTART is a
            // valid argument; the previous action is not needed.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&raw mut action.sa_mask);
                libc::sigaction(signal, &raw const action, std::ptr::null_mut());
            }
        }
        true
    }
}

#[cfg(not(unix))]
mod imp {
    pub(super) fn watch() -> bool {
        false
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn a_hook_runs_once_and_can_be_cleared() {
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
        let r = ran.clone();
        assert!(super::on_terminate(move |sig| r.store(sig, std::sync::atomic::Ordering::SeqCst)));
        super::run_hook(15);
        super::run_hook(2);
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 15);
        super::on_terminate(|_| panic!("cleared"));
        super::clear();
        super::run_hook(15);
    }
}
