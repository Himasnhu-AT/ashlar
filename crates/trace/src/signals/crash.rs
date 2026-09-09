//! Panics, handled errors and child process exits.

use std::cell::Cell;
use std::panic::PanicHookInfo;
use std::process::ExitStatus;
use std::sync::{Arc, Weak};

use ashlar_trace_proto::{CrashKind, ItemBody, Signal};

use super::Shared;
use super::scrub::{MAX_BACKTRACE, MAX_LABEL, MAX_MESSAGE};

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
thread_local! {
    /// Set by the test fixture around `Signals::start` when the test does not want the process-wide hook.
    pub(super) static SKIP_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// Longest source location, in bytes.
const MAX_LOCATION: usize = 256;

/// Installs a panic hook that reports the panic and then runs the hook that was set before. The hook holds
/// the shared state weakly, so once the `Signals` that installed it are gone it only chains.
pub(super) fn install_hook(shared: &Arc<Shared>) {
    // `take_hook` and `set_hook` panic when called from a panicking thread, and a panic while unwinding
    // aborts the process. A `Tracer` built from a destructor during unwinding must not do that: go without.
    if std::thread::panicking() {
        return;
    }
    #[cfg(test)]
    if SKIP_HOOK.with(Cell::get) {
        return;
    }
    let weak = Arc::downgrade(shared);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        on_panic(&weak, info);
        previous(info);
    }));
}

/// Marks the thread as inside the hook until dropped.
struct Reentry;

impl Reentry {
    fn enter() -> Option<Self> {
        let already = IN_HOOK.try_with(|flag| flag.replace(true)).unwrap_or(true);
        (!already).then_some(Self)
    }
}

impl Drop for Reentry {
    fn drop(&mut self) {
        let _ = IN_HOOK.try_with(|flag| flag.set(false));
    }
}

/// Must never panic and never wait for a lock: the panicking thread may hold one of ours.
fn on_panic(weak: &Weak<Shared>, info: &PanicHookInfo<'_>) {
    let Some(shared) = weak.upgrade() else { return };
    let Some(_guard) = Reentry::enter() else { return };
    if !shared.allowed(Signal::Crash) {
        return;
    }
    let scrubber = &shared.scrubber;
    let payload = info.payload();
    let message = payload.downcast_ref::<&str>().map(|text| (*text).to_owned()).or_else(|| payload.downcast_ref::<String>().cloned());

    let mut crash = shared.crash_base(CrashKind::Panic, false);
    crash.message = Some(scrubber.apply(message.as_deref().unwrap_or("panic with a non-text payload"), MAX_MESSAGE));
    crash.location = info.location().map(|at| scrubber.apply(&format!("{}:{}:{}", at.file(), at.line(), at.column()), MAX_LOCATION));
    crash.thread = Some(scrubber.apply(std::thread::current().name().unwrap_or("<unnamed>"), MAX_LABEL));
    crash.backtrace = Some(scrubber.apply(&std::backtrace::Backtrace::force_capture().to_string(), MAX_BACKTRACE));

    shared.remember_crash(&crash, false);
    shared.sink.submit_now(Signal::Crash, ItemBody::Crash(crash));
    // Counted now, as an error the session saw: whether the process survives it cannot be known here.
    // If it does not, the pipeline takes the one that ended the session off again. Counted before the
    // marker is written, so that the marker has it.
    shared.sink.count_error();
    shared.sink.mark_panicked();
}

/// Reports an error the host handled. Counts it against the session.
pub(super) fn report_handled(shared: &Shared, message: &str, context: Option<&'static str>) {
    if !shared.allowed(Signal::Crash) {
        return;
    }
    let mut crash = shared.crash_base(CrashKind::Handled, true);
    crash.message = Some(shared.scrubber.apply(message, MAX_MESSAGE));
    crash.context = context.map(str::to_owned);
    crash.thread = Some(shared.scrubber.apply(std::thread::current().name().unwrap_or("<unnamed>"), MAX_LABEL));
    shared.remember_crash(&crash, true);
    shared.sink.submit(Signal::Crash, ItemBody::Crash(crash));
    shared.sink.count_error();
}

