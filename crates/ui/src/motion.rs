//! Animated values for egui, built on [`ashlar_motion`]: one call per value per frame, frames
//! requested only while something moves.
//!
//! ```rust,ignore
//! use ashlar_ui::motion;
//! let x = motion::f32(ctx, id, if open { 240.0 } else { 0.0 }, ashlar_motion::tokens::SNAP.enter);
//! let alpha = motion::opacity(ctx, id.with("fade"), visible);
//! ```
//!
//! - The first call for an id returns its target: nothing animates on first show.
//! - While a value is unsettled the call asks for the next frame ([`Reason::ANIMATION`]); once
//!   settled it returns exactly its target and asks for nothing, so an idle app stays at zero
//!   frames.
//! - Values live in the context, keyed by [`egui::Id`] and value type; a value not asked for in
//!   [`PRUNE_AFTER`] frames is forgotten (it shows at its target the next time).
//! - Motion time is egui's input time plus the predicted frame time (when the frame is shown),
//!   scaled by [`MotionPrefs::time_scale`]; time scale 0 freezes every value where it is.
//! - Under [`MotionPrefs::reduced`] spatial values jump and opacity and colour fade briefly
//!   (see [`Class::reduce`]); [`set_prefs`] also turns off egui's own widget animations.

use std::any::{Any, TypeId};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ashlar_motion::color::Rgba;
use ashlar_motion::{Animatable, Animated, Class, Curve, Edges, MotionPrefs, Pair, tokens};
use egui::{Color32, Id, IdMap, Rect, pos2};

use crate::wake::{Reason, wake};

/// A value not asked for in this many frames is forgotten.
pub const PRUNE_AFTER: u64 = 120;
/// How often (in frames) forgotten values are swept.
const PRUNE_EVERY: u64 = 60;

/// The animated value for `id`, moving toward `target` along `curve` whenever `target` changes.
/// Returns the value to draw this frame (exactly `target` once settled, and on the first call).
pub fn value<T>(ctx: &egui::Context, id: Id, target: T, class: Class, curve: impl Into<Curve>) -> T
where
    T: Animatable + Send + Sync + 'static,
{
    step(ctx, id, Op::To { target, class, curve: curve.into() }).0
}

/// An `f32` ([`Class::Other`]) animated toward `target`.
pub fn f32(ctx: &egui::Context, id: Id, target: f32, curve: impl Into<Curve>) -> f32 {
    value(ctx, id, target, Class::Other, curve)
}

/// Opacity 0..=1 for something shown (`visible`) or hidden: fades in with [`tokens::SNAP`]'s
/// enter spring and out with its shorter exit spring. Never overshoots.
pub fn opacity(ctx: &egui::Context, id: Id, visible: bool) -> f32 {
    let target = if visible { 1.0 } else { 0.0 };
    value(ctx, id, target, Class::Opacity, tokens::SNAP.pick(visible))
}

/// Keeps an item drawn while it leaves. Returns `Some(progress)` (0 = gone, 1 = fully shown)
/// while the item should be drawn and `None` once it has left. It comes in along `pair.enter`
/// and leaves along `pair.exit`, as an opacity: it never overshoots, and under reduced motion it
/// takes the short fade. The first call for an id returns `Some(1.0)` when shown and `None` when
/// hidden (nothing animates on first show); to animate an item's arrival anyway (a new toast),
/// [`set`] its id to `0.0_f32` first.
pub fn presence(ctx: &egui::Context, id: Id, shown: bool, pair: Pair) -> Option<f32> {
    let target = if shown { 1.0_f32 } else { 0.0 };
    let (p, settled) = step(ctx, id, Op::To { target, class: Class::Opacity, curve: pair.pick(shown).into() });
    if !shown && settled { None } else { Some(p.clamp(0.0, 1.0)) }
}

/// A colour animated in premultiplied OKLab (no muddy midpoints, no dark fringe when fading to
/// transparent). Returns exactly `target` once settled.
pub fn color(ctx: &egui::Context, id: Id, target: Color32, curve: impl Into<Curve>) -> Color32 {
    let (shown, settled) = step(ctx, id, Op::To { target: to_rgba(target), class: Class::Color, curve: curve.into() });
    if settled { target } else { from_rgba(shown) }
}

