#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Small, reusable egui components for desktop apps.
//!
//! Applications own their assets, themes, layouts, commands and document semantics. Components
//! accept focused style values and return egui responses; no application engine is required.

pub mod automation;
pub mod buttons;
pub mod cache;
pub mod dialog;
pub mod docking;
pub mod edit;
pub mod fonts;
pub mod icons;
pub mod layout;
pub mod printing;
pub mod search;
pub mod slider;
pub mod status_bar;
pub mod swatches;
pub mod tabs;
pub mod theme;
pub mod toasts;
pub mod tokens;
pub mod tree;
pub mod value_field;
pub mod virtual_list;

#[cfg(test)]
pub(crate) mod test_support;
