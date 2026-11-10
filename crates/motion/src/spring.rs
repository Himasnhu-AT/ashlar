//! Closed-form damped springs set by a duration and a bounce.
//!
//! A spring with duration `d` and bounce `b` has angular frequency `ω = 2π / d` and damping ratio
//! `ζ = 1 − b` for `b ≥ 0` and `ζ = 1 / (1 + b)` for `b < 0`: bounce 0 is critically damped, a
//! positive bounce overshoots, a negative one is overdamped. It is the SwiftUI-style
//! parameterisation, so a duration is roughly how long the motion feels, whatever the bounce.
//!
//! [`Spring::sample`] evaluates position and velocity in closed form (no stepping), so a sample
//! costs the same at any time and two samples at the same time always agree.

use crate::class::Eps;
use core::f64::consts::TAU;

/// The longest settle time, in seconds. A value still moving then snaps to its target.
pub const MAX_SETTLE: f32 = 120.0;

/// A damped spring of unit mass, set by `(duration, bounce)`. The fields are private so the
/// invariants hold: the duration is 0 (no spring, see [`Spring::NONE`]) or within
/// [`Spring::MIN_DURATION`]`..=`[`Spring::MAX_DURATION`], and the bounce is within
/// `-MAX_BOUNCE..=MAX_BOUNCE`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    duration: f32,
    bounce: f32,
}

impl Default for Spring {
    fn default() -> Self {
        Spring::DEFAULT
    }
}

/// The derived constants of a spring, in f64.
#[derive(Clone, Copy, Debug)]
struct Params {
    /// Decay rate `ζω`.
    alpha: f64,
    /// Undamped angular frequency `ω`.
    w0: f64,
    kind: Kind,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    /// ζ < 1, with the damped frequency `ω√(1 − ζ²)`.
    Under { wd: f64 },
    /// ζ = 1.
    Critical,
    /// ζ > 1, with `s = ω√(ζ² − 1)`.
    Over { s: f64 },
}

impl Spring {
    /// The shortest duration of a spring that moves, in seconds.
    pub const MIN_DURATION: f32 = 0.01;
    /// The longest duration, in seconds.
    pub const MAX_DURATION: f32 = 60.0;
    /// The largest bounce either way.
    pub const MAX_BOUNCE: f32 = 0.95;
    /// 0.3 s without bounce: the default for most interface motion.
    pub const DEFAULT: Spring = Spring { duration: 0.3, bounce: 0.0 };
    /// No spring: values jump to their target (treated like `Curve::Instant`).
    pub const NONE: Spring = Spring { duration: 0.0, bounce: 0.0 };

    /// A spring of `duration` seconds and `bounce` (−0.95..=0.95). Never panics: a NaN duration
    /// gives the default 0.3 s, zero or negative gives [`Spring::NONE`], other values are clamped
    /// to `MIN_DURATION..=MAX_DURATION`; a NaN bounce is 0 and others are clamped.
    pub const fn new(duration: f32, bounce: f32) -> Spring {
        let duration = if duration.is_nan() {
            Self::DEFAULT.duration
        } else if duration <= 0.0 {
            0.0
        } else {
            duration.clamp(Self::MIN_DURATION, Self::MAX_DURATION)
        };
        let bounce = if bounce.is_nan() { 0.0 } else { bounce.clamp(-Self::MAX_BOUNCE, Self::MAX_BOUNCE) };
        Spring { duration, bounce }
    }

    /// A spring from a response (the duration) and a damping ratio `ζ` (1 = critical, below 1
    /// bounces). Clamped like [`Spring::new`]; a NaN ratio is 1.
    pub const fn from_response(response: f32, damping_ratio: f32) -> Spring {
        let zeta = if damping_ratio.is_nan() { 1.0 } else { damping_ratio };
        let bounce = if zeta <= 0.0 {
            Self::MAX_BOUNCE
        } else if zeta <= 1.0 {
            1.0 - zeta
        } else {
            1.0 / zeta - 1.0
        };
        Spring::new(response, bounce)
    }

    /// The duration in seconds (0 for [`Spring::NONE`]).
    pub const fn duration(self) -> f32 {
        self.duration
    }