/// A rectangle animated by its edges (an edge that does not move stays still). Returns exactly
/// `target` once settled.
pub fn rect(ctx: &egui::Context, id: Id, target: Rect, curve: impl Into<Curve>) -> Rect {
    let edges = Edges::new(target.min.x, target.min.y, target.max.x, target.max.y);
    let (e, settled) = step(ctx, id, Op::To { target: edges, class: Class::Spatial, curve: curve.into() });
    if settled { target } else { Rect::from_min_max(pos2(e.left, e.top), pos2(e.right, e.bottom)) }
}

/// Jump the value for `id` to `value` and stop: for direct manipulation, where the value follows
/// the user's hand. A new id starts as a [`Class::Gesture`] value.
pub fn set<T>(ctx: &egui::Context, id: Id, value: T)
where
    T: Animatable + Send + Sync + 'static,
{
    step(ctx, id, Op::Set(value));
}

/// Let go of the value for `id` with `velocity` (units per second, per lane) and move it toward
/// `target` along `curve`: the end of a drag or fling. A new id starts at `target`.
pub fn release<T>(ctx: &egui::Context, id: Id, velocity: T, target: T, curve: impl Into<Curve>)
where
    T: Animatable + Send + Sync + 'static,
{
    step(ctx, id, Op::Release { velocity, target, curve: curve.into() });
}

/// The context's motion preferences.
pub fn prefs(ctx: &egui::Context) -> MotionPrefs {
    Motion::of(ctx).lock().prefs
}

/// Set the context's motion preferences. A new time scale applies from now on (values neither
/// jump nor rewind); reduced motion also sets egui's `animation_time` to 0 (restored when
/// reduced motion is turned off).
pub fn set_prefs(ctx: &egui::Context, prefs: MotionPrefs) {
    let prefs = MotionPrefs::new(prefs.reduced, prefs.time_scale);
    let egui_now = egui_now(ctx);
    let current_animation_time = ctx.global_style().animation_time;
    let motion = Motion::of(ctx);
    let animation_time = {
        let mut m = motion.lock();
        let old = m.prefs;
        if prefs.scale() != old.scale() {
            let t = m.clock.now(egui_now);
            m.clock.reanchor(t, egui_now, prefs.scale());
        }
        m.prefs = prefs;
        match (old.reduced, prefs.reduced) {
            (false, true) => {
                m.saved_animation_time = Some(current_animation_time);
                Some(0.0)
            }
            (true, false) => m.saved_animation_time.take(),
            _ => None,
        }
    };
    if let Some(a) = animation_time {
        ctx.all_styles_mut(|s| s.animation_time = a);
    }
    // values frozen by a time scale of 0, or slowed, need a frame to move on
    wake(ctx, Reason::ANIMATION);
}

/// How many values are still moving (for stats and the control port).
pub fn active(ctx: &egui::Context) -> usize {
    let motion = Motion::of(ctx);
    let m = motion.lock();
    let t = m.clock.last;
    m.stores.iter().map(|(_, s)| s.active(t)).sum()
}

#[cfg(test)]
fn slot_count(ctx: &egui::Context) -> usize {
    let motion = Motion::of(ctx);
    let m = motion.lock();
    m.stores.iter().map(|(_, s)| s.len()).sum()
}

enum Op<T> {
    To { target: T, class: Class, curve: Curve },
    Set(T),
    Release { velocity: T, target: T, curve: Curve },
}

