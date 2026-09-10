#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! General-purpose egui components for ashlar apps.
//!
//! - Theme: [`tokens`] (design tokens, light/dark/system, interface scale), [`fonts`] (install
//!   app-supplied faces with a CJK fallback chain), [`theme`] (widget-state colouring).
//! - Inputs: [`value_field`] (drag/scrub and formatted numeric fields), [`slider`] (slider, knob),
//!   [`swatches`], [`search`], [`buttons`], [`tabs`]; all value edits report [`edit`] phases.
//! - Data: [`virtual_list`] (list and table, O(visible) per frame), [`tree`].
//! - Shell: [`dialog`], [`toasts`], [`status_bar`], [`layout`], [`docking`], [`printing`].
//! - Commands: [`commands`] (menu model and bar, palette, keymap editor, key dispatch).
//! - Infrastructure: [`automation`] (ids and rects for `ui.inspect`/`ui.click`), [`devtools`]
//!   (the UI debugger: element tree with refs, injected input, waits, screenshots, inspector), [`cache`]
//!   (revision-keyed galleys and meshes), [`wake`] (frame requests with a reason, per-reason
//!   frame counts).
//!
//! Apps own their data, selection, assets and command handling: components take state by
//! reference and report what the user asked for. Domain widgets (timelines, canvases) stay in the
//! apps and are built from these pieces.

pub mod automation;
pub mod cache;
pub mod commands;
pub mod devtools;
pub mod dialog;
pub mod edit;
pub mod fonts;
pub mod layout;
pub mod search;
pub mod slider;
pub mod status_bar;
pub mod swatches;
#[cfg(feature = "telemetry")]
pub mod telemetry;
pub mod theme;
pub mod toasts;
pub mod tokens;
pub mod tree;
pub mod value_field;
pub mod virtual_list;
pub mod wake;

#[cfg(test)]
pub(crate) mod test_support;
