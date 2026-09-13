//! Panel docking: a layout of tab groups, accordions, splits and floating groups that apps change
//! only through validated, atomic transactions, plus drop-target resolution and an optional egui
//! renderer that reports what the user asked for as actions.
//!
//! - [`Layout`] and [`Action`]: the model, [`Layout::apply_with_permissions`] and
//!   [`Layout::location`] / [`Layout::restore`] for hiding and showing panels.
//! - [`resolve_drop`], [`drop_zone`], [`zone_rect`]: drop rules for apps with their own chrome.
//! - [`DockArea`]: the built-in renderer; it never mutates the layout.

mod model;
mod surface;
mod targets;

pub use model::{Action, DockError, Floating, Layout, Location, MAX_DEPTH, MAX_PANELS, Node, Permissions, Placement, StackEntry, Zone};
pub use surface::{DockArea, DockContent, DockOutput, DockStyle, PanelLimits, begin_drag, cancel_drag};
pub use targets::{DropProposal, DropTarget, drop_zone, resolve_drop, zone_rect};

#[cfg(test)]
mod tests;
