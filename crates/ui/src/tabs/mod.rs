//! Tab strips: fitting tab widths with an overflow menu, and an accessible tab hit-region that
//! the caller paints.
//!
//! The caller owns everything else: ids, which tab is selected, labels, the overflow menu, close
//! buttons, context menus and drags.

use egui::accesskit::Role;
use egui::{Id, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType};

use crate::layout::{EXTENT, usable};

/// The outcome of [`fit`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabFit {
    /// Tabs to draw, in index order, with the width each gets.
    pub shown: Vec<(usize, f32)>,
    /// Tabs that only appear in the overflow menu, in index order.
    pub overflow: Vec<usize>,
    /// Width reserved for the overflow button (0 when nothing overflows).
    pub overflow_width: f32,
}

/// Bisection passes used to find the common width cap.
const SQUEEZE_PASSES: usize = 32;

fn clean(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, EXTENT) } else { 0.0 }
}

/// Fit tabs of `natural` widths into `available` points, keeping `selected` visible.
///
/// Tabs shrink before any is hidden: the widest give way first, down to `minimum_width` (a tab
/// narrower than that keeps its natural width). When even that does not fit, an overflow button
/// `overflow_width` wide is reserved, the selected tab is always kept, and other tabs are kept in
/// order while their minimum still fits. Every dimension is sanitized (non-finite → 0, clamped to
/// one million), so hostile input gives finite, non-negative widths that never add up to more
/// than `available`. Runs in linear time.
pub fn fit(natural: &[f32], selected: usize, available: f32, minimum_width: f32, overflow_width: f32) -> TabFit {
    let widths: Vec<f32> = natural.iter().map(|&w| clean(w)).collect();
    let available = clean(available);
    let minimum = clean(minimum_width);
    let Some(last) = widths.len().checked_sub(1) else { return TabFit::default() };

    let total: f64 = widths.iter().map(|&w| f64::from(w)).sum();
    if total <= f64::from(available) {
        return TabFit { shown: widths.iter().copied().enumerate().collect(), overflow: Vec::new(), overflow_width: 0.0 };
    }
    if last == 0 {
        return TabFit { shown: vec![(0, available)], overflow: Vec::new(), overflow_width: 0.0 };
    }
    let floor = |w: f32| w.min(minimum);
    let floors: f64 = widths.iter().map(|&w| f64::from(floor(w))).sum();
    if floors <= f64::from(available) {
        let all: Vec<usize> = (0..widths.len()).collect();
        return TabFit { shown: squeeze(&widths, &all, minimum, available), overflow: Vec::new(), overflow_width: 0.0 };
    }

    let overflow_width = clean(overflow_width).min(available);
    let budget = f64::from(available - overflow_width).max(0.0);
    let selected = selected.min(last);
    let mut used = f64::from(floor(widths.get(selected).copied().unwrap_or(0.0)));
    let keep: Vec<bool> = widths
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            if i == selected {
                return true;
            }
            let fits = used + f64::from(floor(w)) <= budget;
            if fits {
                used += f64::from(floor(w));
            }
            fits
        })
        .collect();
    let (shown, overflow): (Vec<usize>, Vec<usize>) = (0..widths.len()).partition(|&i| keep.get(i).copied().unwrap_or(false));
    TabFit { shown: squeeze(&widths, &shown, minimum, budget as f32), overflow, overflow_width }
}

/// Widths for the tabs at `indices` that add up to at most `total`: each tab is capped at a
/// common width (never below its own floor), found by bisection; any rounding left over is taken
/// from the last tabs, which can then shrink below their floor, down to zero.
fn squeeze(widths: &[f32], indices: &[usize], minimum: f32, total: f32) -> Vec<(usize, f32)> {
    let width_at = |i: usize, cap: f64| {
        let natural = f64::from(widths.get(i).copied().unwrap_or(0.0));
        natural.min(cap.max(natural.min(f64::from(minimum))))
    };
    let sum_at = |cap: f64| indices.iter().map(|&i| width_at(i, cap)).sum::<f64>();
    let total = f64::from(total);
    let widest = indices.iter().filter_map(|&i| widths.get(i)).fold(0.0f32, |a, &b| a.max(b));
    let (mut lo, mut hi) = (f64::from(minimum), f64::from(widest.max(minimum)));
    let cap = if sum_at(hi) <= total {
        hi
    } else {
        for _ in 0..SQUEEZE_PASSES {
            let mid = (lo + hi) / 2.0;
            if sum_at(mid) <= total {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    };
    let mut remaining = total;
    indices
        .iter()
        .map(|&i| {
            let w = width_at(i, cap).min(remaining).max(0.0);
            remaining = (remaining - w).max(0.0);
            (i, w as f32)
        })
        .collect()
}

/// One tab's interaction and accessibility; the caller paints it.
///
/// It senses at the given rect without allocating space, reports itself to AccessKit as a tab
/// with its selected state, and draws a focus outline inside the rect after the caller's painter.
/// Enter and Space activate a focused tab (egui's click handling). It never changes the
/// selection: read `clicked()` and do that yourself.
pub struct Tab<'a> {
    id: Id,
    label: &'a str,
    selected: bool,
    sense: Sense,
    focus_stroke: Option<Stroke>,
}

impl<'a> Tab<'a> {
    /// `id` must be unique and stable (two tabs with the same label still need two ids);
    /// `label` is the accessible name.
    pub fn new(id: Id, label: &'a str, selected: bool) -> Self {
        Self { id, label, selected, sense: Sense::click(), focus_stroke: None }
    }

    /// What the tab senses (default: clicks). Use `Sense::click_and_drag()` for draggable tabs.
    pub fn sense(mut self, sense: Sense) -> Self {
        self.sense = sense;
        self
    }

    /// The keyboard focus outline (default: the theme's selection stroke). `Stroke::NONE`
    /// disables it.
    pub fn focus_stroke(mut self, stroke: Stroke) -> Self {
        self.focus_stroke = Some(stroke);
        self
    }

    /// Interact at `rect` and call `paint` with the response. A non-finite, empty or oversized
    /// rect gives an inert response and `paint` is not called.
    pub fn show_at(self, ui: &mut Ui, rect: Rect, paint: impl FnOnce(&Ui, &Response)) -> Response {
        if !usable(rect) {
            let response = ui.interact(Rect::from_min_size(Pos2::ZERO, Vec2::ZERO), self.id, Sense::hover());
            response.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, false, self.selected, self.label));
            return response;
        }
        let response = ui.interact(rect, self.id, self.sense);
        let enabled = ui.is_enabled();
        response.widget_info(|| WidgetInfo::selected(WidgetType::SelectableLabel, enabled, self.selected, self.label));
        ui.ctx().accesskit_node_builder(self.id, |node| {
            node.set_role(Role::Tab);
            node.clear_toggled();
            node.set_selected(self.selected);
        });

        paint(ui, &response);

        if response.has_focus() && rect.width() > 4.0 && rect.height() > 4.0 {
            let stroke = self.focus_stroke.unwrap_or(ui.visuals().selection.stroke);
            let inner = rect.shrink(2.0);
            if stroke.width.is_finite() && stroke.width > 0.0 {
                let width = stroke.width.min(inner.width().min(inner.height()) / 2.0);
                ui.painter().rect_stroke(inner, 0.0, Stroke::new(width, stroke.color), StrokeKind::Inside);
            }
        }
        response
    }
}

#[cfg(test)]
mod tests;