    /// The bounce, −0.95..=0.95.
    pub const fn bounce(self) -> f32 {
        self.bounce
    }

    /// Whether this is no spring at all (duration 0): values jump.
    pub const fn is_none(self) -> bool {
        self.duration <= 0.0
    }

    /// The damping ratio `ζ`.
    pub fn damping_ratio(self) -> f32 {
        zeta(self.bounce) as f32
    }

    /// The stiffness `k = ω²` for unit mass (0 for [`Spring::NONE`]).
    pub fn stiffness(self) -> f32 {
        self.params().map_or(0.0, |p| (p.w0 * p.w0) as f32)
    }

    /// The damping coefficient `c = 2ζω` for unit mass (0 for [`Spring::NONE`]).
    pub fn damping(self) -> f32 {
        self.params().map_or(0.0, |p| (2.0 * p.alpha) as f32)
    }

    fn params(self) -> Option<Params> {
        if self.is_none() {
            return None;
        }
        let w0 = TAU / f64::from(self.duration);
        let zeta = zeta(self.bounce);
        let alpha = zeta * w0;
        let kind = if zeta < 1.0 {
            let wd = w0 * (1.0 - zeta * zeta).sqrt();
            if wd > 0.0 { Kind::Under { wd } } else { Kind::Critical }
        } else if zeta > 1.0 {
            let s = w0 * (zeta * zeta - 1.0).sqrt();
            if s > 0.0 { Kind::Over { s } } else { Kind::Critical }
        } else {
            Kind::Critical
        };
        Some(Params { alpha, w0, kind })
    }

    /// Position and velocity `t` seconds after starting at `x0` with velocity `v0`, pulled to
    /// `target`. Closed form for all three damping regimes. Non-finite inputs are replaced
    /// (position by the target, velocity by 0, a bad target by the start), `t ≤ 0` returns the
    /// start, and the result is always finite.
    pub fn sample(self, x0: f32, v0: f32, target: f32, t: f32) -> (f32, f32) {
        let (x0, v0, target) = sane_state(x0, v0, target);
        if t.is_nan() || t <= 0.0 {
            return (x0, v0);
        }
        let Some(p) = self.params() else { return (target, 0.0) };
        let (x, v) = p.sample(f64::from(x0), f64::from(v0), f64::from(target), f64::from(t));
        (finite_f32(x, target), finite_f32(v, 0.0))
    }

    /// The time in seconds after which a motion from `(from, v0)` to `target` stays within
    /// `eps` (position and velocity) for good. Never earlier than the true settle (it is the last
    /// crossing of a bound of the motion), at most [`MAX_SETTLE`], and 0 when already settled.
    pub fn settle_time(self, from: f32, v0: f32, target: f32, eps: Eps) -> f32 {
        self.settle_time_lanes([(from, v0, target)], eps)
    }

    /// The settle time of several lanes `(from, v0, target)` moving together: one bound over all
    /// of them (each coefficient of the bound is the largest of any lane's), so it is never
    /// earlier than any lane's settle and costs one root solve.
    pub fn settle_time_lanes(self, lanes: impl IntoIterator<Item = (f32, f32, f32)>, eps: Eps) -> f32 {
        let Some(p) = self.params() else { return 0.0 };
        let mut bounds = Bounds::default();
        for (from, v0, target) in lanes {
            let (from, v0, target) = sane_state(from, v0, target);
            bounds = bounds.max(p.bounds(f64::from(from) - f64::from(target), f64::from(v0)));
        }
        let t = p.solve(&bounds, eps.sanitized());
        // a little later for f32 rounding of the samples near the boundary, then capped
        let t = t * (1.0 + 1e-4) + 1e-4;
        if t.is_finite() { (t as f32).clamp(0.0, MAX_SETTLE) } else { MAX_SETTLE }
    }
}

