//! Quit, logout and shutdown requests from outside the window, so they get the same "Save
//! changes?" question and the same cleanup (settings, autosave, telemetry) as closing the window.
//!
//! - macOS: Dock ▸ Quit, the app menu's Quit item (`terminate:`), logout, restart and shutdown
//!   all ask the application delegate `applicationShouldTerminate:`. winit's delegate doesn't
//!   answer it, so [`listen`] adds the method to winit's delegate class at run time. It answers
//!   "later" and sends an [`EndRequest`]; the app answers with [`reply`] once the user has chosen.
//! - Windows: a hidden top-level window on its own thread receives `WM_QUERYENDSESSION` /
//!   `WM_ENDSESSION` (message-only windows don't get these broadcasts). Windows doesn't wait for
//!   a dialog, so the app saves recovery data and allows the logout ([`CAN_WAIT`] is `false`);
//!   `WM_ENDSESSION` waits a few seconds for [`reply`] so the flush finishes first.
//! - Elsewhere [`listen`] returns `Unsupported`; SIGTERM goes through [`crate::terminate`].
//!
//! What the app does with a request is the pure state machine [`step`].

use std::sync::mpsc::{Receiver, channel};

use crate::{Result, Waker};

/// Why the OS asks the app to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndKind {
    /// Dock ▸ Quit or another app asking this one to quit.
    Quit,
    /// The user logs out, restarts or shuts down.
    Logout,
}

/// One request to end the app. Answer it with [`reply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndRequest {
    pub kind: EndKind,
}

/// Whether the OS waits while the app asks "Save changes?" (macOS does; Windows ends the session
/// after a few seconds whatever the app answers).
pub const CAN_WAIT: bool = cfg!(target_os = "macos");

/// Start receiving end requests; `waker` runs after each one is sent. macOS: call on the main
/// thread once the event loop runs (winit's delegate must exist). Calling again replaces the
/// previous receiver.
pub fn listen(waker: Waker) -> Result<Receiver<EndRequest>> {
    let (tx, rx) = channel();
    imp::listen(tx, waker)?;
    Ok(rx)
}

/// Answer the pending request: `true` lets the app end (after its cleanup), `false` cancels
/// (the user chose Cancel, or a save failed). Without a pending request it only records the
/// answer for later requests (`true`: answer at once from now on).
pub fn reply(allow: bool) {
    imp::reply(allow);
}

// ------------------------------------------------------------------ the quit state machine

/// Where the app is in answering an end request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    /// No request pending.
    #[default]
    Idle,
    /// The "Save changes?" question is up.
    Asking(EndKind),
    /// The user chose Save; the save (maybe through a Save As dialog) runs.
    Saving(EndKind),
    /// Cleaned up and allowed: the process is ending.
    Done,
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The OS asks to end. `dirty`: the document has unsaved changes. `can_wait`: the OS waits
    /// for a dialog ([`CAN_WAIT`]).
    Requested { kind: EndKind, dirty: bool, can_wait: bool },
    /// Answers to the question.
    Save,
    Discard,
    Cancel,
    /// The save finished, or failed / its Save As dialog was cancelled.
    Saved,
    SaveFailed,
}

/// What the app must do, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Show "Save changes?".
    Ask,
    /// Save the document.
    Save,
    /// Stop jobs, flush settings and telemetry, end autosave. `keep_recovery`: the changes are
    /// unsaved and the user wasn't asked, so autosave keeps them for the next launch.
    Shutdown { keep_recovery: bool },
    /// `reply(true)`: let the app end.
    Allow,
    /// `reply(false)`: stay open.
    Refuse,
}

/// One step of the quit flow. Pure: the caller performs the effects.
pub fn step(state: State, event: Event) -> (State, Vec<Effect>) {
    use Effect::*;
    match (state, event) {
        // already cleaned up: let any later ask through at once
        (State::Done, Event::Requested { .. }) => (State::Done, vec![Allow]),
        (State::Idle, Event::Requested { dirty: false, .. }) => (State::Done, vec![Shutdown { keep_recovery: false }, Allow]),
        (State::Idle, Event::Requested { kind, dirty: true, can_wait: true }) => (State::Asking(kind), vec![Ask]),
        (State::Idle, Event::Requested { dirty: true, can_wait: false, .. }) => (State::Done, vec![Shutdown { keep_recovery: true }, Allow]),
        (State::Asking(kind), Event::Save) => (State::Saving(kind), vec![Save]),
        (State::Asking(_), Event::Discard) | (State::Saving(_), Event::Saved) => (State::Done, vec![Shutdown { keep_recovery: false }, Allow]),
        (State::Asking(_) | State::Saving(_), Event::Cancel) | (State::Saving(_), Event::SaveFailed) => (State::Idle, vec![Refuse]),
        // a second request while answering the first, or an answer nobody asked for
        (s, _) => (s, vec![]),
    }
}

