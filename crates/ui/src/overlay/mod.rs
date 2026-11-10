//! Floating surfaces anchored to a control: [`Select`], [`Combobox`], [`Popover`] and the
//! [`help`] tooltip. Context menus built from a `MenuSpec` are
//! [`crate::commands::menu::context`].
//!
//! They share one behaviour: the surface opens below its anchor and flips above it when there is
//! more room there, Escape or a click outside closes it, it fades in and out with the
//! `enter_exit` motion token (a short fade under reduced motion) and asks for no frames once
//! settled. Options register as `<id>.option.<n>` for automation, where `n` is the option's
//! index in the slice the caller passed.
//!
//! ```
//! # use ashlar_ui::overlay::{Combobox, ComboboxState, Select};
//! # fn frame(ui: &mut egui::Ui, size: &mut usize, font: &mut ComboboxState) {
//! let sizes = ["Small", "Medium", "Large"];
//! if Select::new("size", &sizes, size).show(ui).changed() {
//!     // sizes[*size] was chosen
//! }
//! let fonts = ["Inter", "Iosevka", "JetBrains Mono"];
//! if let Some(i) = Combobox::new("font", &fonts, font).show(ui).chosen {
//!     // fonts[i] was chosen; font.text holds its label
//! }
//! # }
//! ```

mod combobox;
mod options;
mod popover;
mod select;
mod tooltip;

#[cfg(test)]
mod tests;

pub use combobox::{Combobox, ComboboxResponse, ComboboxState};
pub use popover::{Popover, place};
pub use select::Select;
pub use tooltip::help;

/// At most this many options are visible at once; the rest scroll.
pub const VISIBLE_OPTIONS: usize = 8;