/// Non-negative coefficients of upper bounds on `|y(t)|` and `|v(t)|`; each bound only grows with
/// its coefficients, so the lane-wise maximum bounds every lane at once.
#[derive(Clone, Copy, Debug, Default)]
struct Bounds {
    /// `|y| ≤ (y_a + y_b·t)·e^{-rt}`, and the same for `v`.
    y_a: f64,
    y_b: f64,
    v_a: f64,
    v_b: f64,
    /// Underdamped: the envelope amplitude (`y_e·e^{-αt}`), `y_f` unused. Overdamped: the slow and
    /// fast mode amplitudes (`y_e·e^{r1 t} + y_f·e^{r2 t}`).
    y_e: f64,
    y_f: f64,
    v_e: f64,
    v_f: f64,
}

impl Bounds {
    fn max(self, o: Bounds) -> Bounds {
        Bounds {
            y_a: self.y_a.max(o.y_a),
            y_b: self.y_b.max(o.y_b),
            v_a: self.v_a.max(o.v_a),
            v_b: self.v_b.max(o.v_b),
            y_e: self.y_e.max(o.y_e),
            y_f: self.y_f.max(o.y_f),
            v_e: self.v_e.max(o.v_e),
            v_f: self.v_f.max(o.v_f),
        }
    }
}

impl Params {
    /// `(e^{-αt}·C(t), e^{-αt}·S(t))`, where the displacement is `y = y0·C + (v0 + α·y0)·S`
    /// (times the decay): `C = cos(ωd t), S = sin(ωd t)/ωd` underdamped, `C = 1, S = t` critical,
    /// `C = cosh(st), S = sinh(st)/s` overdamped. Written so nothing overflows for any `t`.
    fn decayed_c_s(&self, t: f64) -> (f64, f64) {
        match self.kind {
            Kind::Under { wd } => {
                let e = (-self.alpha * t).exp();
                let (sin, cos) = (wd * t).sin_cos();
                (e * cos, e * sin / wd)
            }
            Kind::Critical => {
                let e = (-self.w0 * t).exp();
                (e, e * t)
            }
            Kind::Over { s } => {
                let e1 = ((s - self.alpha) * t).exp();
                let e2 = (-(s + self.alpha) * t).exp();
                let x = s * t;
                let es = if x < 1e-3 {
                    // sinh(x)/s without cancellation
                    (-self.alpha * t).exp() * t * (1.0 + x * x / 6.0)
                } else {
                    (e1 - e2) / (2.0 * s)
                };
                ((e1 + e2) * 0.5, es)
            }
        }
    }

    fn sample(&self, x0: f64, v0: f64, target: f64, t: f64) -> (f64, f64) {
        let y0 = x0 - target;
        let (ec, es) = self.decayed_c_s(t);
        let y = y0 * ec + (v0 + self.alpha * y0) * es;
        let v = v0 * ec - (self.alpha * v0 + self.w0 * self.w0 * y0) * es;
        (target + y, v)
    }

    /// The bound coefficients of a motion with displacement `y0` and velocity `v0`. Both `y` and
    /// `v` have the form `e^{-αt}(a·C + b·S)`.
    fn bounds(&self, y0: f64, v0: f64) -> Bounds {
        let w2 = self.w0 * self.w0;
        // y = e(y0·C + by·S), v = e(v0·C − bv·S)
        let by = v0 + self.alpha * y0;
        let bv = self.alpha * v0 + w2 * y0;
        let (y_e, y_f, v_e, v_f) = match self.kind {
            // the exact envelope R·e^{-αt}
            Kind::Under { wd } => ((y0 * y0 + (by / wd) * (by / wd)).sqrt(), 0.0, (v0 * v0 + (bv / wd) * (bv / wd)).sqrt(), 0.0),
            Kind::Critical => (0.0, 0.0, 0.0, 0.0),
            // y = c1·e^{r1 t} + c2·e^{r2 t} with r2 < r1 < 0
            Kind::Over { s } => {
                let (r1, r2) = (s - self.alpha, -s - self.alpha);
                let c2 = (v0 - r1 * y0) / (r2 - r1);
                let c1 = y0 - c2;
                (c1.abs(), c2.abs(), (r1 * c1).abs(), (r2 * c2).abs())
            }
        };
        Bounds { y_a: y0.abs(), y_b: by.abs(), v_a: v0.abs(), v_b: bv.abs(), y_e, y_f, v_e, v_f }
    }

