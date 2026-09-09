//! Edit phases: how a continuous gesture (a drag, a typed entry) maps onto one undo step.
//!
//! Value components ([`crate::value_field`], [`crate::slider`]) report an [`Edit`] whenever the
//! user changes their value:
//!
//! - [`EditPhase::Begin`] opens a gesture (a drag started, a text entry opened). Nothing has
//!   changed yet; open a history merge group here.
//! - [`EditPhase::Update`] carries a live value while the gesture continues. Apply it inside the
//!   open group (with a merge key, so the steps collapse into one).
//! - [`EditPhase::Commit`] closes the gesture with its final value. A `Commit` without a
//!   preceding `Begin` is a one-shot change (an arrow key step, a click on a slider track): one
//!   undo step on its own.
//! - [`EditPhase::Cancel`] abandons the gesture (Escape): `start` is the value to restore; roll the
//!   group back. The component has already written `start` back into the bound value.

/// Where in a gesture an edit is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditPhase {
    Begin,
    Update,
    Commit,
    Cancel,
}

/// One edit event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edit {
    pub phase: EditPhase,
    /// The value before the gesture began.
    pub start: f64,
    /// The value now (for `Cancel`, equal to `start`).
    pub value: f64,
}

impl Edit {
    pub fn begin(start: f64) -> Self {
        Edit { phase: EditPhase::Begin, start, value: start }
    }
    pub fn update(start: f64, value: f64) -> Self {
        Edit { phase: EditPhase::Update, start, value }
    }
    pub fn commit(start: f64, value: f64) -> Self {
        Edit { phase: EditPhase::Commit, start, value }
    }
    pub fn cancel(start: f64) -> Self {
        Edit { phase: EditPhase::Cancel, start, value: start }
    }
    /// Whether the bound value changed in this event.
    pub fn changed(&self) -> bool {
        match self.phase {
            EditPhase::Begin => false,
            EditPhase::Update | EditPhase::Commit => true,
            EditPhase::Cancel => true,
        }
    }
}

/// A numeric range made safe: finite bounds in order (or unbounded).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Bounds {
    pub min: f64,
    pub max: f64,
}

impl Bounds {
    pub fn new(range: &std::ops::RangeInclusive<f64>) -> Self {
        let (a, b) = (*range.start(), *range.end());
        let lo = if a.is_nan() { f64::MIN } else { a.max(f64::MIN) };
        let hi = if b.is_nan() { f64::MAX } else { b.min(f64::MAX) };
        if lo <= hi { Bounds { min: lo, max: hi } } else { Bounds { min: hi, max: lo } }
    }

    pub fn clamp(self, v: f64) -> f64 {
        if v.is_nan() { self.min.max(0.0f64.min(self.max)) } else { v.clamp(self.min, self.max) }
    }

    /// Position of `v` in the range as 0..=1 (0 for an empty or unbounded range).
    pub fn fraction(self, v: f64) -> f64 {
        let span = self.max - self.min;
        if span.is_finite() && span > 0.0 { ((self.clamp(v) - self.min) / span).clamp(0.0, 1.0) } else { 0.0 }
    }

    pub fn lerp(self, t: f64) -> f64 {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        self.clamp(self.min + (self.max - self.min) * t)
    }

    pub fn finite(self) -> bool {
        (self.max - self.min).is_finite()
    }
}

/// Round `v` to a multiple of `step` (when `step` is a positive finite number).
pub(crate) fn snap(v: f64, step: f64) -> f64 {
    if step.is_finite() && step > 0.0 && v.is_finite() {
        let snapped = (v / step).round() * step;
        if snapped.is_finite() { snapped } else { v }
    } else {
        v
    }
}

/// Drag gesture bookkeeping kept in egui memory between frames.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Drag {
    pub start: f64,
    /// Pointer travel accumulated (with modifier speed applied), in value units.
    pub travel: f64,
    /// The value the gesture points at now.
    pub target: f64,
    /// Escape was pressed: ignore the rest of this drag.
    pub cancelled: bool,
}

