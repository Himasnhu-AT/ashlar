//! Whether this process can show a window at all.
//!
//! On Windows a program started by a service, a scheduled task "whether the user is logged on or
//! not", or a remote shell running as SYSTEM (SSH, a management agent) lives in session 0 or in a
//! service's window station. Windows never shows that window station: a window can be created, but
//! the graphics driver refuses it a swap chain, and wgpu treats that as fatal (it panics on
//! `Surface::configure`, before the app can install an error handler). [`check`] tells apps so up
//! front, so they can say why and stop cleanly instead of crashing.
//!
//! A disconnected or locked remote session is not this case: its window station is interactive,
//! the app keeps running and draws again when the user reconnects.
//!
//! On every other platform [`check`] answers `Ok`: there the windowing library reports a missing
//! display as an ordinary error.

/// Why no window can be shown here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NoDesktop(pub String);

/// `Ok` when this process can show windows (see the module docs).
pub fn check() -> Result<(), NoDesktop> {
    imp::check()
}

/// `WSF_VISIBLE`: the window station has visible display surfaces (it is the interactive one).
pub const WSF_VISIBLE: u32 = 0x0001;

/// [`check`]'s decision from a window station's `UOI_FLAGS` (`None` when they could not be read,
/// which is not treated as "no desktop": an app that can't tell tries to open its window).
pub fn from_window_station_flags(flags: Option<u32>) -> Result<(), NoDesktop> {
    match flags {
        Some(f) if f & WSF_VISIBLE == 0 => Err(NoDesktop(
            "this program was started outside the signed-in desktop (by a service, a scheduled task that runs whether or not the user is signed in, or a remote shell running as SYSTEM), where Windows shows no windows. Start it from the desktop, or from a task that runs only when the user is signed in"
                .into(),
        )),
        _ => Ok(()),
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod imp {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::StationsAndDesktops::{GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS};

    pub fn check() -> Result<(), super::NoDesktop> {
        super::from_window_station_flags(flags())
    }

    fn flags() -> Option<u32> {
        // SAFETY: GetProcessWindowStation takes no arguments and returns a handle owned by the
        // system, which must not be closed.
        let station = unsafe { GetProcessWindowStation() }.ok()?;
        let mut info = USEROBJECTFLAGS::default();
        let size = std::mem::size_of::<USEROBJECTFLAGS>() as u32;
        // SAFETY: `station` is the live window station handle from above; `info` is a writable
        // USEROBJECTFLAGS and `size` is its exact size, which is what UOI_FLAGS fills.
        unsafe { GetUserObjectInformationW(HANDLE(station.0), UOI_FLAGS, Some((&raw mut info).cast()), size, None) }.ok()?;
        Some(info.dwFlags)
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    pub fn check() -> Result<(), super::NoDesktop> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hidden_window_station_has_no_desktop() {
        // the flags of a service's window station (Service-0x0-3e7$): no WSF_VISIBLE
        let e = from_window_station_flags(Some(0)).err();
        assert!(e.is_some_and(|e| e.0.contains("outside the signed-in desktop")));
        assert!(from_window_station_flags(Some(0x0002)).is_err());
    }

    #[test]
    fn the_interactive_window_station_and_unknown_flags_pass() {
        assert_eq!(from_window_station_flags(Some(WSF_VISIBLE)), Ok(()));
        assert_eq!(from_window_station_flags(Some(WSF_VISIBLE | 0x0100)), Ok(()));
        assert_eq!(from_window_station_flags(None), Ok(()));
    }

    #[test]
    fn this_test_process_can_show_windows_or_says_why() {
        // on a desktop and in CI this passes; it must never panic either way
        if let Err(e) = check() {
            assert!(!e.0.is_empty());
        }
    }
}
