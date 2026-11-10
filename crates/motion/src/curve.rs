//! How a value travels from where it is to its target.

use crate::class::Eps;
use crate::easing::Easing;
use crate::lanes::Lanes;
use crate::spring::{MAX_SETTLE, Spring, finite_f32, sane_state};

/// How a value moves to its target. Every curve starts from the value's current position *and
/// velocity*, so a retarget mid-flight never jumps or snaps its speed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    /// A spring (the default for interface motion).
    Spring(Spring),
    /// A timed curve: `duration` seconds along `easing`. A starting velocity fades out over the
    /// duration, so it is kept at the start and the value still lands at `duration`.
    Tween {
        /// Seconds; 0 or less jumps, NaN is 0.3, capped at 60.
        duration: f32,
        /// The easing curve.
        easing: Easing,
    },
    /// Momentum: the value coasts on its velocity and slows at `k` per second, resting at
    /// `x + v / k`. The target is ignored (see `gesture::project` for where it rests).
    Decay {
        /// Deceleration rate per second, clamped to 0.05..=1000 (NaN is 2, about a normal scroll).
        k: f32,
    },
    /// Jump to the target.
    Instant,
}

impl Default for Curve {
    fn default() -> Self {
        Curve::DEFAULT
    }
}

impl From<Spring> for Curve {
    fn from(s: Spring) -> Self {
        Curve::Spring(s)
    }
}

const DEFAULT_TWEEN: f32 = 0.3;
const MAX_TWEEN: f32 = 60.0;
const DEFAULT_DECAY: f32 = 2.0;

impl Curve {
    /// [`Spring::DEFAULT`].
    pub const DEFAULT: Curve = Curve::Spring(Spring::DEFAULT);

    /// A tween of `duration` seconds (sanitised like the field docs say).
    pub const fn tween(duration: f32, easing: Easing) -> Curve {
        Curve::Tween { duration, easing }.sanitized()
    }

    /// Momentum with a per-millisecond deceleration `rate` (0.998 normal, 0.99 fast; see
    /// `gesture::deceleration`).
    pub fn decay(rate_per_ms: f32) -> Curve {
        Curve::Decay { k: crate::gesture::decay_k(rate_per_ms) }
    }

    /// The same curve with its public fields in range: a zero-length tween or spring becomes
    /// [`Curve::Instant`].
    pub const fn sanitized(self) -> Curve {
        match self {
            Curve::Spring(s) => {
                // re-clamp in case it came from somewhere odd; NONE means instant
                let s = Spring::new(s.duration(), s.bounce());
                if s.is_none() { Curve::Instant } else { Curve::Spring(s) }
            }
            Curve::Tween { duration, easing } => {
                let d = if duration.is_nan() { DEFAULT_TWEEN } else { duration };
                if d <= 0.0 {
                    Curve::Instant
                } else {
                    let [x1, y1, x2, y2] = easing.points();
                    Curve::Tween { duration: d.min(MAX_TWEEN), easing: Easing::cubic_bezier(x1, y1, x2, y2) }
                }
            }
            Curve::Decay { k } => Curve::Decay { k: if k.is_nan() { DEFAULT_DECAY } else { k.clamp(0.05, 1000.0) } },
            Curve::Instant => Curve::Instant,
        }
    }

    /// Whether the value jumps.
    pub const fn is_instant(self) -> bool {
        matches!(self.sanitized(), Curve::Instant)
    }

    /// Where a motion from `(x0, v0)` toward `target` comes to rest: the target, or for
    /// [`Curve::Decay`] the projected point `x0 + v0 / k`.
    pub fn rest(self, x0: f32, v0: f32, target: f32) -> f32 {
        let (x0, v0, target) = sane_state(x0, v0, target);
        match self.sanitized() {
            Curve::Decay { k } => finite_f32(f64::from(x0) + f64::from(v0) / f64::from(k), x0),
            _ => target,
        }
    }

    /// Position and velocity `t` seconds into a motion from `(x0, v0)` toward `target`. Always
    /// finite; `t ≤ 0` (or NaN) is the start.
    pub fn sample(self, x0: f32, v0: f32, target: f32, t: f32) -> (f32, f32) {
        let (x0, v0, target) = sane_state(x0, v0, target);
        if t.is_nan() || t <= 0.0 {
            return (x0, v0);
        }
        match self.sanitized() {
            Curve::Spring(s) => s.sample(x0, v0, target, t),
            Curve::Tween { duration, easing } => tween(duration, easing, x0, v0, target, t),
            Curve::Decay { k } => {
                let (k, v) = (f64::from(k), f64::from(v0));
                let e = (-k * f64::from(t)).exp();
                (finite_f32(f64::from(x0) + v / k * (1.0 - e), x0), finite_f32(v * e, 0.0))
            }
            Curve::Instant => (target, 0.0),
        }
    }

    /// Seconds until a motion from `(x0, v0)` toward `target` stays within `eps` for good (the
    /// duration for a tween, 0 for instant, at most [`MAX_SETTLE`]).
    pub fn settle_time(self, x0: f32, v0: f32, target: f32, eps: Eps) -> f32 {
        self.settle_time_lanes([(x0, v0, target)], eps)
    }