// ------------------------------------------------------------------ macOS

#[cfg(all(target_os = "macos", feature = "session-end"))]
#[allow(unsafe_code)]
mod imp {
    use std::ffi::CStr;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::{Mutex, PoisonError};

    use block2::RcBlock;
    use objc2::runtime::{AnyClass, AnyObject, Imp, ProtocolObject, Sel};
    use objc2::{MainThreadMarker, sel};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{NSAppleEventManager, NSRunLoop};

    use super::*;
    use crate::NativeError;

    // NSApplicationTerminateReply
    const TERMINATE_NOW: usize = 1;
    const TERMINATE_LATER: usize = 2;

    const fn four(c: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*c)
    }
    /// `kEventParamReason`, the quit event's attribute that says why.
    const QUIT_REASON: u32 = four(b"why?");

    static SINK: Mutex<Option<(Sender<EndRequest>, Waker)>> = Mutex::new(None);
    /// We answered "later" and owe AppKit a reply.
    static PENDING: AtomicBool = AtomicBool::new(false);
    /// The app has cleaned up: answer "now" from here on.
    static ALLOWED: AtomicBool = AtomicBool::new(false);

    /// The quit Apple event's reason: logout, restart and shutdown carry one; Dock ▸ Quit doesn't.
    fn kind() -> EndKind {
        let event = NSAppleEventManager::sharedAppleEventManager().currentAppleEvent();
        let reason = event.and_then(|e| e.attributeDescriptorForKeyword(QUIT_REASON)).map(|d| d.enumCodeValue()).unwrap_or(0);
        if reason == 0 { EndKind::Quit } else { EndKind::Logout }
    }

    fn answer() -> usize {
        if ALLOWED.load(Ordering::SeqCst) {
            return TERMINATE_NOW;
        }
        let sink = SINK.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let Some((tx, wake)) = sink else { return TERMINATE_NOW };
        if PENDING.load(Ordering::SeqCst) {
            return TERMINATE_LATER;
        }
        // the app is gone (its receiver dropped): nothing to ask
        if tx.send(EndRequest { kind: kind() }).is_err() {
            return TERMINATE_NOW;
        }
        PENDING.store(true, Ordering::SeqCst);
        wake();
        TERMINATE_LATER
    }

    /// `-[delegate applicationShouldTerminate:]`. Never unwinds into AppKit: a panic lets the
    /// quit go on, as AppKit would without the method.
    extern "C-unwind" fn should_terminate(_this: *mut AnyObject, _cmd: Sel, _sender: *mut AnyObject) -> usize {
        std::panic::catch_unwind(answer).unwrap_or(TERMINATE_NOW)
    }

    pub fn listen(tx: Sender<EndRequest>, waker: Waker) -> Result<()> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Err(NativeError::Os("quit requests must be set up on the main thread".into()));
        };
        let app = NSApplication::sharedApplication(mtm);
        let Some(delegate) = app.delegate() else {
            return Err(NativeError::Os("the application has no delegate yet; set up quit requests once the event loop runs".into()));
        };
        let obj: &AnyObject = ProtocolObject::as_ref(&*delegate);
        let cls: &AnyClass = obj.class();
        let name = sel!(applicationShouldTerminate:);
        let ours = should_terminate as extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize;
        if let Some(m) = cls.instance_method(name) {
            let current = m.implementation();
            if current as usize != ours as usize {
                return Err(NativeError::Os("the application delegate already answers quit requests".into()));
            }
        } else {
            const TYPES: &CStr = c"Q@:@"; // NSUInteger (id self, SEL _cmd, id sender)
            // SAFETY: `Imp` is the untyped method pointer type; the runtime calls it with the
            // signature `TYPES` declares, which is `should_terminate`'s (`Q` = NSUInteger =
            // usize on the 64-bit targets macOS supports).
            let imp: Imp = unsafe { std::mem::transmute::<extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize, Imp>(ours) };
            // SAFETY: `cls` is a registered class (the delegate's), `name` a valid selector and
            // `TYPES` a NUL-terminated encoding matching `imp`. Adding a method the class lacks
            // is how the runtime extends classes; it only fails if the method exists.
            let added = unsafe { objc2::ffi::class_addMethod(cls as *const AnyClass as *mut AnyClass, name, imp, TYPES.as_ptr()) };
            if !added.as_bool() {
                return Err(NativeError::Os("could not add the quit handler to the application delegate".into()));
            }
            // NSApplication may cache what its delegate answers when the delegate is set: set
            // the same delegate again so it looks again.
            app.setDelegate(Some(&delegate));
        }
        *SINK.lock().unwrap_or_else(PoisonError::into_inner) = Some((tx, waker));
        Ok(())
    }

    pub fn reply(allow: bool) {
        if allow {
            ALLOWED.store(true, Ordering::SeqCst);
        }
        if !PENDING.swap(false, Ordering::SeqCst) {
            return;
        }
        // Not from inside the app's frame: a yes ends the process through winit's
        // `applicationWillTerminate:`, which must not run while winit handles an event. The
        // block runs from the run loop (common modes, which include the modal mode AppKit
        // waits in after "later").
        let block = RcBlock::new(move || {
            if let Some(mtm) = MainThreadMarker::new() {
                NSApplication::sharedApplication(mtm).replyToApplicationShouldTerminate(allow);
            }
        });
        // SAFETY: the main run loop runs the block on the main thread; it captures only a
        // `bool` and checks for the main thread again before touching AppKit.
        unsafe { NSRunLoop::mainRunLoop().performBlock(&block) };
    }
}

