//! Gesture helpers: velocity from pointer samples, where a fling comes to rest, and rubber-band
//! resistance past an edge.

use core::marker::PhantomData;

use crate::lanes::{Animatable, Lanes, MAX_LANES, lane_count};

/// Per-millisecond deceleration rates for [`project`] and `Curve::decay`.
pub mod deceleration {
    /// A normal scroll or fling.
    pub const NORMAL: f32 = 0.998;
    /// A short, quick stop (paging, snapping).
    pub const FAST: f32 = 0.99;
}

/// The per-second decay rate `k` of a per-millisecond deceleration `rate` (clamped to
/// 0.5..=0.99995; NaN is [`deceleration::NORMAL`]).
pub fn decay_k(rate_per_ms: f32) -> f32 {
    let r = if rate_per_ms.is_nan() { deceleration::NORMAL } else { rate_per_ms.clamp(0.5, 0.999_95) };
    -1000.0 * r.ln()
}

/// Where a value at `x` moving at `v` units per second comes to rest when it slows at a
/// per-millisecond `rate` (see [`deceleration`]): `x + v / k`. The same point `Curve::decay`
/// rests at. Non-finite inputs give `x` (or 0).
pub fn project(x: f32, v: f32, rate: f32) -> f32 {
    let x = if x.is_finite() { x } else { 0.0 };
    if !v.is_finite() {
        return x;
    }
    let p = f64::from(x) + f64::from(v) / f64::from(decay_k(rate));
    let p = p as f32;
    if p.is_finite() { p } else { x }
}

/// Rubber-band resistance: how far a value pulled `over` past an edge moves, for a view of size
/// `dim`. Odd in `over`, about `0.55·over` at first and never more than `dim`. A size that is not
/// positive and finite gives 0.
pub fn rubber_band(over: f32, dim: f32) -> f32 {
    if !dim.is_finite() || dim <= 0.0 || over.is_nan() {
        return 0.0;
    }
    const C: f32 = 0.55;
    let o = over.abs();
    let moved = if o.is_infinite() { dim } else { (1.0 - 1.0 / (o * C / dim + 1.0)) * dim };
    moved.min(dim).copysign(over)
}

/// How many samples a [`VelocityTracker`] keeps.
const SAMPLES: usize = 16;
/// Samples older than this before the newest are ignored, in seconds.
const HORIZON: f64 = 0.1;
/// A pause this long (between samples, or before `velocity_at`) means the pointer stopped.
pub const STOP: f64 = 0.04;

/// Estimates the velocity of a value from timed samples (a pointer during a drag): a
/// least-squares line through the samples of the last 100 ms. A pause of 40 ms or more means the
/// pointer stopped, so its velocity is 0.
#[derive(Clone, Copy, Debug)]
pub struct VelocityTracker<T: Animatable = f32> {
    times: [f64; SAMPLES],
    values: [Lanes; SAMPLES],
    /// Index of the newest sample.
    head: usize,
    len: usize,
    _value: PhantomData<fn() -> T>,
}

impl<T: Animatable> Default for VelocityTracker<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Animatable> VelocityTracker<T> {
    /// An empty tracker.
    pub fn new() -> Self {
        VelocityTracker { times: [0.0; SAMPLES], values: [[0.0; MAX_LANES]; SAMPLES], head: 0, len: 0, _value: PhantomData }
    }

    /// Forgets every sample.
    pub fn clear(&mut self) {
        self.len = 0;
        self.head = 0;
    }

    /// Records `value` at time `t` (seconds). A non-finite time or value is ignored; a time
    /// earlier than the newest sample starts over; the same time replaces the newest sample.
    pub fn add(&mut self, t: f64, value: T) {
        let lanes = value.to_lanes();
        if !t.is_finite() || lanes.iter().take(lane_count::<T>()).any(|v| !v.is_finite()) {
            return;
        }
        if let Some(last) = self.last_time() {
            if t < last {
                self.clear();
            } else if t == last {
                if let Some(v) = self.values.get_mut(self.head) {
                    *v = lanes;
                }
                return;
            }
        }
        let next = if self.len == 0 { 0 } else { (self.head + 1) % SAMPLES };
        if let (Some(tt), Some(v)) = (self.times.get_mut(next), self.values.get_mut(next)) {
            *tt = t;
            *v = lanes;
        }
        self.head = next;
        self.len = (self.len + 1).min(SAMPLES);
    }

