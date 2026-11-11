//! One animated value you own.

use core::fmt;

use crate::class::{Class, Eps, MotionPrefs};
use crate::curve::Curve;
use crate::lanes::{Animatable, Lanes, MAX_LANES, lane_count};

/// The display density `Animated` assumes until told otherwise: 2, so spatial values settle
/// within a quarter of a physical pixel on a high-density display too.
pub const DEFAULT_PIXELS_PER_POINT: f32 = 2.0;

/// A value that moves toward its target along a [`Curve`]. Time is in seconds on the caller's
/// clock (f64), the same clock for every call.
///
/// A retarget samples position and velocity at `now` and starts the new motion from there, so
/// a value interrupted mid-flight never jumps or loses its speed. Once settled, the value is
/// exactly its target.
pub struct Animated<T: Animatable> {
    /// Start of the current motion, per lane.
    from: Lanes,
    /// Velocity at the start, per lane.
    vel: Lanes,
    /// Where it comes to rest, per lane.
    target: Lanes,
    curve: Curve,
    class: Class,
    eps: Eps,
    /// When the current motion started.
    t0: f64,
    /// When it is settled for good; `NEG_INFINITY` at rest.
    settle_at: f64,
    /// The target as given, returned exactly once settled (lanes may not round-trip bit for bit).
    rest: T,
}

impl<T: Animatable> Clone for Animated<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Animatable> Copy for Animated<T> {}

impl<T: Animatable> fmt::Debug for Animated<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = lane_count::<T>();
        f.debug_struct("Animated")
            .field("from", &self.from.get(..n))
            .field("vel", &self.vel.get(..n))
            .field("target", &self.target.get(..n))
            .field("curve", &self.curve)
            .field("class", &self.class)
            .field("t0", &self.t0)
            .field("settle_at", &self.settle_at)
            .finish()
    }
}

impl<T: Animatable> Animated<T> {
    /// A value at rest at `value`.
    pub fn new(value: T, class: Class) -> Self {
        let lanes = sane_lanes::<T>(value.to_lanes(), &[0.0; MAX_LANES]);
        let rest = exact(value, lanes);
        Animated {
            from: lanes,
            vel: [0.0; MAX_LANES],
            target: lanes,
            curve: Curve::Instant,
            class,
            eps: class.eps(DEFAULT_PIXELS_PER_POINT),
            t0: 0.0,
            settle_at: f64::NEG_INFINITY,
            rest,
        }
    }

    /// Settle thresholds for a display with `pixels_per_point` (from the next motion on).
    pub fn set_pixels_per_point(&mut self, pixels_per_point: f32) {
        self.eps = self.class.eps(pixels_per_point);
    }

    /// The value's class.
    pub fn class(&self) -> Class {
        self.class
    }

    /// Where the value comes to rest.
    pub fn target(&self) -> T {
        self.rest
    }

    /// The curve of the current motion ([`Curve::Instant`] at rest or after a jump).
    pub fn curve(&self) -> Curve {
        self.curve
    }

    /// Jump to `value` and stop: for direct manipulation, where the value follows the user's hand.
    pub fn set(&mut self, value: T) {
        let lanes = sane_lanes::<T>(value.to_lanes(), &self.target);
        self.rest_at(lanes, exact(value, lanes));
    }

    /// Move toward `target` along `curve`, from wherever the value is at `now` and with its
    /// current velocity. Under `prefs.reduced` the class's reduced curve is used instead
    /// ([`Class::reduce`]). Returns false and does nothing when `target` is the current target
    /// (so calling this every frame with the same target is free).
    pub fn animate_to(&mut self, target: T, curve: Curve, now: f64, prefs: &MotionPrefs) -> bool {
        let lanes = sane_lanes::<T>(target.to_lanes(), &self.target);
        if lanes == self.target {
            return false;
        }
        let now = self.sane_now(now);
        let (x, v) = self.state(now);
        self.start(x, v, lanes, exact(target, lanes), curve, now, prefs);
        true
    }

    /// Let go with `velocity` (per lane, units per second) and move toward `target` along
    /// `curve`, from wherever the value is at `now`: the end of a drag or fling. Always restarts.
    pub fn release(&mut self, velocity: T, target: T, curve: Curve, now: f64, prefs: &MotionPrefs) {
        let lanes = sane_lanes::<T>(target.to_lanes(), &self.target);
        let v = sane_lanes::<T>(velocity.to_lanes(), &[0.0; MAX_LANES]);
        let now = self.sane_now(now);
        let (x, _) = self.state(now);
        self.start(x, v, lanes, exact(target, lanes), curve, now, prefs);
    }