    /// The last time either bound exceeds its threshold: when `|y| ≤ eps.pos` and
    /// `|v| ≤ eps.vel` hold for good.
    fn solve(&self, b: &Bounds, eps: Eps) -> f64 {
        let (ep, ev) = (f64::from(eps.pos), f64::from(eps.vel));
        let (ty, tv) = match self.kind {
            // |C| ≤ 1 and |S| ≤ t, or the envelope
            Kind::Under { .. } => (
                linear_exp_root(b.y_a, b.y_b, self.alpha, ep).min(exp_root(b.y_e, self.alpha, ep)),
                linear_exp_root(b.v_a, b.v_b, self.alpha, ev).min(exp_root(b.v_e, self.alpha, ev)),
            ),
            Kind::Critical => (linear_exp_root(b.y_a, b.y_b, self.w0, ep), linear_exp_root(b.v_a, b.v_b, self.w0, ev)),
            // e^{-αt}cosh(st) ≤ e^{r1 t} and e^{-αt}sinh(st)/s ≤ t·e^{r1 t}; or the two modes
            Kind::Over { s } => {
                let (slow, fast) = (self.alpha - s, self.alpha + s);
                (
                    linear_exp_root(b.y_a, b.y_b, slow, ep).min(two_exp_root(b.y_e, slow, b.y_f, fast, ep)),
                    linear_exp_root(b.v_a, b.v_b, slow, ev).min(two_exp_root(b.v_e, slow, b.v_f, fast, ev)),
                )
            }
        };
        let t = ty.max(tv);
        if t.is_nan() { f64::from(MAX_SETTLE) } else { t.max(0.0) }
    }
}

fn zeta(bounce: f32) -> f64 {
    let b = f64::from(bounce);
    if b >= 0.0 { 1.0 - b } else { 1.0 / (1.0 + b) }
}

/// The time after which `a·e^{-rt} ≤ eps` for good (0 when it already is).
fn exp_root(a: f64, r: f64, eps: f64) -> f64 {
    if a.is_nan() || a <= eps {
        return 0.0;
    }
    if r.is_nan() || r <= 0.0 || !a.is_finite() {
        return f64::INFINITY;
    }
    (a / eps).ln() / r
}

/// The time at which `a·e^{-ra·t} + b·e^{-rb·t}` falls to `eps` (it only decreases), with
/// `ra ≤ rb`: a bracketed false position (Illinois) that returns the bracket's late end, so it is
/// never early.
fn two_exp_root(a: f64, ra: f64, b: f64, rb: f64, eps: f64) -> f64 {
    let f = |t: f64| a * (-ra * t).exp() + b * (-rb * t).exp() - eps;
    // the sum is at least a·e^{-ra·t} and at most (a + b)·e^{-ra·t}
    let (mut lo, mut hi) = (exp_root(a, ra, eps), exp_root(a + b, ra, eps));
    if !hi.is_finite() || hi <= lo {
        return hi;
    }
    let (mut flo, mut fhi) = (f(lo), f(hi));
    if flo <= 0.0 {
        // the fast term is gone by `lo`: the bound is under eps from there on
        return lo;
    }
    let mut side = 0i8;
    for _ in 0..24 {
        if flo.is_nan() || fhi.is_nan() || fhi > 0.0 || hi - lo < 1e-6 * (1.0 + hi) {
            break;
        }
        let t = (lo * fhi - hi * flo) / (fhi - flo);
        let t = if t > lo && t < hi { t } else { (lo + hi) * 0.5 };
        let ft = f(t);
        if ft > 0.0 {
            lo = t;
            flo = ft;
            if side == -1 {
                fhi *= 0.5;
            }
            side = -1;
        } else {
            hi = t;
            fhi = ft;
            if side == 1 {
                flo *= 0.5;
            }
            side = 1;
        }
    }
    hi
}