/// Apply `op` to the value for `id`; returns the value to draw and whether it is settled.
fn step<T>(ctx: &egui::Context, id: Id, op: Op<T>) -> (T, bool)
where
    T: Animatable + Send + Sync + 'static,
{
    let (time, egui_now, ppp) = ctx.input(|i| (i.time, now_of(i), i.pixels_per_point));
    // all inside one short hold of the context's data (no context calls in here)
    let (out, ask) = Motion::with(ctx, |m| {
        let t = m.clock.now(egui_now);
        let pass = m.begin_pass(time);
        let prefs = m.prefs;
        // test recording (`motion_gate`): one `Option` check when off
        let rec = m.recorder.clone();
        let Some(slots) = m.store::<T>() else {
            return match op {
                Op::To { target, .. } | Op::Set(target) | Op::Release { target, .. } => ((target, true), false),
            };
        };
        let (out, moving) = match slots.get_mut(&id) {
            None => {
                let held = matches!(op, Op::Set(_));
                let (value, class) = match op {
                    Op::To { target, class, .. } => (target, class),
                    Op::Set(v) => (v, Class::Gesture),
                    Op::Release { target, .. } => (target, Class::Gesture),
                };
                let mut anim = Animated::new(value, class);
                anim.set_pixels_per_point(ppp);
                let mut slot = Slot { anim, used: pass };
                let moving = if let Op::Release { velocity, target, curve } = op {
                    slot.anim.release(velocity, target, curve, t, &prefs);
                    !slot.anim.is_settled(t)
                } else {
                    false
                };
                let out = (slot.anim.sample(t), !moving);
                if let Some(r) = &rec {
                    r.push(sample_of(id, t, time, &slot.anim, held));
                }
                slots.insert(id, slot);
                (out, moving)
            }
            Some(slot) => {
                slot.used = pass;
                let anim = &mut slot.anim;
                let held = matches!(op, Op::Set(_));
                match op {
                    Op::To { target, class, curve } => {
                        if anim.class() != class {
                            // the same id reused for another kind of value: carry on from where
                            // it is, with the new class's thresholds
                            *anim = Animated::new(anim.sample(t), class);
                        }
                        anim.set_pixels_per_point(ppp);
                        anim.animate_to(target, curve, t, &prefs);
                    }
                    Op::Set(v) => anim.set(v),
                    Op::Release { velocity, target, curve } => {
                        anim.set_pixels_per_point(ppp);
                        anim.release(velocity, target, curve, t, &prefs);
                    }
                }
                let settled = anim.is_settled(t);
                if let Some(r) = &rec {
                    r.push(sample_of(id, t, time, anim, held));
                }
                ((anim.sample(t), settled), !settled)
            }
        };
        // one request per pass is enough; a frozen clock (time scale 0) asks for nothing
        let ask = moving && !m.woke && m.prefs.scale() > 0.0;
        if ask {
            m.woke = true;
        }
        (out, ask)
    });
    if ask {
        wake(ctx, Reason::ANIMATION);
    }
    out
}

/// What the motion gate records for one value this frame.
fn sample_of<T: Animatable>(id: Id, t: f64, time: f64, anim: &Animated<T>, held: bool) -> crate::motion_gate::Sample {
    crate::motion_gate::Sample {
        id,
        t,
        time,
        lanes: T::LANES,
        value: anim.sample_lanes(t),
        velocity: anim.velocity_lanes(t),
        target: anim.target().to_lanes(),
        class: anim.class(),
        bounce: match anim.curve() {
            Curve::Spring(s) => s.bounce(),
            _ => 0.0,
        },
        held,
    }
}

/// Install (or with `None` remove) the motion gate's recorder.
pub(crate) fn set_recorder(ctx: &egui::Context, rec: Option<crate::motion_gate::Sink>) {
    Motion::with(ctx, |m| m.recorder = rec);
}

fn now_of(i: &egui::InputState) -> f64 {
    let dt = if i.predicted_dt.is_finite() { f64::from(i.predicted_dt.clamp(0.0, 1.0)) } else { 0.0 };
    i.time + dt
}

fn egui_now(ctx: &egui::Context) -> f64 {
    ctx.input(now_of)
}

/// egui `Color32` (premultiplied, sRGB-encoded) to linear straight-alpha `Rgba`.
fn to_rgba(c: Color32) -> Rgba {
    let [r, g, b, a] = egui::Rgba::from(c).to_rgba_unmultiplied();
    Rgba::new(r, g, b, a)
}

fn from_rgba(c: Rgba) -> Color32 {
    let c = c.clamped();
    Color32::from(egui::Rgba::from_rgba_unmultiplied(c.r, c.g, c.b, c.a))
}

