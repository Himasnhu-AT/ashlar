//! A native menu bar described by a plain, serializable [`MenuSpec`].
//!
//! The model lives in [`ashlar_command::menu`] (one model for every renderer; `ashlar-ui` draws
//! the same spec in the window). Its types are re-exported here so `ashlar_native::menu::MenuSpec`
//! keeps working; new code may use either path.
//!
//! The app builds the spec from its state every frame (cheap: plain data), and
//! [`NativeMenuBar::update`] turns it into the OS menu: it rebuilds only when
//! [`MenuSpec::structure_hash`] changes (items added or removed, accelerators changed) and
//! otherwise pushes label / enabled / checked changes in place, only when
//! [`MenuSpec::state_hash`] changes. Activations arrive as item ids on a channel, with a
//! [`crate::Waker`] call so an idle app wakes up.
//!
//! The spec has no OS or UI types, builds everywhere (the web uses an in-window menu bar drawn
//! from the same spec) and is tested without creating real menus. The native bar exists on macOS
//! (the app's menu bar) and Windows (a window's menu bar, see [`NativeMenuBar::attach_window`]);
//! elsewhere [`NativeMenuBar::install`] returns `Unsupported`.

mod accelerator;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use accelerator::to_muda;
#[allow(deprecated)]
pub use accelerator::{Accelerator, Modifiers, menu_accelerator};
pub use ashlar_command::menu::{Item, MAX_DEPTH, MAX_NODES, MAX_TEXT, MenuNode, MenuSpec, Path, Role, Submenu, SubmenuRole, menu_chord};
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use native::NativeMenuBar;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use crate::{NativeError, Result};

/// What [`NativeMenuBar::update`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Update {
    Unchanged,
    /// Labels / enabled / checked state pushed in place.
    Patched,
    /// The native menus were rebuilt.
    Rebuilt,
}

/// No native menu bar on this platform: every call reports `Unsupported` (draw the spec in the
/// window instead).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct NativeMenuBar(());

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl NativeMenuBar {
    pub fn install(_spec: &MenuSpec, _waker: crate::Waker) -> Result<Self> {
        Err(NativeError::Unsupported)
    }
    pub fn attach_window(&self, _window: &impl raw_window_handle::HasWindowHandle) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn update(&mut self, _spec: &MenuSpec) -> Result<Update> {
        Err(NativeError::Unsupported)
    }
    pub fn try_recv(&self) -> Option<String> {
        None
    }
    pub fn drain(&self) -> Vec<String> {
        vec![]
    }
}