    /// The value at `now`.
    pub fn sample(&self, now: f64) -> T {
        if self.is_settled(now) { self.rest } else { T::from_lanes(self.sample_lanes(now)) }
    }

    /// The velocity at `now`, per lane in units per second (0 once settled). For a type whose
    /// lanes are not linear in the value (a colour), read it through [`Animated::velocity_lanes`].
    pub fn velocity(&self, now: f64) -> T {
        T::from_lanes(self.velocity_lanes(now))
    }

    /// The lanes of the value at `now`.
    pub fn sample_lanes(&self, now: f64) -> Lanes {
        self.state(now).0
    }

    /// The velocity lanes at `now` (all 0 once settled; 0 for a lane an effect class holds at
    /// its start or target).
    pub fn velocity_lanes(&self, now: f64) -> Lanes {
        self.state(now).1
    }

    /// Position and velocity lanes at `now`.
    fn state(&self, now: f64) -> (Lanes, Lanes) {
        let now = self.sane_now(now);
        if now >= self.settle_at {
            return (self.target, [0.0; MAX_LANES]);
        }
        let dt = (now - self.t0).max(0.0) as f32;
        let n = lane_count::<T>();
        let (mut x, mut v) = self.curve.sample_lanes(&self.from, &self.vel, &self.target, n, dt);
        if self.class.is_effect() {
            let lanes = x.iter_mut().zip(v.iter_mut()).zip(self.from.iter().zip(self.target.iter()));
            for ((x, v), (&from, &target)) in lanes.take(n) {
                let held = clamp_between(*x, from, target);
                if held != *x {
                    *x = held;
                    *v = 0.0;
                }
            }
        }
        (x, v)
    }

    /// When the value is settled for good (`f64::NEG_INFINITY` at rest). A caller requests
    /// frames only while `now < settle_at()`.
    pub fn settle_at(&self) -> f64 {
        self.settle_at
    }

    /// Whether the value is settled at `now` (it then is exactly its target).
    pub fn is_settled(&self, now: f64) -> bool {
        self.sane_now(now) >= self.settle_at
    }

    fn sane_now(&self, now: f64) -> f64 {
        if now.is_finite() { now } else { self.t0 }
    }

    fn rest_at(&mut self, lanes: Lanes, rest: T) {
        self.rest = rest;
        self.from = lanes;
        self.target = lanes;
        self.vel = [0.0; MAX_LANES];
        self.curve = Curve::Instant;
        self.settle_at = f64::NEG_INFINITY;
    }

    fn start(&mut self, x: Lanes, v: Lanes, target: Lanes, rest: T, curve: Curve, now: f64, prefs: &MotionPrefs) {
        let mut curve = curve.sanitized();
        if prefs.reduced {
            curve = self.class.reduce(curve);
        }
        if curve.is_instant() {
            self.rest_at(target, rest);
            return;
        }
        let n = lane_count::<T>();
        let mut target = target;
        if matches!(curve, Curve::Decay { .. }) {
            for (t, (&x, &v)) in target.iter_mut().zip(x.iter().zip(v.iter())).take(n) {
                *t = curve.rest(x, v, *t);
            }
        }
        let lanes = x.iter().zip(v.iter()).zip(target.iter()).take(n).map(|((&x, &v), &t)| (x, v, t));
        let settle = curve.settle_time_lanes(lanes, self.eps);
        // a decay rests where its momentum takes it, not at the given target
        let rest = if matches!(curve, Curve::Decay { .. }) { T::from_lanes(target) } else { rest };
        if settle <= 0.0 {
            self.rest_at(target, rest);
            return;
        }
        self.rest = rest;
        self.from = x;
        self.vel = v;
        self.target = target;
        self.curve = curve;
        self.t0 = now;
        self.settle_at = now + f64::from(settle);
    }
}

