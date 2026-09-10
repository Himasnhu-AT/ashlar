//! Layout building blocks that hold no state of their own.
//!
//! - [`split_rect`] divides a rect into two panes and a divider, and [`SplitLayout::resized`]
//!   turns a divider movement back into a new [`SplitSize`].
//! - [`ResizeHandle`] is the divider widget: pointer, keyboard and AccessKit input all come out
//!   as one per-frame delta; the caller decides what to do with it.
//! - [`fit_toolbar`] decides which toolbar groups fit and which move into an overflow menu.

mod geometry;
mod resize;
mod toolbar;

pub use geometry::{SplitAxis, SplitLayout, SplitSize, split_rect};
pub use resize::{ResizeHandle, ResizeHandleStyle, ResizeResponse};
pub use toolbar::{ToolbarFit, fit_toolbar};

/// Largest coordinate or length these modules accept (points).
pub(crate) const EXTENT: f32 = 1_000_000.0;

/// A rect worth laying anything out in: finite, with both sides positive and at most [`EXTENT`].
pub(crate) fn usable(rect: egui::Rect) -> bool {
    let (w, h) = (rect.width(), rect.height());
    rect.min.is_finite() && rect.max.is_finite() && w > 0.0 && h > 0.0 && w <= EXTENT && h <= EXTENT
}

#[cfg(test)]
mod tests;
