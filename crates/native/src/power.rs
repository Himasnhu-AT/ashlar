//! Keep the system awake, and keep the app at full speed in the background, while long work runs
//! (a render, an export, playback). An RAII guard: the request lasts while the [`KeepAwake`]
//! lives.
//!
//! | | macOS | Windows | Linux |
//! |---|---|---|---|
//! | [`Awake::Busy`] | no App Nap (user-initiated activity, idle sleep allowed) | nothing to do | nothing to do |
//! | [`Awake::System`] | + no idle system sleep | `ES_SYSTEM_REQUIRED` | logind `idle:sleep` inhibitor |
//! | [`Awake::Display`] | + no display sleep | + `ES_DISPLAY_REQUIRED` | logind `idle:sleep` inhibitor |
//!
//! The guard is not `Send`: on Windows the request belongs to the thread that made it, so create
//! and drop it on the same thread (a job's worker thread is fine).

use crate::Result;

/// How awake to stay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Awake {
    /// Keep the process at full speed when it is hidden (macOS App Nap off). The machine may
    /// still sleep when idle.
    Busy,
    /// Also keep the machine from sleeping when idle (renders, exports, downloads).
    System,
    /// Also keep the display on (playback, presentations).
    Display,
}

/// Holds the request until dropped.
pub struct KeepAwake {
    _inner: imp::Guard,
    // Not Send / Sync: the Windows request is per thread, the macOS token is an Objective-C object.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl KeepAwake {
    /// Ask the OS to stay awake. `reason` is shown by the OS (`pmset -g assertions`, logind) and
    /// clipped to one line.
    pub fn new(kind: Awake, reason: &str) -> Result<Self> {
        let reason: String = reason.chars().filter(|c| !c.is_control()).take(128).collect();
        let reason = if reason.trim().is_empty() { "working".to_string() } else { reason };
        Ok(Self { _inner: imp::Guard::new(kind, &reason)?, _not_send: std::marker::PhantomData })
    }
}

impl std::fmt::Debug for KeepAwake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("KeepAwake")
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod imp {
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    use super::*;

    pub struct Guard(Retained<ProtocolObject<dyn NSObjectProtocol>>);

    impl Guard {
        pub fn new(kind: Awake, reason: &str) -> Result<Self> {
            let options = match kind {
                Awake::Busy => NSActivityOptions::UserInitiatedAllowingIdleSystemSleep | NSActivityOptions::LatencyCritical,
                Awake::System => NSActivityOptions::UserInitiated,
                Awake::Display => NSActivityOptions::UserInitiated | NSActivityOptions::IdleDisplaySleepDisabled,
            };
            let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &NSString::from_str(reason));
            Ok(Self(token))
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: the token is exactly the object `beginActivityWithOptions:reason:` returned
            // for this activity, and it is ended once (here).
            unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
        }
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod imp {
    use std::cell::RefCell;

    use crate::NativeError;
    use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, EXECUTION_STATE, SetThreadExecutionState};

    use super::*;

    thread_local! {
        /// The requests alive on this thread: (id, flags). The thread's state is their union.
        static ACTIVE: RefCell<(u64, Vec<(u64, EXECUTION_STATE)>)> = const { RefCell::new((0, Vec::new())) };
    }

    fn apply(active: &[(u64, EXECUTION_STATE)]) -> bool {
        let flags = active.iter().fold(ES_CONTINUOUS, |acc, (_, f)| acc | *f);
        // SAFETY: SetThreadExecutionState takes flags by value and only changes this thread's
        // power request; it has no pointers or other preconditions.
        let previous = unsafe { SetThreadExecutionState(flags) };
        previous.0 != 0
    }

    pub struct Guard(Option<u64>);

    impl Guard {
        pub fn new(kind: Awake, _reason: &str) -> Result<Self> {
            let flags = match kind {
                Awake::Busy => return Ok(Self(None)),
                Awake::System => ES_SYSTEM_REQUIRED,
                Awake::Display => ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED,
            };
            ACTIVE.with(|a| {
                let mut a = a.borrow_mut();
                a.0 = a.0.wrapping_add(1);
                let id = a.0;
                a.1.push((id, flags));
                if apply(&a.1) {
                    Ok(Self(Some(id)))
                } else {
                    a.1.retain(|(i, _)| *i != id);
                    Err(NativeError::Os("Windows refused the power request".into()))
                }
            })
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let Some(id) = self.0 else { return };
            let _ = ACTIVE.try_with(|a| {
                if let Ok(mut a) = a.try_borrow_mut() {
                    a.1.retain(|(i, _)| *i != id);
                    apply(&a.1);
                }
            });
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::NativeError;

    /// The logind inhibitor's file descriptor; closing it ends the inhibition.
    pub struct Guard {
        _fd: Option<zbus::zvariant::OwnedFd>,
    }

    impl Guard {
        pub fn new(kind: Awake, reason: &str) -> Result<Self> {
            if kind == Awake::Busy {
                return Ok(Self { _fd: None });
            }
            let os = |e: zbus::Error| NativeError::Os(format!("logind refused the inhibitor ({})", crate::os_text(e)));
            let conn = zbus::blocking::Connection::system().map_err(os)?;
            let who =
                std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "app".into());
            let reply = conn
                .call_method(
                    Some("org.freedesktop.login1"),
                    "/org/freedesktop/login1",
                    Some("org.freedesktop.login1.Manager"),
                    "Inhibit",
                    &("idle:sleep", who.as_str(), reason, "block"),
                )
                .map_err(os)?;
            let fd: zbus::zvariant::OwnedFd = reply.body().deserialize().map_err(os)?;
            Ok(Self { _fd: Some(fd) })
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod imp {
    use super::*;
    use crate::NativeError;

    pub struct Guard;

    impl Guard {
        pub fn new(_kind: Awake, _reason: &str) -> Result<Self> {
            Err(NativeError::Unsupported)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativeError;

    #[test]
    fn busy_is_held_and_released_without_side_effects() {
        // App Nap off (macOS) or nothing to do (Windows, Linux): always succeeds there.
        let r = KeepAwake::new(Awake::Busy, "ashlar test\nwith a newline");
        if cfg!(any(target_os = "macos", target_os = "windows", target_os = "linux")) {
            let g = r.unwrap();
            drop(g);
        } else {
            assert_eq!(r.err(), Some(NativeError::Unsupported));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_system_assertion_shows_up_while_the_guard_lives() {
        let reason = format!("ashlar-native test {}", std::process::id());
        let assertions = || {
            String::from_utf8_lossy(&std::process::Command::new("pmset").args(["-g", "assertions"]).output().map(|o| o.stdout).unwrap_or_default())
                .into_owned()
        };
        let g = KeepAwake::new(Awake::System, &reason).unwrap();
        let during = assertions();
        drop(g);
        let after = assertions();
        if during.is_empty() {
            return; // pmset unavailable (sandbox): nothing to check
        }
        assert!(during.contains(&reason), "{during}");
        assert!(!after.contains(&reason), "{after}");
    }
}
