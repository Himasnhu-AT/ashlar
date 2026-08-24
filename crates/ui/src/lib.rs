#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Small, reusable egui components for desktop apps.
//!
//! Applications own their assets, themes, layouts, commands and document semantics. Components
//! accept focused style values and return egui responses; no application engine is required.

pub mod automation;
pub mod buttons;
pub mod cache;
pub mod docking;
pub mod fonts;
pub mod icons;
pub mod layout;
pub mod printing;
pub mod tabs;
pub mod theme;
pub mod tokens;

#[cfg(test)]
pub(crate) mod test_support;
