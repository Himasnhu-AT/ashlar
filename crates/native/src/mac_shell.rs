//! Fixes and additions to the windowing library's macOS application shell (winit 0.30):
//!
//! - [`guard_appearance`]: the window delegate observes `effectiveAppearance` and casts the old
//!   and new values to `NSAppearance` unchecked. While the screen is being captured
//!   (ScreenCaptureKit, ReplayKit) AppKit can report a nil appearance, and the cast ends the
//!   process. The guard checks the values first and drops such a change.
//! - [`listen_reopen`]: clicking the Dock icon (or opening the app again in Finder) asks the
//!   application delegate `applicationShouldHandleReopen:hasVisibleWindows:`, which winit doesn't
//!   answer, so a hidden window stays hidden. The method is added and reported, so the app shows
//!   and focuses its window.
//! - [`quiet_requested`]: `<PREFIX>_QUIET=1` asks for a launch that doesn't take the focus, for
//!   agents and scripts that rebuild and relaunch the app in a loop. The app then opens its window
//!   without activating (an ashlar app does this itself).
//!
//! The first two return [`NativeError::Unsupported`] off macOS.

use std::sync::mpsc::{Receiver, channel};

use crate::{NativeError, Result, Waker};

/// The Dock (or Finder) asked to reopen the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reopen {
    /// AppKit's view: some window is visible (none: the app should show one).
    pub has_visible_windows: bool,
}

/// `<env_prefix>_QUIET` is set to something other than empty, `0`, `false`, `no` or `off`.
pub fn quiet_requested(env_prefix: &str) -> bool {
    std::env::var(format!("{env_prefix}_QUIET")).is_ok_and(|v| quiet_value(&v))
}

fn quiet_value(v: &str) -> bool {
    let v = v.trim();
    !(v.is_empty() || v == "0" || ["false", "no", "off"].iter().any(|f| v.eq_ignore_ascii_case(f)))
}

/// Guard the window delegate's appearance observer (see the module docs). Call it on the main
/// thread once a window exists (the delegate class is registered with the first window). Calling
/// again does nothing.
pub fn guard_appearance() -> Result<()> {
    imp::guard_appearance()
}