/// The time after which `(a + b·t)·e^{-rt} ≤ eps` for good: the last root, found by Newton's
/// method on the concave `ln(a + bt) − rt − ln(eps)` from a point past it, so every iterate stays
/// at or after the root (never early) and stopping at any iteration is safe.
fn linear_exp_root(a: f64, b: f64, r: f64, eps: f64) -> f64 {
    if b.is_nan() || b <= 0.0 {
        return exp_root(a, r, eps);
    }
    if r.is_nan() || r <= 0.0 || !a.is_finite() || !b.is_finite() {
        return f64::INFINITY;
    }
    // a + bt ≤ (a + 2b/r)·e^{rt/2}, so the bound is under eps from t0 on
    let big = a + 2.0 * b / r;
    if big.is_nan() || big <= eps {
        return 0.0;
    }
    let mut t = 2.0 * (big / eps).ln() / r;
    let target = eps.ln();
    for _ in 0..16 {
        let h = (a + b * t).ln() - r * t - target;
        let dh = b / (a + b * t) - r;
        if dh.is_nan() || dh >= 0.0 {
            // left of the peak and already under eps: under eps since 0
            return if a <= eps { 0.0 } else { t };
        }
        let next = t - h / dh;
        if !next.is_finite() || next >= t {
            break;
        }
        let step = t - next;
        t = next;
        if step < 1e-9 * (1.0 + t) {
            break;
        }
    }
    t.max(0.0)
}

/// Replaces non-finite inputs: a bad target by the start (or 0), a bad start by the target, a bad
/// velocity by 0.
pub(crate) fn sane_state(x0: f32, v0: f32, target: f32) -> (f32, f32, f32) {
    let target = if target.is_finite() {
        target
    } else if x0.is_finite() {
        x0
    } else {
        0.0
    };
    let x0 = if x0.is_finite() { x0 } else { target };
    let v0 = if v0.is_finite() { v0 } else { 0.0 };
    (x0, v0, target)
}

/// `x` as an f32 when it is finite there, else `fallback`.
pub(crate) fn finite_f32(x: f64, fallback: f32) -> f32 {
    let y = x as f32;
    if y.is_finite() { y } else { fallback }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_clamps() {
        assert_eq!(Spring::new(f32::NAN, f32::NAN), Spring::DEFAULT);
        assert!(Spring::new(-1.0, 0.0).is_none());
        assert!(Spring::new(f32::NEG_INFINITY, 0.0).is_none());
        assert_eq!(Spring::new(f32::INFINITY, 2.0).duration(), Spring::MAX_DURATION);
        assert_eq!(Spring::new(1e-9, -2.0).duration(), Spring::MIN_DURATION);
        assert_eq!(Spring::new(1.0, -2.0).bounce(), -Spring::MAX_BOUNCE);
        assert_eq!(Spring::from_response(0.5, 1.0), Spring::new(0.5, 0.0));
        assert!((Spring::from_response(0.5, 0.7).bounce() - 0.3).abs() < 1e-6);
        assert!((Spring::from_response(0.5, 2.0).damping_ratio() - 2.0).abs() < 1e-5);
    }

    #[test]
    fn critical_has_no_overshoot() {
        let s = Spring::new(0.4, 0.0);
        let mut t = 0.0;
        while t < 2.0 {
            let (x, _) = s.sample(0.0, 0.0, 1.0, t);
            assert!(x <= 1.0 + 1e-6, "{x} at {t}");
            t += 0.001;
        }
    }

    #[test]
    fn linear_exp_root_is_last_crossing() {
        for &(a, b, r) in &[(1.0, 10.0, 3.0), (0.0, 5.0, 1.0), (2.0, 0.0, 2.0), (1e-6, 1.0, 50.0)] {
            let eps = 1e-3;
            let t = linear_exp_root(a, b, r, eps);
            let f = |t: f64| (a + b * t) * (-r * t).exp();
            assert!(f(t) <= eps * (1.0 + 1e-9), "{a} {b} {r}");
            let mut u = t;
            while u < t + 20.0 {
                assert!(f(u) <= eps * (1.0 + 1e-9));
                u += 0.01;
            }
            assert!(f(t - 1e-3) > eps * 0.99 || t < 1e-3);
        }
    }
}