/// Motion time: egui time (at presentation) scaled, re-anchored whenever the scale changes so
/// values never jump. Never runs backwards.
#[derive(Debug)]
struct Clock {
    /// egui time and motion time at the last re-anchor; `None` until first read.
    anchor: Option<(f64, f64)>,
    scale: f64,
    /// The latest motion time handed out.
    last: f64,
}

impl Clock {
    fn now(&mut self, egui_now: f64) -> f64 {
        let egui_now = if egui_now.is_finite() { egui_now } else { self.anchor.map_or(0.0, |a| a.0) };
        let (e0, m0) = *self.anchor.get_or_insert((egui_now, egui_now));
        let t = m0 + (egui_now - e0) * self.scale;
        self.last = if t.is_finite() { t.max(self.last) } else { self.last };
        self.last
    }

    fn reanchor(&mut self, motion_now: f64, egui_now: f64, scale: f32) {
        if egui_now.is_finite() {
            self.anchor = Some((egui_now, motion_now));
        }
        self.scale = f64::from(scale);
    }
}

struct Slot<T: Animatable> {
    anim: Animated<T>,
    /// The pass this value was last asked for.
    used: u64,
}

/// The values of one type.
trait Store: Send {
    fn as_any(&mut self) -> &mut dyn Any;
    fn prune(&mut self, pass: u64);
    fn active(&self, t: f64) -> usize;
    #[cfg(test)]
    fn len(&self) -> usize;
}

impl<T: Animatable + Send + 'static> Store for IdMap<Slot<T>> {
    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
    fn prune(&mut self, pass: u64) {
        self.retain(|_, s| pass.saturating_sub(s.used) <= PRUNE_AFTER);
    }
    fn active(&self, t: f64) -> usize {
        self.values().filter(|s| !s.anim.is_settled(t)).count()
    }
    #[cfg(test)]
    fn len(&self) -> usize {
        IdMap::len(self)
    }
}

struct Inner {
    prefs: MotionPrefs,
    clock: Clock,
    /// One store per value type (few types: a linear scan beats hashing).
    stores: Vec<(TypeId, Box<dyn Store>)>,
    /// Frames seen (see [`Inner::begin_pass`]) and the egui time of the latest one.
    pass: u64,
    pass_time: u64,
    next_prune: u64,
    /// Whether this pass has asked for a frame already.
    woke: bool,
    /// egui's `animation_time` from before reduced motion turned it off.
    saved_animation_time: Option<f32>,
    /// The motion gate's recorder, when a test installed one.
    recorder: Option<crate::motion_gate::Sink>,
}

impl Default for Inner {
    fn default() -> Self {
        Inner {
            prefs: MotionPrefs::DEFAULT,
            clock: Clock { anchor: None, scale: 1.0, last: f64::NEG_INFINITY },
            stores: Vec::new(),
            pass: 0,
            pass_time: f64::NAN.to_bits(),
            next_prune: PRUNE_EVERY,
            woke: false,
            saved_animation_time: None,
            recorder: None,
        }
    }
}

impl Inner {
    /// Note the frame at egui time `time` and return its number. A frame is told apart by its
    /// time (egui advances it every frame; extra passes within one frame share it), which costs
    /// nothing where reading the context's pass counter would take another lock per call.
    fn begin_pass(&mut self, time: f64) -> u64 {
        let bits = time.to_bits();
        if bits == self.pass_time {
            return self.pass;
        }
        self.pass_time = bits;
        self.pass = self.pass.wrapping_add(1);
        self.woke = false;
        if self.pass >= self.next_prune {
            self.next_prune = self.pass.saturating_add(PRUNE_EVERY);
            for (_, s) in &mut self.stores {
                s.prune(self.pass);
            }
        }
        self.pass
    }

    /// The values of type `T` (always `Some`: the store's key is its own type id).
    fn store<T: Animatable + Send + 'static>(&mut self) -> Option<&mut IdMap<Slot<T>>> {
        let key = TypeId::of::<IdMap<Slot<T>>>();
        let at = match self.stores.iter().position(|(k, _)| *k == key) {
            Some(at) => at,
            None => {
                self.stores.push((key, Box::new(IdMap::<Slot<T>>::default())));
                self.stores.len() - 1
            }
        };
        self.stores.get_mut(at)?.1.as_any().downcast_mut::<IdMap<Slot<T>>>()
    }
}