    /// The settle time of several lanes `(x0, v0, target)` moving together along this curve:
    /// never earlier than any lane's (see [`Spring::settle_time_lanes`]).
    pub fn settle_time_lanes(self, lanes: impl IntoIterator<Item = (f32, f32, f32)>, eps: Eps) -> f32 {
        match self.sanitized() {
            Curve::Spring(s) => s.settle_time_lanes(lanes, eps),
            Curve::Tween { duration, .. } => duration,
            Curve::Decay { k } => {
                let eps = eps.sanitized();
                let v = lanes.into_iter().map(|(x0, v0, target)| sane_state(x0, v0, target).1.abs()).fold(0.0_f32, f32::max);
                let (k, v) = (f64::from(k), f64::from(v));
                // remaining distance v/k·e^{-kt} and speed v·e^{-kt}
                let t = (v / (k * f64::from(eps.pos))).ln().max((v / f64::from(eps.vel)).ln()).max(0.0) / k;
                let t = t * (1.0 + 1e-4) + 1e-4;
                if t.is_finite() { (t as f32).min(MAX_SETTLE) } else { 0.0 }
            }
            Curve::Instant => 0.0,
        }
    }

    /// Position and velocity of each lane `(x0, v0, target)` at `t`, for up to four lanes, with
    /// the curve's constants worked out once.
    pub(crate) fn sample_lanes(self, from: &Lanes, vel: &Lanes, target: &Lanes, n: usize, t: f32) -> (Lanes, Lanes) {
        let mut x = *target;
        let mut v = [0.0; crate::lanes::MAX_LANES];
        let lanes = x.iter_mut().zip(v.iter_mut()).zip(from.iter().zip(vel.iter()));
        for ((x, v), (&from, &vel)) in lanes.take(n) {
            let (px, pv) = self.sample(from, vel, *x, t);
            *x = px;
            *v = pv;
        }
        (x, v)
    }
}

/// `x0 + Δ·ease(u) + w·h(t)`, where `h = t(1 − u)²` carries the start velocity away by the end
/// (`h(0) = 0, h'(0) = 1, h(d) = h'(d) = 0`) and `w` makes the start velocity exactly `v0`.
/// From rest (`v0 == 0`) there is no motion to continue, so `w = 0` and the tween follows its
/// easing exactly, like a CSS transition (a linear tween is linear).
fn tween(duration: f32, easing: Easing, x0: f32, v0: f32, target: f32, t: f32) -> (f32, f32) {
    if t >= duration {
        return (target, 0.0);
    }
    let d = f64::from(duration);
    let t = f64::from(t);
    let u = t / d;
    let delta = f64::from(target) - f64::from(x0);
    let w = if v0 == 0.0 { 0.0 } else { f64::from(v0) - delta * f64::from(easing.slope(0.0)) / d };
    let uf = u as f32;
    let x = f64::from(x0) + delta * f64::from(easing.ease(uf)) + w * t * (1.0 - u) * (1.0 - u);
    let v = delta * f64::from(easing.slope(uf)) / d + w * (1.0 - u) * (1.0 - 3.0 * u);
    (finite_f32(x, target), finite_f32(v, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tween_keeps_start_velocity_and_lands() {
        for e in [Easing::LINEAR, Easing::EASE, Easing::EASE_OUT] {
            let c = Curve::tween(0.5, e);
            let (x, v) = c.sample(0.0, 7.0, 10.0, 1e-6);
            assert!(x.abs() < 1e-3);
            // ease-out's slope changes like √u near 0, so allow a little
            assert!((v - 7.0).abs() < 5e-2, "{e:?} {v}");
            assert_eq!(c.sample(0.0, 7.0, 10.0, 0.5), (10.0, 0.0));
            let (x, _) = c.sample(0.0, 7.0, 10.0, 0.4999);
            assert!((x - 10.0).abs() < 1e-2, "{e:?} {x}");
        }
    }

    #[test]
    fn decay_rests_at_projection() {
        let c = Curve::Decay { k: 4.0 };
        assert!((c.rest(1.0, 8.0, 99.0) - 3.0).abs() < 1e-6);
        let t = c.settle_time(1.0, 8.0, 99.0, Eps::new(1e-3, 1e-2));
        let (x, v) = c.sample(1.0, 8.0, 99.0, t);
        assert!((x - 3.0).abs() <= 1e-3 && v.abs() <= 1e-2);
    }

    #[test]
    fn sanitize() {
        assert!(Curve::tween(0.0, Easing::EASE).is_instant());
        assert!(Curve::Spring(Spring::NONE).is_instant());
        assert_eq!(Curve::Decay { k: f32::NAN }.sanitized(), Curve::Decay { k: 2.0 });
        assert_eq!(Curve::tween(f32::INFINITY, Easing::EASE), Curve::Tween { duration: 60.0, easing: Easing::EASE });
    }
}