// ------------------------------------------------------------------ Windows

#[cfg(all(target_os = "windows", feature = "session-end"))]
#[allow(unsafe_code)]
mod imp {
    use std::sync::mpsc::Sender;
    use std::sync::{Condvar, Mutex, Once, PoisonError};
    use std::time::Duration;

    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE, WM_ENDSESSION,
        WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
    };
    use windows::core::w;

    use super::*;
    use crate::NativeError;

    /// How long `WM_ENDSESSION` waits for the app's cleanup. Windows shows "this app is
    /// preventing shutdown" after about five seconds.
    const FLUSH_WAIT: Duration = Duration::from_secs(4);

    static SINK: Mutex<Option<(Sender<EndRequest>, Waker)>> = Mutex::new(None);
    /// (a request is pending, the app answered)
    static REPLY: (Mutex<(bool, bool)>, Condvar) = (Mutex::new((false, false)), Condvar::new());

    fn request() {
        let sink = SINK.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let Some((tx, wake)) = sink else { return };
        let mut r = REPLY.0.lock().unwrap_or_else(PoisonError::into_inner);
        if r.1 || r.0 {
            return;
        }
        if tx.send(EndRequest { kind: EndKind::Logout }).is_ok() {
            r.0 = true;
            drop(r);
            wake();
        }
    }

    fn wait_for_reply() {
        let r = REPLY.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !r.0 {
            return;
        }
        let _ = REPLY.1.wait_timeout_while(r, FLUSH_WAIT, |r| !r.1);
    }

    extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let handled = std::panic::catch_unwind(|| match msg {
            WM_QUERYENDSESSION => {
                request();
                // never block the logout: the app keeps unsaved work for recovery instead
                Some(LRESULT(1))
            }
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    wait_for_reply();
                }
                Some(LRESULT(0))
            }
            _ => None,
        });
        match handled {
            Ok(Some(r)) => r,
            // SAFETY: the arguments are the ones this window procedure was called with.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn run() {
        // SAFETY: a null module name asks for this executable's handle.
        let Ok(module) = (unsafe { GetModuleHandleW(None) }) else { return };
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: module.into(), lpszClassName: w!("AshlarSessionEnd"), ..Default::default() };
        // SAFETY: `class` is fully initialised and its name a static wide string.
        if unsafe { RegisterClassW(&class) } == 0 {
            log::warn!("session end: the window class could not be registered");
            return;
        }
        // A top-level window that is never shown: message-only windows don't receive the
        // session-end broadcasts.
        // SAFETY: the class was registered above; no parent, menu or creation data.
        let hwnd = unsafe {
            CreateWindowExW(WINDOW_EX_STYLE(0), w!("AshlarSessionEnd"), w!(""), WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(module.into()), None)
        };
        if hwnd.is_err() {
            log::warn!("session end: the window could not be created");
            return;
        }
        let mut msg = MSG::default();
        // SAFETY: `msg` is a valid out-pointer; the loop ends when GetMessageW returns 0 or -1.
        while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
            // SAFETY: `msg` came from GetMessageW.
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    pub fn listen(tx: Sender<EndRequest>, waker: Waker) -> Result<()> {
        *SINK.lock().unwrap_or_else(PoisonError::into_inner) = Some((tx, waker));
        static START: Once = Once::new();
        let mut spawned = Ok(());
        START.call_once(|| {
            spawned = std::thread::Builder::new().name("ashlar-session-end".into()).spawn(run).map(drop);
        });
        spawned.map_err(|e| NativeError::Os(crate::os_text(e)))
    }

    pub fn reply(_allow: bool) {
        let mut r = REPLY.0.lock().unwrap_or_else(PoisonError::into_inner);
        r.1 = true;
        REPLY.1.notify_all();
    }
}

