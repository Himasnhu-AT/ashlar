//! Two-pane split arithmetic.

use egui::{Rect, Vec2, pos2};

/// Which way a split divides its rect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SplitAxis {
    /// Side by side: the first pane on the left, the second on the right.
    Horizontal,
    /// Stacked: the first pane on top, the second below.
    Vertical,
}

impl SplitAxis {
    /// The component of `v` along this axis.
    pub(crate) fn along(self, v: Vec2) -> f32 {
        match self {
            SplitAxis::Horizontal => v.x,
            SplitAxis::Vertical => v.y,
        }
    }

    /// `amount` placed on this axis, zero across it.
    pub(crate) fn vec(self, amount: f32) -> Vec2 {
        match self {
            SplitAxis::Horizontal => Vec2::new(amount, 0.0),
            SplitAxis::Vertical => Vec2::new(0.0, amount),
        }
    }
}

/// How the space of a split is shared out.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SplitSize {
    /// The first pane gets this fraction of the space left after the divider.
    Ratio(f32),
    /// The first pane is this many points long; the second takes the rest.
    FixedFirst(f32),
    /// The second pane is this many points long; the first takes the rest.
    FixedSecond(f32),
}

impl SplitSize {
    fn payload(self) -> f32 {
        match self {
            SplitSize::Ratio(v) | SplitSize::FixedFirst(v) | SplitSize::FixedSecond(v) => v,
        }
    }
}

/// The result of [`split_rect`]: the two panes and the divider between them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplitLayout {
    pub first: Rect,
    pub divider: Rect,
    pub second: Rect,
    first_size: f32,
    available: f32,
}

impl SplitLayout {
    /// Length of the first pane along the split axis, after clamping.
    pub fn first_size(self) -> f32 {
        self.first_size
    }

    /// Space shared by the two panes (the rect's length minus the divider).
    pub fn available(self) -> f32 {
        self.available
    }

    /// The size that moves the divider by `delta` points, in the same variant as `size`.
    ///
    /// The new first length keeps both panes at least `minimum` long (or half the space each, if
    /// that is less). `None` for non-finite input, a negative `minimum`, or no space at all.
    pub fn resized(self, size: SplitSize, delta: f32, minimum: f32) -> Option<SplitSize> {
        if !size.payload().is_finite() || !delta.is_finite() || !minimum.is_finite() || minimum < 0.0 {
            return None;
        }
        let space = f64::from(self.available);
        if space <= 0.0 {
            return None;
        }
        let low = f64::from(minimum).min(space / 2.0);
        let first = (f64::from(self.first_size) + f64::from(delta)).clamp(low, space - low);
        Some(match size {
            SplitSize::Ratio(_) => SplitSize::Ratio((first / space) as f32),
            SplitSize::FixedFirst(_) => SplitSize::FixedFirst(first as f32),
            SplitSize::FixedSecond(_) => SplitSize::FixedSecond((space - first) as f32),
        })
    }
}

/// Split `rect` along `axis` into two panes separated by a `gap`-long divider.
///
/// Each pane keeps at least `minimum` points when there is room, and half the space each when
/// there is not; requests outside that range are clamped rather than rejected. Returns `None`
/// for a non-finite or inverted rect, a negative or non-finite `gap`/`minimum`, or a non-finite
/// size. Zero-sized rects are fine and produce zero-sized panes.
pub fn split_rect(rect: Rect, axis: SplitAxis, size: SplitSize, gap: f32, minimum: f32) -> Option<SplitLayout> {
    let finite = rect.min.is_finite() && rect.max.is_finite() && rect.width().is_finite() && rect.height().is_finite();
    if !finite || rect.min.x > rect.max.x || rect.min.y > rect.max.y {
        return None;
    }
    if !gap.is_finite() || gap < 0.0 || !minimum.is_finite() || minimum < 0.0 || !size.payload().is_finite() {
        return None;
    }
    let (start, end) = match axis {
        SplitAxis::Horizontal => (rect.min.x, rect.max.x),
        SplitAxis::Vertical => (rect.min.y, rect.max.y),
    };
    let total = f64::from(end) - f64::from(start);
    let gap = f64::from(gap).min(total);
    let space = total - gap;
    let wanted = match size {
        SplitSize::Ratio(r) => space * f64::from(r),
        SplitSize::FixedFirst(v) => f64::from(v),
        SplitSize::FixedSecond(v) => space - f64::from(v),
    };
    let low = f64::from(minimum).min(space / 2.0);
    let first = wanted.clamp(low, space - low);

    // Positions along the axis, kept inside [start, end] after rounding back to f32.
    let place = |offset: f64| ((f64::from(start) + offset) as f32).clamp(start, end);
    let first_end = place(first);
    let divider_end = place(first + gap).max(first_end);

    let span = |a: f32, b: f32| match axis {
        SplitAxis::Horizontal => Rect::from_min_max(pos2(a, rect.min.y), pos2(b, rect.max.y)),
        SplitAxis::Vertical => Rect::from_min_max(pos2(rect.min.x, a), pos2(rect.max.x, b)),
    };
    Some(SplitLayout {
        first: span(start, first_end),
        divider: span(first_end, divider_end),
        second: span(divider_end, end),
        first_size: first as f32,
        available: space as f32,
    })
}