/// The context's motion state: a handle shared through egui's data (cheap to clone).
#[derive(Clone, Default)]
struct Motion(Arc<Mutex<Inner>>);

impl Motion {
    fn of(ctx: &egui::Context) -> Motion {
        ctx.data_mut(|d| d.get_temp_mut_or_default::<Motion>(Id::new("ashlar.motion")).clone())
    }

    /// Run `f` on the motion state while holding the context's data (`f` must not call the
    /// context).
    fn with<R>(ctx: &egui::Context, f: impl FnOnce(&mut Inner) -> R) -> R {
        ctx.data_mut(|d| f(&mut d.get_temp_mut_or_default::<Motion>(Id::new("ashlar.motion")).lock()))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wake::Scheduler;
    use ashlar_motion::Spring;

    const DT: f64 = 1.0 / 60.0;

    /// One pass at egui time `t`.
    fn pass<R>(ctx: &egui::Context, t: f64, mut f: impl FnMut(&egui::Context) -> R) -> R {
        let input = egui::RawInput { time: Some(t), predicted_dt: DT as f32, ..Default::default() };
        let mut out = None;
        ctx.run_ui(input, |ui| out = Some(f(ui.ctx()))).textures_delta.clear();
        out.expect("the pass ran")
    }

    fn requests(ctx: &egui::Context) -> u64 {
        Scheduler::of(ctx).stats().requests_for(Reason::ANIMATION)
    }

    fn id() -> Id {
        Id::new("x")
    }

    #[test]
    fn linear_tween_from_rest_is_linear_over_egui_time() {
        let ctx = egui::Context::default();
        let curve = Curve::tween(2.0, ashlar_motion::Easing::LINEAR);
        pass(&ctx, 0.0, |c| f32(c, id(), 0.0, curve));
        let t0 = 1.0;
        for k in 0..=150 {
            let t = t0 + k as f64 * DT;
            let x = pass(&ctx, t, |c| f32(c, id(), 100.0, curve));
            let want = (100.0 * (t - t0) / 2.0).min(100.0) as f32;
            // one frame of slack for where the clock anchors the start
            assert!((x - want).abs() <= 100.0 * DT as f32 / 2.0 + 1e-3, "t={t}: {x} vs {want}");
        }
    }

    #[test]
    fn first_call_returns_target_and_requests_nothing() {
        let ctx = egui::Context::default();
        let v = pass(&ctx, 0.0, |c| f32(c, id(), 42.0, tokens::SNAP.enter));
        assert_eq!(v, 42.0);
        let o = pass(&ctx, DT, |c| opacity(c, Id::new("o"), true));
        assert_eq!(o, 1.0);
        assert_eq!(requests(&ctx), 0);
        assert_eq!(active(&ctx), 0);
    }

    #[test]
    fn lands_exactly_and_wakes_stop_on_the_settle_frame() {
        let ctx = egui::Context::default();
        pass(&ctx, 0.0, |c| f32(c, id(), 0.0, tokens::SNAP.enter));
        let mut t = 0.0;
        let mut landed = None;
        for k in 1..300 {
            t = k as f64 * DT;
            let before = requests(&ctx);
            let v = pass(&ctx, t, |c| f32(c, id(), 1.0, tokens::SNAP.enter));
            if v == 1.0 {
                // the frame that lands asks for nothing more
                assert_eq!(requests(&ctx), before, "settle frame requested a frame");
                landed = Some(k);
                break;
            }
            assert!((0.0..1.0 + 1e-3).contains(&v), "v = {v}");
            assert_eq!(requests(&ctx), before + 1, "moving frame {k} did not request one frame");
        }
        let k = landed.unwrap_or(usize::MAX);
        assert!(k < 60, "took {k} frames");
        assert_eq!(active(&ctx), 0);
        let flat = requests(&ctx);
        for j in 1..=100 {
            let v = pass(&ctx, t + j as f64 * DT, |c| f32(c, id(), 1.0, tokens::SNAP.enter));
            assert_eq!(v, 1.0);
        }
        assert_eq!(requests(&ctx), flat);
    }

    #[test]
    fn color_and_rect_land_exactly() {
        let ctx = egui::Context::default();
        let (c0, c1) = (Color32::from_rgb(200, 30, 30), Color32::from_rgba_premultiplied(10, 20, 120, 200));
        let (r0, r1) = (Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0)), Rect::from_min_max(pos2(5.5, 1.0), pos2(80.3, 40.7)));
        pass(&ctx, 0.0, |c| (color(c, id(), c0, tokens::SNAP.enter), rect(c, id(), r0, tokens::SNAP.enter)).0.a());
        let mut last = (c0, r0);
        for k in 1..120 {
            last = pass(&ctx, k as f64 * DT, |c| (color(c, id(), c1, tokens::SNAP.enter), rect(c, id(), r1, tokens::SNAP.enter)));
        }
        assert_eq!(last, (c1, r1));
        assert_eq!(active(&ctx), 0);
    }

    #[test]
    fn reduced_motion_jumps_rects_and_fades_briefly() {
        let ctx = egui::Context::default();
        pass(&ctx, 0.0, |c| set_prefs(c, MotionPrefs::REDUCED));
        assert_eq!(ctx.global_style().animation_time, 0.0);
        let (a, b) = (Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(10.0, 10.0)), Rect::from_min_size(pos2(300.0, 0.0), egui::vec2(10.0, 10.0)));
        pass(&ctx, DT, |c| rect(c, id(), a, tokens::REFLOW.enter));
        assert_eq!(pass(&ctx, 2.0 * DT, |c| rect(c, id(), b, tokens::REFLOW.enter)), b);

        pass(&ctx, 3.0 * DT, |c| opacity(c, id(), false));
        let start = 4.0 * DT;
        let mut done = f64::INFINITY;
        for k in 0..60 {
            let t = start + k as f64 * DT;
            if pass(&ctx, t, |c| opacity(c, id(), true)) == 1.0 {
                done = t - start;
                break;
            }
        }
        assert!(done <= 0.2 + 1e-9, "fade took {done} s");

        pass(&ctx, 5.0, |c| set_prefs(c, MotionPrefs::DEFAULT));
        assert!(ctx.global_style().animation_time > 0.0);
    }

    #[test]
    fn time_scale_zero_freezes_and_resumes() {
        let ctx = egui::Context::default();
        pass(&ctx, 0.0, |c| f32(c, id(), 0.0, tokens::REFLOW.enter));
        let mut v = 0.0;
        for k in 1..6 {
            v = pass(&ctx, k as f64 * DT, |c| f32(c, id(), 100.0, tokens::REFLOW.enter));
        }
        assert!(v > 0.0 && v < 100.0);
        pass(&ctx, 6.0 * DT, |c| set_prefs(c, MotionPrefs::new(false, 0.0)));
        let frozen = pass(&ctx, 7.0 * DT, |c| f32(c, id(), 100.0, tokens::REFLOW.enter));
        let before = requests(&ctx);
        for k in 8..60 {
            assert_eq!(pass(&ctx, k as f64 * DT, |c| f32(c, id(), 100.0, tokens::REFLOW.enter)), frozen);
        }
        assert_eq!(requests(&ctx), before, "a frozen clock asked for frames");
        assert_eq!(active(&ctx), 1);
        pass(&ctx, 60.0 * DT, |c| set_prefs(c, MotionPrefs::DEFAULT));
        let resumed = pass(&ctx, 61.0 * DT, |c| f32(c, id(), 100.0, tokens::REFLOW.enter));
        assert!(resumed > frozen && resumed - frozen < 20.0, "{frozen} -> {resumed}");
    }

    #[test]
    fn retarget_keeps_velocity_continuous() {
        let ctx = egui::Context::default();
        let h = 5e-4;
        let curve = Spring::new(0.4, 0.0);
        let x = |c: &egui::Context, target: f32| value(c, id(), target, Class::Spatial, curve);
        pass(&ctx, 0.0, |c| x(c, 0.0));
        let mut xs = Vec::new();
        for k in 1..=203 {
            let target = if k < 200 { 100.0 } else { 40.0 };
            xs.push(f64::from(pass(&ctx, k as f64 * h, |c| x(c, target))));
        }
        // xs[k - 1] is frame k; the retarget happens on frame 200
        let at = |k: usize| xs.get(k - 1).copied().unwrap_or(f64::NAN);
        let before = (at(199) - at(198)) / h;
        let after = (at(201) - at(200)) / h;
        assert!(before > 100.0, "before = {before}");
        assert!((after - before).abs() < 0.1 * before, "velocity jumped: {before} -> {after}");
        // and the position did not jump either
        assert!((at(200) - at(199)).abs() < before * h * 1.5);
    }

    #[test]
    fn unused_slots_are_pruned() {
        let ctx = egui::Context::default();
        for k in 0..5 {
            pass(&ctx, k as f64 * DT, |c| {
                f32(c, Id::new("gone"), k as f32, tokens::SNAP.enter);
                f32(c, Id::new("kept"), 1.0, tokens::SNAP.enter)
            });
        }
        assert_eq!(slot_count(&ctx), 2);
        for k in 5..(5 + PRUNE_AFTER + 2 * PRUNE_EVERY) {
            pass(&ctx, k as f64 * DT, |c| f32(c, Id::new("kept"), 1.0, tokens::SNAP.enter));
        }
        assert_eq!(slot_count(&ctx), 1);
        // back after being forgotten: shows at its target, no animation
        let t = 1000.0;
        assert_eq!(pass(&ctx, t, |c| f32(c, Id::new("gone"), 7.0, tokens::SNAP.enter)), 7.0);
    }

    #[test]
    fn set_follows_the_hand_and_release_moves_on() {
        let ctx = egui::Context::default();
        pass(&ctx, 0.0, |c| set(c, id(), 10.0_f32));
        pass(&ctx, DT, |c| set(c, id(), 20.0_f32));
        assert_eq!(active(&ctx), 0);
        pass(&ctx, 2.0 * DT, |c| release(c, id(), 500.0_f32, 0.0, tokens::SETTLE.enter));
        assert_eq!(active(&ctx), 1);
        let mut v = f32::NAN;
        for k in 3..200 {
            v = pass(&ctx, k as f64 * DT, |c| value(c, id(), 0.0_f32, Class::Gesture, tokens::SETTLE.enter));
        }
        assert_eq!(v, 0.0);
    }
    #[test]
    fn presence_keeps_a_leaving_item_drawn_until_it_settles() {
        let ctx = egui::Context::default();
        let pid = Id::new("p");
        // first show: no animation either way
        assert_eq!(pass(&ctx, 0.0, |c| presence(c, pid, true, tokens::SNAP)), Some(1.0));
        assert_eq!(pass(&ctx, 0.0, |c| presence(c, Id::new("hidden"), false, tokens::SNAP)), None);
        assert_eq!(requests(&ctx), 0);
        let mut last = 1.0;
        let mut gone = None;
        for k in 1..120 {
            match pass(&ctx, k as f64 * DT, |c| presence(c, pid, false, tokens::SNAP)) {
                Some(p) => {
                    assert!(p <= last, "progress went back up at frame {k}");
                    last = p;
                }
                None => {
                    gone = Some(k);
                    break;
                }
            }
        }
        let k = gone.expect("the item left");
        assert!(k > 3, "left too soon ({k} frames)");
        let flat = requests(&ctx);
        for j in 1..=100 {
            assert_eq!(pass(&ctx, (k + j) as f64 * DT, |c| presence(c, pid, false, tokens::SNAP)), None);
        }
        assert_eq!(requests(&ctx), flat);
    }

    #[test]
    fn presence_enters_from_zero_when_seeded() {
        let ctx = egui::Context::default();
        let pid = Id::new("p");
        let first = pass(&ctx, 0.0, |c| {
            set(c, pid, 0.0_f32);
            presence(c, pid, true, tokens::SNAP)
        });
        assert_eq!(first, Some(0.0));
        let mut last = 0.0;
        for k in 1..120 {
            let p = pass(&ctx, k as f64 * DT, |c| presence(c, pid, true, tokens::SNAP)).expect("shown");
            assert!(p >= last);
            last = p;
        }
        assert_eq!(last, 1.0);
    }
}
