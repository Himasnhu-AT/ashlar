//! Small content components: [`EmptyState`], [`Banner`] (a live region), [`Card`],
//! [`Collapsible`] and [`Accordion`] (at most one section open).
//!
//! Like the [`controls`](crate::controls) they take every string from the caller, register a
//! dotted automation id (`<id>`, plus `<id>.action` and `<id>.dismiss` for their buttons), fill
//! their AccessKit nodes and paint from the design tokens. None of them animates: a collapsible
//! opens and closes in one frame, so they need no reduced-motion handling and ask for no frames.
//!
//! ```
//! # use ashlar_ui::content::{Banner, Card, Collapsible, EmptyState};
//! # use ashlar_ui::controls::Tone;
//! # fn ui(ui: &mut egui::Ui, advanced_open: &mut bool, offline: bool) {
//! if offline {
//!     let out = Banner::new("sync.offline", Tone::Warning, "Changes are kept on this computer until the connection is back.")
//!         .title("Offline")
//!         .action("Retry")
//!         .show(ui);
//!     if out.action { /* reconnect */ }
//! }
//! Card::new("settings.account").title("Account").show(ui, |ui| { ui.label("Signed in"); });
//! Collapsible::new("settings.advanced", "Advanced", advanced_open).show(ui, |ui| { ui.label("…"); });
//! if EmptyState::new("files.empty", "No files yet").body("Drop files here or create one.").action("New file").show(ui).action {
//!     // create a file
//! }
//! # }
//! ```

mod banner;
mod card;
mod collapsible;
mod empty_state;

pub use banner::{Banner, BannerOutput};
pub use card::Card;
pub(crate) use collapsible::chevron as chevron_points;
pub use collapsible::{Accordion, Collapsible, CollapsibleOutput};
pub use empty_state::{EmptyState, EmptyStateOutput};

#[cfg(test)]
mod tests;
