//! Progress and a badge on the Dock icon (macOS) or the taskbar button (Windows), for long work
//! the user may watch from another app.
//!
//! macOS draws a progress bar over the app icon in the Dock and a badge label; the bar is a
//! snapshot, so [`Progress::Indeterminate`] shows an empty bar. Windows uses `ITaskbarList3` on
//! the window's taskbar button (pass the window); badges are `Unsupported` there. Main thread
//! only on both. Elsewhere `Unsupported`.

use raw_window_handle::HasWindowHandle;

use crate::{NativeError, Result};

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "state", content = "fraction")]
pub enum Progress {
    /// No progress shown.
    None,
    Indeterminate,
    /// 0.0 ..= 1.0 (clamped; NaN is 0).
    Normal(f64),
    Paused(f64),
    Error(f64),
}

impl Progress {
    /// The fraction, clamped to 0..=1.
    pub fn fraction(self) -> Option<f64> {
        match self {
            Progress::Normal(f) | Progress::Paused(f) | Progress::Error(f) => Some(if f.is_nan() { 0.0 } else { f.clamp(0.0, 1.0) }),
            _ => None,
        }
    }
}

/// Show `progress` for the app (macOS: the Dock icon) or `window`'s taskbar button (Windows).
pub fn set(window: Option<&dyn HasWindowHandle>, progress: Progress) -> Result<()> {
    crate::guarded("the progress bar", || imp::set(window, progress))
}

/// Show a short badge (`"3"`, `"!"`) on the Dock icon, or remove it with `None`. macOS only.
pub fn set_badge(badge: Option<&str>) -> Result<()> {
    let badge: Option<String> = badge.map(|b| b.chars().filter(|c| !c.is_control()).take(16).collect());
    crate::guarded("the badge", || imp::set_badge(badge.as_deref()))
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{NSApplication, NSImageView, NSProgressIndicator, NSProgressIndicatorStyle};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    use super::*;

    thread_local! {
        /// The Dock tile's content: the app icon with a bar over it, made on first use.
        static TILE: RefCell<Option<(Retained<NSImageView>, Retained<NSProgressIndicator>)>> = const { RefCell::new(None) };
    }

    fn main_thread() -> Result<MainThreadMarker> {
        MainThreadMarker::new().ok_or_else(|| NativeError::Os("Dock progress must be set on the main thread".into()))
    }

    pub fn set(_window: Option<&dyn HasWindowHandle>, progress: Progress) -> Result<()> {
        let mtm = main_thread()?;
        let app = NSApplication::sharedApplication(mtm);
        let tile = app.dockTile();
        if progress == Progress::None {
            tile.setContentView(None);
            tile.display();
            return Ok(());
        }
        let Some(icon) = app.applicationIconImage() else { return Err(NativeError::Os("the app has no icon to draw on".into())) };
        TILE.with(|t| {
            let mut t = t.borrow_mut();
            let (view, bar) = t.get_or_insert_with(|| {
                let view = NSImageView::imageViewWithImage(&icon, mtm);
                let size = tile.size();
                let frame = NSRect::new(NSPoint::new(size.width * 0.1, size.height * 0.06), NSSize::new(size.width * 0.8, size.height * 0.12));
                let bar = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), frame);
                bar.setStyle(NSProgressIndicatorStyle::Bar);
                bar.setMinValue(0.0);
                bar.setMaxValue(1.0);
                view.addSubview(&bar);
                (view, bar)
            });
            bar.setIndeterminate(false);
            bar.setDoubleValue(progress.fraction().unwrap_or(0.0));
            tile.setContentView(Some(view));
            tile.display();
        });
        Ok(())
    }

    pub fn set_badge(badge: Option<&str>) -> Result<()> {
        let mtm = main_thread()?;
        let tile = NSApplication::sharedApplication(mtm).dockTile();
        tile.setBadgeLabel(badge.filter(|b| !b.is_empty()).map(NSString::from_str).as_deref());
        Ok(())
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod imp {
    use std::cell::RefCell;

    use raw_window_handle::RawWindowHandle;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
    use windows::Win32::UI::Shell::{ITaskbarList3, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, TaskbarList};

    use super::*;

    thread_local! {
        static TASKBAR: RefCell<Option<ITaskbarList3>> = const { RefCell::new(None) };
    }

    fn os(e: windows::core::Error) -> NativeError {
        NativeError::Os(crate::os_text(e.message()))
    }

    fn taskbar() -> Result<ITaskbarList3> {
        TASKBAR.with(|t| {
            if let Some(tb) = t.borrow().as_ref() {
                return Ok(tb.clone());
            }
            // SAFETY: COM is initialized for this thread (an "already initialized in another
            // mode" result is fine: the object is apartment-agnostic for our calls); the class
            // id and interface are the documented pair for the taskbar list, and `HrInit` is
            // called once before any other method, as required.
            let tb: ITaskbarList3 = unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let tb: ITaskbarList3 = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).map_err(os)?;
                tb.HrInit().map_err(os)?;
                tb
            };
            *t.borrow_mut() = Some(tb.clone());
            Ok(tb)
        })
    }

    pub fn set(window: Option<&dyn HasWindowHandle>, progress: Progress) -> Result<()> {
        let window = window.ok_or_else(|| NativeError::Invalid("taskbar progress needs the window".into()))?;
        let handle = window.window_handle().map_err(|e| NativeError::Os(crate::os_text(e)))?;
        let RawWindowHandle::Win32(h) = handle.as_raw() else { return Err(NativeError::Unsupported) };
        let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
        let tb = taskbar()?;
        let flag = match progress {
            Progress::None => TBPF_NOPROGRESS,
            Progress::Indeterminate => TBPF_INDETERMINATE,
            Progress::Normal(_) => TBPF_NORMAL,
            Progress::Paused(_) => TBPF_PAUSED,
            Progress::Error(_) => TBPF_ERROR,
        };
        // SAFETY: `hwnd` names a live window (borrowed through its handle for this call) and `tb`
        // is an initialized taskbar list; both calls take plain values.
        unsafe {
            tb.SetProgressState(hwnd, flag).map_err(os)?;
            if let Some(f) = progress.fraction() {
                tb.SetProgressValue(hwnd, (f * 10_000.0) as u64, 10_000).map_err(os)?;
            }
        }
        Ok(())
    }

    pub fn set_badge(_badge: Option<&str>) -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use super::*;

    pub fn set(_window: Option<&dyn HasWindowHandle>, _progress: Progress) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn set_badge(_badge: Option<&str>) -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions_are_clamped() {
        assert_eq!(Progress::Normal(1.5).fraction(), Some(1.0));
        assert_eq!(Progress::Error(-1.0).fraction(), Some(0.0));
        assert_eq!(Progress::Paused(f64::NAN).fraction(), Some(0.0));
        assert_eq!(Progress::Indeterminate.fraction(), None);
        assert_eq!(serde_json::to_value(Progress::Normal(0.5)).unwrap(), serde_json::json!({"state": "normal", "fraction": 0.5}));
    }

    #[test]
    fn off_the_main_thread_it_fails_instead_of_crashing() {
        // Test threads are not the main thread.
        assert!(set(None, Progress::Normal(0.5)).is_err());
        assert!(set_badge(Some("3")).is_err());
    }
}