    fn last_time(&self) -> Option<f64> {
        if self.len == 0 { None } else { self.times.get(self.head).copied() }
    }

    /// The velocity at the newest sample, in units per second (0 with fewer than two samples).
    pub fn velocity(&self) -> T {
        T::from_lanes(self.velocity_lanes())
    }

    /// The velocity at `now`: 0 once the newest sample is [`STOP`] old.
    pub fn velocity_at(&self, now: f64) -> T {
        match self.last_time() {
            Some(last) if now.is_finite() && now - last < STOP => self.velocity(),
            _ => T::from_lanes([0.0; MAX_LANES]),
        }
    }

    fn velocity_lanes(&self) -> Lanes {
        let mut out = [0.0; MAX_LANES];
        let Some(last) = self.last_time() else { return out };
        // walk back from the newest while inside the horizon and without a stop
        let mut n = 0usize;
        let (mut st, mut stt) = (0.0_f64, 0.0_f64);
        let mut sx = [0.0_f64; MAX_LANES];
        let mut stx = [0.0_f64; MAX_LANES];
        let mut prev = last;
        for k in 0..self.len {
            let i = (self.head + SAMPLES - k) % SAMPLES;
            let (Some(&t), Some(v)) = (self.times.get(i), self.values.get(i)) else { break };
            if last - t > HORIZON || prev - t >= STOP {
                break;
            }
            prev = t;
            let dt = t - last;
            n += 1;
            st += dt;
            stt += dt * dt;
            for ((a, b), &x) in sx.iter_mut().zip(stx.iter_mut()).zip(v.iter()) {
                *a += f64::from(x);
                *b += dt * f64::from(x);
            }
        }
        if n < 2 {
            return out;
        }
        let nf = n as f64;
        let denom = nf * stt - st * st;
        if denom.abs() < 1e-12 {
            return out;
        }
        for (o, (&a, &b)) in out.iter_mut().zip(sx.iter().zip(stx.iter())).take(lane_count::<T>()) {
            let slope = (nf * b - st * a) / denom;
            let s = slope as f32;
            *o = if s.is_finite() { s } else { 0.0 };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_velocity() {
        let mut vt = VelocityTracker::<f32>::new();
        for i in 0..40 {
            let t = f64::from(i) / 120.0;
            vt.add(t, 300.0 * t as f32);
        }
        let v = vt.velocity();
        assert!((v - 300.0).abs() < 300.0 * 0.02, "{v}");
        assert!((vt.velocity_at(39.0 / 120.0 + 0.01) - 300.0).abs() < 6.0);
        assert_eq!(vt.velocity_at(39.0 / 120.0 + 0.05), 0.0);
    }

    #[test]
    fn stop_before_release_is_zero() {
        let mut vt = VelocityTracker::<[f32; 2]>::new();
        for i in 0..10 {
            let t = f64::from(i) / 60.0;
            vt.add(t, [100.0 * t as f32, 0.0]);
        }
        vt.add(9.0 / 60.0 + 0.05, [15.0, 0.0]);
        assert_eq!(vt.velocity(), [0.0, 0.0]);
    }

    #[test]
    fn rubber_band_is_odd_and_bounded() {
        for &o in &[0.0, 1.0, 10.0, 100.0, 1e6, f32::INFINITY] {
            let r = rubber_band(o, 200.0);
            assert!((0.0..=200.0).contains(&r));
            assert_eq!(rubber_band(-o, 200.0), -r);
        }
        assert_eq!(rubber_band(10.0, 0.0), 0.0);
        assert_eq!(rubber_band(f32::NAN, 10.0), 0.0);
    }

    #[test]
    fn project_matches_decay() {
        let p = project(10.0, 1000.0, deceleration::NORMAL);
        assert!((p - (10.0 + 1000.0 / decay_k(deceleration::NORMAL))).abs() < 1e-3);
        assert!((p - 509.5).abs() < 1.0, "{p}");
        assert_eq!(project(1.0, f32::NAN, 0.5), 1.0);
        assert!(project(f32::MAX, f32::MAX, 0.99995).is_finite());
    }
}