/// Reports a child process that ended unsuccessfully. A successful exit records nothing.
pub(super) fn report_child_exit(shared: &Shared, program: &'static str, status: ExitStatus) {
    if status.success() || !shared.allowed(Signal::Crash) {
        return;
    }
    let mut crash = shared.crash_base(CrashKind::ChildExit, true);
    // The child could not continue; that much is known. A panic's `fatal` is left false at capture,
    // because the hook cannot know whether the host catches it.
    crash.fatal = true;
    crash.exit_code = status.code();
    crash.signal = signal_of(&status);
    crash.context = Some(program.to_owned());
    shared.remember_crash(&crash, true);
    shared.sink.submit(Signal::Crash, ItemBody::Crash(crash));
}

#[cfg(unix)]
fn signal_of(status: &ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(status)
}

#[cfg(not(unix))]
fn signal_of(_status: &ExitStatus) -> Option<i32> {
    None
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use ashlar_trace_proto::{Crash, ItemBody, Signal};

    use super::*;
    use crate::signals::testutil::{Fixture, serial};

    /// Crashes whose message contains `needle`. Other tests in the process panic on purpose, and a live hook
    /// sees those panics too, so assertions about the hook must pick out their own.
    fn panics_about(fixture: &Fixture, needle: &str) -> Vec<Crash> {
        crashes(fixture)
            .into_iter()
            .filter(|crash| crash.kind == CrashKind::Panic && crash.message.as_deref().is_some_and(|m| m.contains(needle)))
            .collect()
    }

    fn crashes(fixture: &Fixture) -> Vec<Crash> {
        fixture
            .sink
            .bodies()
            .into_iter()
            .filter_map(|body| match body {
                ItemBody::Crash(crash) => Some(crash),
                _ => None,
            })
            .collect()
    }

    /// Puts the hook that was installed before the test back when dropped.
    type HookFn = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

    struct RestoreHook(Option<HookFn>);

    impl Drop for RestoreHook {
        fn drop(&mut self) {
            // Changing the hook from a panicking thread panics, and a panic during unwinding aborts the
            // whole test process. Leave the hook as it is if the test already failed.
            if std::thread::panicking() {
                return;
            }
            let _ = std::panic::take_hook();
            if let Some(hook) = self.0.take() {
                std::panic::set_hook(hook);
            }
        }
    }

    fn save_hook() -> RestoreHook {
        RestoreHook(Some(std::panic::take_hook()))
    }

    #[test]
    fn panic_on_a_thread_is_reported_scrubbed_and_chained() {
        let _serial = serial();
        let _restore = save_hook();
        let previous_ran = Arc::new(AtomicBool::new(false));
        let flag = previous_ran.clone();
        std::panic::set_hook(Box::new(move |_| flag.store(true, Ordering::SeqCst)));

        let fixture = Fixture::new_locked(|config| config.signals = vec![Signal::Crash, Signal::Logs]);
        let logger = fixture.signals.logger(None);
        log::Log::log(&*logger, &log::Record::builder().level(log::Level::Info).target("app").args(format_args!("starting export")).build());

        let result = std::thread::Builder::new()
            .name("worker-7".to_owned())
            .spawn(|| {
                panic!("failed to open /Users/alice/Documents/plan.txt");
            })
            .map(std::thread::JoinHandle::join);
        assert!(matches!(result, Ok(Err(_))));

        assert!(previous_ran.load(Ordering::SeqCst), "the previous hook must still run");
        assert!(fixture.sink.panicked.load(Ordering::SeqCst), "mark_panicked must be called");
        // The thread's panic was survived by the process, so it is one of the session's errors.
        assert!(fixture.sink.errors.load(Ordering::SeqCst) >= 1, "a panic must be counted as an error");
        let items: Vec<_> = fixture
            .sink
            .items
            .lock()
            .expect("lock")
            .iter()
            .filter(|(_, body, _)| matches!(body, ItemBody::Crash(c) if c.message.as_deref().is_some_and(|m| m.contains("failed to open"))))
            .cloned()
            .collect();
        assert_eq!(items.len(), 1, "exactly one item: {items:?}");
        let (signal, body, now) = &items[0];
        assert_eq!(*signal, Signal::Crash);
        assert!(*now, "the crash path uses submit_now");
        let ItemBody::Crash(crash) = body else { panic!("not a crash") };
        assert_eq!(crash.kind, CrashKind::Panic);
        assert!(!crash.fatal);
        assert_eq!(crash.thread.as_deref(), Some("worker-7"));
        let message = crash.message.as_deref().unwrap_or_default();
        assert!(message.contains("failed to open"), "{message}");
        assert!(!message.contains("alice"), "{message}");
        assert!(crash.location.as_deref().is_some_and(|l| l.contains("crash.rs")), "{:?}", crash.location);
        assert!(crash.backtrace.as_deref().is_some_and(|b| !b.is_empty()));
        assert_eq!(crash.breadcrumbs.len(), 1);
        assert_eq!(crash.breadcrumbs[0].message, "starting export");
    }

    #[test]
    fn string_and_non_text_payloads_are_handled() {
        let _serial = serial();
        let _restore = save_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let fixture = Fixture::new_locked(|config| config.signals = vec![Signal::Crash]);
        let _ = std::thread::spawn(|| std::panic::panic_any(42_u32)).join();
        let _ = std::thread::spawn(|| {
            let n = 3;
            panic!("formatted {n}");
        })
        .join();
        let all: Vec<_> =
            crashes(&fixture).into_iter().filter(|c| c.message.as_deref().is_some_and(|m| m.contains("non-text") || m == "formatted 3")).collect();
        assert_eq!(all.len(), 2);
        assert!(all[0].message.as_deref().is_some_and(|m| m.contains("non-text")));
        assert_eq!(all[1].message.as_deref(), Some("formatted 3"));
    }

    #[test]
    fn denied_crash_signal_reports_nothing_but_still_chains() {
        let _serial = serial();
        let _restore = save_hook();
        let previous_ran = Arc::new(AtomicBool::new(false));
        let flag = previous_ran.clone();
        std::panic::set_hook(Box::new(move |_| flag.store(true, Ordering::SeqCst)));
        let fixture = Fixture::new_locked(|config| config.signals = vec![Signal::Crash]);
        fixture.deny(Signal::Crash);
        let _ = std::thread::spawn(|| panic!("nope")).join();
        assert!(previous_ran.load(Ordering::SeqCst));
        assert!(fixture.sink.bodies().is_empty());
        assert!(!fixture.sink.panicked.load(Ordering::SeqCst));
    }

    #[test]
    fn no_hook_when_crash_is_not_enabled() {
        let _serial = serial();
        let _restore = save_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let fixture = Fixture::new_locked(|config| config.signals = vec![Signal::Perf]);
        let _ = std::thread::spawn(|| panic!("nope")).join();
        assert!(fixture.sink.bodies().is_empty());
        assert!(!fixture.sink.panicked.load(Ordering::SeqCst));
    }

    #[test]
    fn a_second_start_keeps_working_and_a_dropped_one_goes_quiet() {
        let _serial = serial();
        let _restore = save_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let first = Fixture::new_locked(|config| config.signals = vec![Signal::Crash]);
        let second = Fixture::new_locked(|config| config.signals = vec![Signal::Crash]);
        let _ = std::thread::spawn(|| panic!("one")).join();
        assert_eq!(panics_about(&first, "one").len(), 1);
        assert_eq!(panics_about(&second, "one").len(), 1);

        let Fixture { signals, sink, .. } = first;
        drop(signals);
        let _ = std::thread::spawn(|| panic!("two")).join();
        let seen_by_first = sink.bodies().into_iter().filter(|b| matches!(b, ItemBody::Crash(c) if c.message.as_deref() == Some("two"))).count();
        assert_eq!(seen_by_first, 0, "a dropped Signals no longer reports");
        assert_eq!(panics_about(&second, "two").len(), 1);
    }

    #[test]
    fn a_panic_inside_the_hook_path_is_not_reported_twice() {
        let _serial = serial();
        let _restore = save_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let fixture = Fixture::new_locked(|config| config.signals = vec![Signal::Crash]);
        // Hold the thread's re-entry flag, as if this thread were already inside the hook.
        let guard = Reentry::enter();
        assert!(guard.is_some());
        assert!(Reentry::enter().is_none(), "second entry on the same thread is refused");
        let _ = std::thread::spawn(|| panic!("other thread")).join();
        drop(guard);
        assert_eq!(panics_about(&fixture, "other thread").len(), 1, "other threads are unaffected");
        assert!(Reentry::enter().is_some(), "flag is released");
    }

    #[test]
    fn hook_does_not_wait_for_a_busy_lock() {
        let _serial = serial();
        let _restore = save_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let fixture = Fixture::new_locked(|config| config.signals = vec![Signal::Crash, Signal::Logs]);
        let shared = fixture.signals.shared.clone();
        // Hold the perf and ring locks while another thread panics: the hook must still finish.
        let _perf = crate::signals::lock(&shared.perf, true);
        let _crash_slot = crate::signals::lock(&shared.last_crash, true);
        let _ = std::thread::spawn(|| panic!("while locked")).join();
        // The hook gave up on the busy locks instead of waiting for them; it still reported the panic.
        assert_eq!(panics_about(&fixture, "while locked").len(), 1);
    }

    /// Regression test for the intermittent abort ("thread caused non-unwinding panic"): changing the hook from
    /// a thread that is already panicking panics inside a destructor during unwinding, which aborts. Neither
    /// starting the SDK nor the test helper may do that.
    #[test]
    fn starting_signals_while_panicking_does_not_touch_the_hook() {
        let _serial = serial();
        let _restore = save_hook();
        std::panic::set_hook(Box::new(|_| {}));
        struct StartOnDrop(Option<Fixture>, Arc<AtomicBool>);
        impl Drop for StartOnDrop {
            fn drop(&mut self) {
                // Runs during unwinding.
                self.0 = Some(Fixture::new_locked(|config| config.signals = vec![Signal::Crash]));
                self.1.store(true, Ordering::SeqCst);
            }
        }
        let started = Arc::new(AtomicBool::new(false));
        let flag = started.clone();
        let result = std::thread::spawn(move || {
            let _guard = StartOnDrop(None, flag);
            panic!("unwinding while the SDK starts");
        })
        .join();
        assert!(result.is_err(), "the thread panicked and unwound normally");
        assert!(started.load(Ordering::SeqCst), "the destructor ran to completion instead of aborting");
    }

    #[test]
    fn handled_error_is_reported_and_counted() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.report_handled("could not read /home/bob/data.db", Some("import"));
        let items = fixture.sink.items.lock().expect("lock").clone();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0, Signal::Crash);
        assert!(!items[0].2, "handled errors use submit");
        let ItemBody::Crash(crash) = &items[0].1 else { panic!("not a crash") };
        assert_eq!(crash.kind, CrashKind::Handled);
        assert_eq!(crash.context.as_deref(), Some("import"));
        assert!(!crash.message.as_deref().unwrap_or_default().contains("bob"));
        assert!(!crash.fatal);
        assert_eq!(fixture.sink.errors.load(Ordering::SeqCst), 1);
        assert!(!fixture.sink.panicked.load(Ordering::SeqCst));
    }

    #[test]
    fn long_handled_messages_are_truncated() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.report_handled(&"m".repeat(10_000), None);
        let all = crashes(&fixture);
        assert!(all[0].message.as_ref().is_some_and(|m| m.len() <= MAX_MESSAGE));
    }

    #[test]
    fn denied_handled_error_produces_nothing() {
        let fixture = Fixture::new(|_| {});
        fixture.deny(Signal::Crash);
        fixture.signals.report_handled("x", None);
        assert!(fixture.sink.bodies().is_empty());
        assert_eq!(fixture.sink.errors.load(Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    fn status(raw: i32) -> ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw(raw)
    }

    #[cfg(windows)]
    fn status(raw: i32) -> ExitStatus {
        std::os::windows::process::ExitStatusExt::from_raw(u32::try_from(raw >> 8).unwrap_or(1))
    }

    #[test]
    fn successful_child_exit_records_nothing() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.report_child_exit("exporter", status(0));
        assert!(fixture.sink.bodies().is_empty());
    }

    #[test]
    fn child_exit_with_a_code() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.report_child_exit("exporter", status(3 << 8));
        let all = crashes(&fixture);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].kind, CrashKind::ChildExit);
        assert!(all[0].fatal, "a child that exited unsuccessfully could not continue");
        assert_eq!(all[0].exit_code, Some(3));
        assert_eq!(all[0].signal, None);
        assert_eq!(all[0].context.as_deref(), Some("exporter"));
        assert_eq!(fixture.sink.items.lock().expect("lock")[0].0, Signal::Crash);
    }

    #[cfg(unix)]
    #[test]
    fn child_killed_by_a_signal() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.report_child_exit("exporter", status(9));
        let all = crashes(&fixture);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].signal, Some(9));
        assert_eq!(all[0].exit_code, None);
    }

    #[test]
    fn denied_child_exit_produces_nothing() {
        let fixture = Fixture::new(|_| {});
        fixture.deny(Signal::Crash);
        fixture.signals.report_child_exit("exporter", status(1 << 8));
        assert!(fixture.sink.bodies().is_empty());
    }
}