/// Report Dock reopen requests: each one is queued on the returned receiver and `waker` is
/// called. Main thread, once the event loop runs (winit's delegate must exist). Calling again
/// moves the reports to the new receiver.
pub fn listen_reopen(waker: Waker) -> Result<Receiver<Reopen>> {
    let (tx, rx) = channel();
    imp::listen_reopen(tx, waker)?;
    Ok(rx)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use std::ffi::{CStr, c_void};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::{Mutex, PoisonError};

    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, ProtocolObject, Sel};
    use objc2::{MainThreadMarker, msg_send, sel};
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{NSKeyValueChangeNewKey, NSKeyValueChangeOldKey, NSString};

    use super::*;

    // ----------------------------------------------------------------- appearance guard

    type Observe = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject, *mut AnyObject, *mut c_void);

    /// winit's own `observeValueForKeyPath:ofObject:change:context:`, once replaced.
    static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

    /// `obj` is a non-nil `NSAppearance`.
    fn is_appearance(obj: *mut AnyObject) -> bool {
        let Some(cls) = AnyClass::get(c"NSAppearance") else { return false };
        // SAFETY: a non-null `obj` is an object from the change dictionary; every object answers
        // `isKindOfClass:`.
        !obj.is_null() && unsafe { msg_send![obj, isKindOfClass: cls] }
    }

    /// Whether the change is one winit's observer can take: the appearance key path with an
    /// `NSAppearance` on both sides. Anything else would end the process inside it.
    fn safe_change(key_path: *mut AnyObject, change: *mut AnyObject) -> bool {
        if key_path.is_null() || change.is_null() {
            return false;
        }
        let ours = NSString::from_str("effectiveAppearance");
        // SAFETY: `key_path` is the `NSString` KVO passes; `isEqual:` takes any object.
        let same: bool = unsafe { msg_send![key_path, isEqual: &*ours] };
        if !same {
            return false;
        }
        // SAFETY: `change` is the KVO change `NSDictionary`; the keys are AppKit's constants.
        let (old, new): (*mut AnyObject, *mut AnyObject) =
            unsafe { (msg_send![change, objectForKey: NSKeyValueChangeOldKey], msg_send![change, objectForKey: NSKeyValueChangeNewKey]) };
        is_appearance(old) && is_appearance(new)
    }

    extern "C-unwind" fn observe(
        this: *mut AnyObject,
        cmd: Sel,
        key_path: *mut AnyObject,
        object: *mut AnyObject,
        change: *mut AnyObject,
        context: *mut c_void,
    ) {
        let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| safe_change(key_path, change))).unwrap_or(false);
        let original = ORIGINAL.load(Ordering::SeqCst);
        if !ok || original == 0 {
            log::debug!("ignored an appearance change without an appearance (screen capture?)");
            return;
        }
        // SAFETY: `original` is the implementation `method_setImplementation` handed back for
        // this selector, whose signature is `Observe` (v@:@@@^v).
        let f: Observe = unsafe { std::mem::transmute::<usize, Observe>(original) };
        f(this, cmd, key_path, object, change, context);
    }

    pub fn guard_appearance() -> Result<()> {
        if MainThreadMarker::new().is_none() {
            return Err(NativeError::Os("guard the appearance observer on the main thread".into()));
        }
        let Some(cls) = AnyClass::get(c"WinitWindowDelegate") else {
            return Err(NativeError::Os("no window yet: guard the appearance observer after the first window opens".into()));
        };
        let Some(method) = cls.instance_method(sel!(observeValueForKeyPath:ofObject:change:context:)) else {
            return Err(NativeError::Unsupported);
        };
        let ours = observe as Observe;
        if method.implementation() as usize == ours as usize {
            return Ok(());
        }
        // SAFETY: `Imp` is the untyped method pointer; the runtime calls it with the selector's
        // signature, which is `observe`'s.
        let imp: Imp = unsafe { std::mem::transmute::<Observe, Imp>(ours) };
        // SAFETY: `method` belongs to a registered class and `imp` matches its signature. The
        // original goes into `ORIGINAL` before any call could reach `observe`: KVO calls come on
        // the main thread, which is this one.
        let before = unsafe { method.set_implementation(imp) };
        ORIGINAL.store(before as usize, Ordering::SeqCst);
        Ok(())
    }

    // ----------------------------------------------------------------- Dock reopen

    static REOPEN: Mutex<Option<(Sender<Reopen>, Waker)>> = Mutex::new(None);

    fn report(visible: bool) {
        let sink = REOPEN.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some((tx, wake)) = sink
            && tx.send(Reopen { has_visible_windows: visible }).is_ok()
        {
            wake();
        }
    }

    /// `-[delegate applicationShouldHandleReopen:hasVisibleWindows:]`: report it and let AppKit
    /// do its usual part (un-minimize a window). Never unwinds into AppKit.
    extern "C-unwind" fn should_reopen(_this: *mut AnyObject, _cmd: Sel, _app: *mut AnyObject, visible: Bool) -> Bool {
        let _ = std::panic::catch_unwind(|| report(visible.as_bool()));
        Bool::YES
    }

    pub fn listen_reopen(tx: Sender<Reopen>, waker: Waker) -> Result<()> {
        let Some(mtm) = MainThreadMarker::new() else {
            return Err(NativeError::Os("listen for Dock reopen on the main thread".into()));
        };
        let app = NSApplication::sharedApplication(mtm);
        let Some(delegate) = app.delegate() else {
            return Err(NativeError::Os("the application has no delegate yet; listen once the event loop runs".into()));
        };
        let obj: &AnyObject = ProtocolObject::as_ref(&*delegate);
        let cls: &AnyClass = obj.class();
        let name = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
        type Reopened = extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, Bool) -> Bool;
        let ours = should_reopen as Reopened;
        match cls.instance_method(name) {
            Some(m) if m.implementation() as usize == ours as usize => {}
            Some(_) => return Err(NativeError::Os("the application delegate already answers Dock reopen".into())),
            None => {
                const TYPES: &CStr = c"c@:@c"; // BOOL (id self, SEL _cmd, NSApplication *app, BOOL flag)
                // SAFETY: `Imp` is the untyped method pointer; the runtime calls it with the
                // signature `TYPES` declares, which is `should_reopen`'s.
                let imp: Imp = unsafe { std::mem::transmute::<Reopened, Imp>(ours) };
                // SAFETY: `cls` is a registered class (the delegate's), `name` a valid selector
                // and `TYPES` a NUL-terminated encoding matching `imp`; it only fails if the
                // method exists, which was checked above.
                let added = unsafe { objc2::ffi::class_addMethod(cls as *const AnyClass as *mut AnyClass, name, imp, TYPES.as_ptr()) };
                if !added.as_bool() {
                    return Err(NativeError::Os("could not add the reopen handler to the application delegate".into()));
                }
                // NSApplication may cache what its delegate answers: set it again.
                app.setDelegate(Some(&delegate));
            }
        }
        *REOPEN.lock().unwrap_or_else(PoisonError::into_inner) = Some((tx, waker));
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use std::sync::mpsc::Sender;

    use super::*;

    pub fn guard_appearance() -> Result<()> {
        Err(NativeError::Unsupported)
    }

    pub fn listen_reopen(_tx: Sender<Reopen>, _waker: Waker) -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_values() {
        for on in ["1", "yes", "true", " on "] {
            assert!(quiet_value(on), "{on:?}");
        }
        for off in ["", " ", "0", "false", "NO", "off"] {
            assert!(!quiet_value(off), "{off:?}");
        }
        assert!(!quiet_requested("ASHLAR_TEST_SURELY_UNSET_PREFIX"));
    }

    #[test]
    fn off_the_main_thread_is_an_error_not_a_crash() {
        // the test harness runs tests on worker threads
        assert!(guard_appearance().is_err());
        assert!(listen_reopen(crate::no_waker()).is_err());
    }
}
