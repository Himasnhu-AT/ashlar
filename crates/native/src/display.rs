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
//!
//! [`monitors`] lists the monitors before any window exists (macOS `NSScreen.screens`, Windows
//! `EnumDisplayMonitors`; empty elsewhere), so a window placement saved last time can be checked
//! against them before the window opens.

/// Why no window can be shown here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NoDesktop(pub String);

/// `Ok` when this process can show windows (see the module docs).
pub fn check() -> Result<(), NoDesktop> {
    imp::check()
}

/// A monitor's area in points (logical pixels), in desktop coordinates: origin at the primary
/// monitor's top left, y down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Monitor {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Pixels per point.
    pub scale: f32,
}

/// At most this many monitors are listed.
pub const MAX_MONITORS: usize = 64;

/// The monitors, the primary first. Empty where the platform has no way to ask before a window
/// exists (Linux, the web), when called off the main thread on macOS, or when the call fails.
pub fn monitors() -> Vec<Monitor> {
    screens::monitors()
}

/// A macOS screen frame (points, origin at the primary screen's bottom left, y up) in desktop
/// coordinates (y down from the primary's top), given the primary's height.
pub fn flip_y(x: f64, y: f64, width: f64, height: f64, primary_height: f64, scale: f64) -> Option<Monitor> {
    let m = Monitor { x: x as f32, y: (primary_height - y - height) as f32, width: width as f32, height: height as f32, scale: scale as f32 };
    let ok = [m.x, m.y, m.width, m.height, m.scale].iter().all(|v| v.is_finite()) && m.width >= 1.0 && m.height >= 1.0 && m.scale > 0.0;
    ok.then_some(m)
}

#[cfg(target_os = "macos")]
mod screens {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSScreen;

    pub fn monitors() -> Vec<super::Monitor> {
        // AppKit answers only on the main thread
        let Some(mtm) = MainThreadMarker::new() else { return Vec::new() };
        let screens = NSScreen::screens(mtm);
        // the first screen is the one with the menu bar, whose bottom left is the origin
        let Some(primary) = screens.iter().next().map(|s| s.frame().size.height) else { return Vec::new() };
        screens
            .iter()
            .take(super::MAX_MONITORS)
            .filter_map(|s| {
                let f = s.frame();
                super::flip_y(f.origin.x, f.origin.y, f.size.width, f.size.height, primary, s.backingScaleFactor())
            })
            .collect()
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod screens {
    use windows::Win32::Foundation::{LPARAM, RECT};
    use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::core::BOOL;

    /// `MONITORINFO::dwFlags` bit of the primary monitor (winuser.h).
    const MONITORINFOF_PRIMARY: u32 = 1;

    /// One monitor: its rectangle in pixels, its scale and whether it is the primary.
    type Found = Vec<(super::Monitor, bool)>;

    pub fn monitors() -> Vec<super::Monitor> {
        let mut found: Found = Vec::new();
        // SAFETY: `found` outlives the call, which runs `collect` synchronously on this thread
        // and passes the LPARAM back unchanged; no clip rectangle and no device context.
        let _ = unsafe { EnumDisplayMonitors(None, None, Some(collect), LPARAM((&raw mut found) as isize)) };
        // the primary first, as the other platforms list it
        found.sort_by_key(|(_, primary)| !primary);
        found.into_iter().map(|(m, _)| m).collect()
    }

    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `&mut Found` that `monitors` passed, alive and not otherwise
        // borrowed while EnumDisplayMonitors runs.
        let Some(found) = (unsafe { (data.0 as *mut Found).as_mut() }) else { return false.into() };
        if found.len() >= super::MAX_MONITORS {
            return false.into();
        }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        // SAFETY: `monitor` is the handle the enumeration passed; `info` is writable and its
        // `cbSize` says its size.
        if !unsafe { GetMonitorInfoW(monitor, &raw mut info) }.as_bool() {
            return true.into();
        }
        let (mut dx, mut dy) = (0u32, 0u32);
        // SAFETY: as above; both outputs are writable u32s. A process that is not DPI aware
        // yet gets 96 and rectangles already in 96-DPI units, so points come out right either way.
        let dpi = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dx, &raw mut dy) }.ok().map_or(96, |()| dx);
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let r = info.rcMonitor;
        let (w, h) = (r.right.saturating_sub(r.left) as f32, r.bottom.saturating_sub(r.top) as f32);
        let m = super::Monitor { x: r.left as f32 / scale, y: r.top as f32 / scale, width: w / scale, height: h / scale, scale };
        if m.width >= 1.0 && m.height >= 1.0 {
            found.push((m, info.dwFlags & MONITORINFOF_PRIMARY != 0));
        }
        true.into()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod screens {
    pub fn monitors() -> Vec<super::Monitor> {
        Vec::new()
    }
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
    fn mac_frames_flip_to_y_down() {
        // primary 1440x900 at the origin; a second screen above-left of it
        assert_eq!(flip_y(0.0, 0.0, 1440.0, 900.0, 900.0, 2.0), Some(Monitor { x: 0.0, y: 0.0, width: 1440.0, height: 900.0, scale: 2.0 }));
        let above = flip_y(-1920.0, 900.0, 1920.0, 1080.0, 900.0, 1.0);
        assert_eq!(above, Some(Monitor { x: -1920.0, y: -1080.0, width: 1920.0, height: 1080.0, scale: 1.0 }));
        assert_eq!(flip_y(0.0, 0.0, 0.0, 900.0, 900.0, 1.0), None);
        assert_eq!(flip_y(f64::NAN, 0.0, 10.0, 10.0, 900.0, 1.0), None);
    }

    #[test]
    fn monitors_off_the_main_thread_are_empty_not_a_crash() {
        // test threads are not the main thread: macOS answers nothing rather than touch AppKit
        let off = std::thread::spawn(monitors).join().unwrap_or_default();
        if cfg!(target_os = "macos") {
            assert!(off.is_empty());
        }
        assert!(off.len() <= MAX_MONITORS);
    }

    #[test]
    fn this_test_process_can_show_windows_or_says_why() {
        // on a desktop and in CI this passes; it must never panic either way
        if let Err(e) = check() {
            assert!(!e.0.is_empty());
        }
    }
}