#[cfg(not(all(any(target_os = "macos", target_os = "windows"), feature = "session-end")))]
mod imp {
    use std::sync::mpsc::Sender;

    use super::*;

    pub fn listen(_tx: Sender<EndRequest>, _waker: Waker) -> Result<()> {
        Err(crate::NativeError::Unsupported)
    }
    pub fn reply(_allow: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use Effect::*;

    fn req(kind: EndKind, dirty: bool, can_wait: bool) -> Event {
        Event::Requested { kind, dirty, can_wait }
    }

    /// Run events from `Idle`, collecting every effect.
    fn run(events: &[Event]) -> (State, Vec<Effect>) {
        events.iter().fold((State::Idle, vec![]), |(s, mut all), e| {
            let (s, fx) = step(s, *e);
            all.extend(fx);
            (s, all)
        })
    }

    #[test]
    fn a_clean_quit_cleans_up_and_ends_at_once() {
        for kind in [EndKind::Quit, EndKind::Logout] {
            for can_wait in [true, false] {
                assert_eq!(run(&[req(kind, false, can_wait)]), (State::Done, vec![Shutdown { keep_recovery: false }, Allow]));
            }
        }
    }

    #[test]
    fn a_dirty_quit_asks_first() {
        assert_eq!(run(&[req(EndKind::Quit, true, true)]), (State::Asking(EndKind::Quit), vec![Ask]));
        assert_eq!(run(&[req(EndKind::Logout, true, true)]), (State::Asking(EndKind::Logout), vec![Ask]));
    }

    #[test]
    fn save_then_saved_ends() {
        let (s, fx) = run(&[req(EndKind::Quit, true, true), Event::Save, Event::Saved]);
        assert_eq!(s, State::Done);
        assert_eq!(fx, vec![Ask, Save, Shutdown { keep_recovery: false }, Allow]);
    }

    #[test]
    fn a_failed_or_cancelled_save_stays_open() {
        assert_eq!(run(&[req(EndKind::Quit, true, true), Event::Save, Event::SaveFailed]), (State::Idle, vec![Ask, Save, Refuse]));
        assert_eq!(run(&[req(EndKind::Quit, true, true), Event::Save, Event::Cancel]), (State::Idle, vec![Ask, Save, Refuse]));
    }

    #[test]
    fn discard_ends_without_keeping_recovery_data() {
        assert_eq!(run(&[req(EndKind::Quit, true, true), Event::Discard]), (State::Done, vec![Ask, Shutdown { keep_recovery: false }, Allow]));
    }

    #[test]
    fn cancel_stays_open_and_a_later_quit_asks_again() {
        let (s, fx) = run(&[req(EndKind::Logout, true, true), Event::Cancel]);
        assert_eq!((s, fx), (State::Idle, vec![Ask, Refuse]));
        assert_eq!(step(s, req(EndKind::Quit, true, true)), (State::Asking(EndKind::Quit), vec![Ask]));
    }

    #[test]
    fn a_logout_that_cant_wait_keeps_the_unsaved_work_for_recovery() {
        assert_eq!(run(&[req(EndKind::Logout, true, false)]), (State::Done, vec![Shutdown { keep_recovery: true }, Allow]));
    }

    #[test]
    fn repeated_requests_dont_stack_dialogs_and_stray_answers_do_nothing() {
        let (s, fx) = run(&[req(EndKind::Quit, true, true), req(EndKind::Quit, true, true), req(EndKind::Logout, true, true)]);
        assert_eq!((s, fx), (State::Asking(EndKind::Quit), vec![Ask]));
        for e in [Event::Save, Event::Discard, Event::Cancel, Event::Saved, Event::SaveFailed] {
            assert_eq!(step(State::Idle, e), (State::Idle, vec![]), "{e:?}");
        }
        assert_eq!(step(State::Saving(EndKind::Quit), req(EndKind::Quit, true, true)), (State::Saving(EndKind::Quit), vec![]));
    }

    #[test]
    fn once_done_every_later_ask_is_allowed() {
        assert_eq!(step(State::Done, req(EndKind::Quit, true, true)), (State::Done, vec![Allow]));
        assert_eq!(step(State::Done, Event::Cancel), (State::Done, vec![]));
    }

    #[test]
    fn listening_off_the_main_thread_or_unsupported_is_an_error_not_a_crash() {
        let r = std::thread::spawn(|| listen(crate::no_waker()).map(drop)).join();
        assert!(matches!(r, Ok(Err(_))));
        reply(false); // nothing pending: a no-op
    }
}