/// Speed multiplier from modifiers: Shift is coarse (×10), Alt (Option) or ⌘/Ctrl is fine (×0.1).
pub(crate) fn speed_multiplier(modifiers: egui::Modifiers) -> f64 {
    if modifiers.shift {
        10.0
    } else if modifiers.alt || modifiers.command {
        0.1
    } else {
        1.0
    }
}

/// How a drag maps to a value.
pub(crate) enum DragMode<F: FnOnce(egui::Vec2, f64) -> f64> {
    /// Pointer movement (and the modifier speed multiplier) → value change (fields, knobs).
    Relative(F),
    /// The value under the pointer, when known this frame (tracks).
    Absolute(Option<f64>),
}

/// Runs the drag part of a value gesture on `response`: Begin on drag start, Update while
/// moving, Commit on release, Cancel on Escape. Movement in the start and release frames counts,
/// so a quick flick still changes the value.
pub(crate) fn drive_drag<F: FnOnce(egui::Vec2, f64) -> f64>(
    ui: &egui::Ui,
    response: &egui::Response,
    value: &mut f64,
    bounds: Bounds,
    step: f64,
    mode: DragMode<F>,
) -> Option<Edit> {
    let key = response.id.with("ashlar_drag");
    let current = bounds.clamp(*value);
    let aim = |drag: &mut Drag| {
        let t = match mode {
            DragMode::Absolute(Some(abs)) => bounds.clamp(snap(abs, step)),
            DragMode::Absolute(None) => drag.target,
            DragMode::Relative(delta) => {
                drag.travel += delta(response.drag_delta(), speed_multiplier(ui.input(|i| i.modifiers)));
                bounds.clamp(snap(drag.start + drag.travel, step))
            }
        };
        if t.is_finite() {
            drag.target = t;
        }
    };
    if response.drag_started() {
        let mut drag = Drag { start: current, travel: 0.0, target: current, cancelled: false };
        // this frame's movement is remembered and applied with the first Update
        aim(&mut drag);
        ui.data_mut(|d| d.insert_temp(key, drag));
        return Some(Edit::begin(current));
    }
    let mut drag: Drag = ui.data(|d| d.get_temp(key))?;
    if response.drag_stopped() || !response.dragged() {
        // released, aborted by egui (it ends a drag on Escape), or the widget was hidden
        ui.data_mut(|d| d.remove::<Drag>(key));
        if drag.cancelled {
            return None;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            *value = drag.start;
            return Some(Edit::cancel(drag.start));
        }
        aim(&mut drag);
        *value = drag.target;
        return Some(Edit::commit(drag.start, drag.target));
    }
    if drag.cancelled {
        return None;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        drag.cancelled = true;
        ui.data_mut(|d| d.insert_temp(key, drag));
        *value = drag.start;
        return Some(Edit::cancel(drag.start));
    }
    aim(&mut drag);
    ui.data_mut(|d| d.insert_temp(key, drag));
    if drag.target != current {
        *value = drag.target;
        return Some(Edit::update(drag.start, drag.target));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_tolerate_hostile_ranges() {
        let b = Bounds::new(&(10.0..=-10.0));
        assert_eq!((b.min, b.max), (-10.0, 10.0));
        let b = Bounds::new(&(f64::NAN..=f64::INFINITY));
        assert_eq!((b.min, b.max), (f64::MIN, f64::MAX));
        assert!(!b.finite());
        assert_eq!(b.fraction(5.0), 0.0);
        assert_eq!(Bounds::new(&(0.0..=1.0)).clamp(f64::NAN), 0.0);
        assert_eq!(Bounds::new(&(2.0..=3.0)).clamp(f64::NAN), 2.0);
        assert_eq!(Bounds::new(&(0.0..=10.0)).lerp(f64::NAN), 0.0);
        assert_eq!(Bounds::new(&(0.0..=10.0)).fraction(2.5), 0.25);
    }

    #[test]
    fn snapping() {
        assert_eq!(snap(0.26, 0.25), 0.25);
        assert_eq!(snap(7.0, 0.0), 7.0);
        assert_eq!(snap(7.0, f64::NAN), 7.0);
        assert_eq!(snap(f64::MAX, 1e-300), f64::MAX);
    }
}
