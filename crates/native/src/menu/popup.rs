//! Native context menus from a [`MenuSpec`] (macOS and Windows; mapped like the tray's menu:
//! one top-level menu shows its children, several show as submenus).
//!
//! The OS runs a context menu in its own tracking loop until it closes, so it must never open
//! inside the UI pass: on macOS [`show`] posts it to the main run loop, so it opens once the
//! current frame is done; on Windows it opens at once, so call it between frames (after the UI
//! pass). Either way the choice arrives on the returned receiver: `Some(id)` for a chosen item,
//! `None` when the menu was dismissed, and `waker` is called.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use super::MenuSpec;
use crate::{NativeError, Result, Waker};

static NEXT: AtomicU64 = AtomicU64::new(1);

fn os(e: impl std::fmt::Display) -> NativeError {
    NativeError::Os(crate::os_text(e))
}

/// Run `open` (which shows the menu and says whether an item was chosen) and report the outcome.
fn finish(open: impl FnOnce() -> bool, picks: &Receiver<String>, tx: &Sender<Option<String>>, waker: &Waker) {
    let chosen = std::panic::catch_unwind(std::panic::AssertUnwindSafe(open)).unwrap_or(false);
    // the item's event is delivered while the menu tracks, before `open` returns
    let id = if chosen { picks.try_iter().last() } else { None };
    if tx.send(id).is_ok() {
        waker();
    }
}

/// Open `spec` as a context menu in `window` at `at` (logical points from the window's top-left
/// corner; `None`: at the pointer). See the module docs for when it opens.
#[allow(unsafe_code)]
pub fn show(
    window: &impl raw_window_handle::HasWindowHandle,
    spec: &MenuSpec,
    at: Option<(f64, f64)>,
    waker: Waker,
) -> Result<Receiver<Option<String>>> {
    use muda::ContextMenu as _;
    use raw_window_handle::RawWindowHandle;

    spec.validate().map_err(|p| NativeError::Invalid(p.join("; ")))?;
    let prefix = format!("\u{1}popup/{}/", NEXT.fetch_add(1, Ordering::Relaxed));
    let menu = crate::menu_tree::build(&crate::menu_tree::plan(spec, &prefix))?;
    let (pick_tx, picks) = channel();
    let route = crate::menu_events::subscribe(prefix, pick_tx, crate::no_waker());
    let (tx, rx) = channel();
    let position = at.map(|(x, y)| muda::dpi::Position::Logical(muda::dpi::LogicalPosition::new(x, y)));
    let raw = window.window_handle().map_err(os)?.as_raw();
    match raw {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(h) => {
            use objc2::rc::Retained;
            use objc2::runtime::AnyObject;

            let mtm = objc2::MainThreadMarker::new().ok_or_else(|| NativeError::Os("open context menus on the main thread".into()))?;
            // SAFETY: `ns_view` comes from a live window handle borrowed for this call, so it is a
            // valid `NSView`; retaining it keeps it valid until the posted block has run, even if
            // the window closes first.
            let view: Retained<AnyObject> = unsafe { Retained::retain(h.ns_view.as_ptr().cast::<AnyObject>()) }
                .ok_or_else(|| NativeError::Os("the window has no view".into()))?;
            let block = block2::RcBlock::new(move || {
                let _keep = &route;
                // SAFETY: `view` is a retained, valid `NSView` and this block runs on the main
                // thread (the main run loop), outside the frame that asked for the menu.
                finish(|| unsafe { menu.show_context_menu_for_nsview(Retained::as_ptr(&view).cast(), position) }, &picks, &tx, &waker);
            });
            let _ = mtm;
            // SAFETY: the main run loop runs the block on the main thread; it owns everything it
            // touches.
            unsafe { objc2_foundation::NSRunLoop::mainRunLoop().performBlock(&block) };
        }
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(h) => {
            let hwnd = h.hwnd.get();
            // SAFETY: `hwnd` comes from a live window handle borrowed for this call, so it names
            // a valid window of this thread; the menu tracks and closes before this returns.
            finish(|| unsafe { menu.show_context_menu_for_hwnd(hwnd, position) }, &picks, &tx, &waker);
            drop(route);
        }
        _ => {
            let _ = (menu, route, picks, tx, position, waker);
            return Err(NativeError::Unsupported);
        }
    }
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_pick_wins_and_a_dismissal_is_none() {
        let (ptx, picks) = channel();
        let (tx, rx) = channel();
        ptx.send("a".to_string()).unwrap();
        finish(|| true, &picks, &tx, &crate::no_waker());
        assert_eq!(rx.try_recv().unwrap().as_deref(), Some("a"));
        ptx.send("stale".to_string()).unwrap();
        finish(|| false, &picks, &tx, &crate::no_waker());
        assert_eq!(rx.try_recv().unwrap(), None);
        finish(|| panic!("in the OS binding"), &picks, &tx, &crate::no_waker());
        assert_eq!(rx.try_recv().unwrap(), None, "a panic is a dismissal");
    }
}
