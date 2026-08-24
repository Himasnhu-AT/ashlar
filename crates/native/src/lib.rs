//! Native OS features behind one small, safe API: file dialogs, the menu bar, the clipboard,
//! keep-awake, files opened from the file manager, single instance, notifications, dock / taskbar
//! progress, the OS keychain and system preferences.
//!
//! Every feature is behind a cargo feature and degrades cleanly: on a platform where it is not
//! implemented (or on the web) a call returns [`NativeError::Unsupported`]; nothing panics. The
//! crate never depends on a UI toolkit, so apps wire it to their own event loop: activation events
//! arrive on channels, and callbacks take a waker so an idle app wakes only when something
//! happened.
//!
//! | Module | macOS | Windows | Linux | Web |
//! |---|---|---|---|---|
//! | [`dialogs`] | rfd | rfd | rfd (portal) | rfd (bytes only) |
//! | [`menu`] | muda | muda (per window) | spec only | spec only |
//! | [`clipboard`] | arboard + NSPasteboard types | arboard | arboard | – |
//! | [`power`] | NSProcessInfo activity | SetThreadExecutionState | logind inhibit | – |
//! | [`open_files`] | Apple events + argv | argv | argv | – |
//! | [`single_instance`] | Unix socket | loopback + token | Unix socket | – |
//! | [`notify`] | UNUserNotificationCenter (bundled apps) | – | D-Bus notifications | – |
//! | [`progress`] | NSDockTile | ITaskbarList3 | – | – |
//! | [`secrets`] | Keychain | Credential Manager | Secret Service | – |
//! | [`system`] | AppKit | registry / SPI | portal settings | media queries |
//!
//! `unsafe` is denied in this crate and allowed only in the FFI modules that need it, each block
//! with a `// SAFETY:` comment (docs/adr/0001-native-ffi.md).

mod error;
pub use error::{NativeError, Result};

#[cfg(feature = "clipboard")]
pub mod clipboard;
#[cfg(feature = "dialogs")]
pub mod dialogs;
#[cfg(feature = "menu")]
pub mod menu;
#[cfg(feature = "notify")]
pub mod notify;
#[cfg(feature = "open-files")]
pub mod open_files;
#[cfg(feature = "power")]
pub mod power;
#[cfg(feature = "progress")]
pub mod progress;
#[cfg(feature = "secrets")]
pub mod secrets;
#[cfg(feature = "single-instance")]
pub mod single_instance;
#[cfg(feature = "system")]
pub mod system;

/// Something that wakes the app's event loop (an egui `request_repaint`, a winit proxy event).
/// Called from background threads after a new event was queued.
pub type Waker = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A waker that does nothing, for apps that poll.
pub fn no_waker() -> Waker {
    std::sync::Arc::new(|| {})
}

/// Run `f`, turning a panic inside a third-party binding into an error instead of unwinding into
/// the app.
#[allow(dead_code)]
pub(crate) fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(NativeError::Os(format!("{what} failed inside the OS binding"))))
}

/// The OS's own words for a failure, made safe to show: one line, at most 200 characters.
#[allow(dead_code)]
pub(crate) fn os_text(e: impl std::fmt::Display) -> String {
    let text: String = e.to_string().chars().map(|c| if c.is_control() { ' ' } else { c }).take(200).collect();
    let text = text.trim();
    if text.is_empty() { "no reason given".into() } else { text.to_string() }
}
