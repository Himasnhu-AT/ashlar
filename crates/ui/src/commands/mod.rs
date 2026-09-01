//! Commands in the UI: the menu bar, the command palette, the keymap editor and keyboard
//! dispatch, all driven by [`ashlar_command::CommandInfo`] and [`ashlar_command::Keymap`].
//!
//! The app lists its commands (`Registry::infos`) and runs the id these widgets return through
//! its one `execute` chokepoint; nothing here runs a command itself.

pub mod keymap_editor;
pub mod keys;
pub mod menu;
pub mod palette;

pub use keymap_editor::{KeymapChange, KeymapEditor};
pub use keys::{chord_from_egui, chord_to_egui, dispatch};
#[allow(deprecated)]
pub use menu::MenuItem;
pub use menu::{Item, MenuNode, MenuSpec, Submenu, menu_bar};
pub use palette::CommandPalette;