/// Lanes with non-finite values replaced by `fallback`'s and unused lanes zeroed.
fn sane_lanes<T: Animatable>(mut lanes: Lanes, fallback: &Lanes) -> Lanes {
    let n = lane_count::<T>();
    for (i, (x, &f)) in lanes.iter_mut().zip(fallback.iter()).enumerate() {
        if i >= n {
            *x = 0.0;
        } else if !x.is_finite() {
            *x = if f.is_finite() { f } else { 0.0 };
        }
    }
    lanes
}

/// `value` itself when its lanes are `lanes` (it was not sanitised), else the value of `lanes`.
fn exact<T: Animatable>(value: T, lanes: Lanes) -> T {
    let n = lane_count::<T>();
    if value.to_lanes().get(..n) == lanes.get(..n) { value } else { T::from_lanes(lanes) }
}

/// `x` held between `a` and `b` (either order).
fn clamp_between(x: f32, a: f32, b: f32) -> f32 {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    x.clamp(lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Easing, Edges, Spring};

    const P: MotionPrefs = MotionPrefs::DEFAULT;

    #[test]
    fn linear_tween_from_rest_is_linear() {
        let mut a = Animated::new(0.0_f32, Class::Spatial);
        assert!(a.animate_to(100.0, Curve::tween(2.0, Easing::LINEAR), 1.0, &P));
        for t in [0.0_f64, 0.1, 0.525, 1.0, 1.5, 1.99] {
            let x = a.sample(1.0 + t);
            assert!((x - 50.0 * t as f32).abs() < 1e-4, "t={t}: {x}");
        }
        assert_eq!(a.sample(3.0), 100.0);
    }

    #[test]
    fn lands_exactly_and_settles() {
        let mut a = Animated::new(0.0_f32, Class::Spatial);
        assert!(a.is_settled(0.0));
        assert!(a.animate_to(100.0, Spring::new(0.3, 0.2).into(), 1.0, &P));
        assert!(!a.animate_to(100.0, Spring::new(0.3, 0.2).into(), 1.1, &P));
        assert!(!a.is_settled(1.1));
        let s = a.settle_at();
        assert!(s > 1.0 && s < 3.0, "{s}");
        assert_eq!(a.sample(s), 100.0);
        assert_eq!(a.velocity(s + 1.0), 0.0);
        assert!(a.is_settled(s));
    }

    #[test]
    fn effects_never_overshoot() {
        let mut a = Animated::new(0.0_f32, Class::Opacity);
        a.animate_to(1.0, Spring::new(0.3, 0.9).into(), 0.0, &P);
        let mut t = 0.0;
        while t < 3.0 {
            let x = a.sample(t);
            assert!((0.0..=1.0).contains(&x), "{x}");
            t += 0.002;
        }
    }

    #[test]
    fn reduced_motion() {
        let r = MotionPrefs::REDUCED;
        let mut rect = Animated::new(Edges::new(0.0, 0.0, 10.0, 10.0), Class::Spatial);
        rect.animate_to(Edges::new(50.0, 0.0, 60.0, 10.0), Curve::DEFAULT, 0.0, &r);
        assert!(rect.is_settled(0.0));
        assert_eq!(rect.sample(0.0), Edges::new(50.0, 0.0, 60.0, 10.0));
        let mut o = Animated::new(0.0_f32, Class::Opacity);
        o.animate_to(1.0, Spring::new(1.0, 0.0).into(), 0.0, &r);
        assert!(o.settle_at() <= 0.2);
        assert!(o.sample(0.09) > 0.0 && o.sample(0.09) < 1.0);
        let mut g = Animated::new(0.0_f32, Class::Gesture);
        g.release(500.0, 0.0, Curve::DEFAULT, 0.0, &r);
        assert!(g.settle_at() > 0.2);
    }

    #[test]
    fn decay_coasts_to_projection() {
        let mut a = Animated::new(10.0_f32, Class::Gesture);
        a.release(100.0, 0.0, Curve::Decay { k: 5.0 }, 0.0, &P);
        assert!((a.target() - 30.0).abs() < 1e-4);
        assert_eq!(a.sample(a.settle_at()), a.target());
    }

    #[test]
    fn set_jumps() {
        let mut a = Animated::new(0.0_f32, Class::Spatial);
        a.animate_to(10.0, Curve::DEFAULT, 0.0, &P);
        a.set(3.0);
        assert!(a.is_settled(0.1));
        assert_eq!(a.sample(0.1), 3.0);
        assert_eq!(a.target(), 3.0);
    }
}
