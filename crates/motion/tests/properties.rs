#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Property tests over many random inputs from a tiny deterministic PRNG.

use ashlar_motion::color::{self, Rgba};
use ashlar_motion::{Animated, Class, Curve, Easing, Edges, Eps, MAX_SETTLE, MotionPrefs, Spring, tokens};

/// xorshift64*: deterministic, no dependencies.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    /// Uniform in [lo, hi).
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        let u = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * u
    }
    fn f(&mut self, lo: f32, hi: f32) -> f32 {
        self.range(f64::from(lo), f64::from(hi)) as f32
    }
}

/// x'' = −k·(x − target) − c·x' integrated with RK4 from (x0, v0), checked against the closed
/// form at `checks` points.
fn rk4_check(s: Spring, x0: f32, v0: f32, target: f32) -> f64 {
    let (k, c) = (f64::from(s.stiffness()), f64::from(s.damping()));
    let target64 = f64::from(target);
    let acc = |x: f64, v: f64| -k * (x - target64) - c * v;
    let horizon = f64::from(s.duration()) * 3.0;
    let steps = 6000;
    let dt = horizon / f64::from(steps);
    let (mut x, mut v) = (f64::from(x0), f64::from(v0));
    let mut worst = 0.0_f64;
    for i in 1..=steps {
        let (k1x, k1v) = (v, acc(x, v));
        let (k2x, k2v) = (v + 0.5 * dt * k1v, acc(x + 0.5 * dt * k1x, v + 0.5 * dt * k1v));
        let (k3x, k3v) = (v + 0.5 * dt * k2v, acc(x + 0.5 * dt * k2x, v + 0.5 * dt * k2v));
        let (k4x, k4v) = (v + dt * k3v, acc(x + dt * k3x, v + dt * k3v));
        x += dt / 6.0 * (k1x + 2.0 * k2x + 2.0 * k3x + k4x);
        v += dt / 6.0 * (k1v + 2.0 * k2v + 2.0 * k3v + k4v);
        if i % 50 == 0 {
            let t = dt * f64::from(i);
            let (cx, cv) = s.sample(x0, v0, target, t as f32);
            let ex = (f64::from(cx) - x).abs();
            // velocities reach hundreds: compare relative to the motion's scale
            let scale = 1.0 + f64::from(v0.abs()) + (f64::from(x0) - target64).abs() * f64::from(s.stiffness()).sqrt();
            let ev = (f64::from(cv) - v).abs() / scale;
            assert!(ex < 1e-4, "x {s:?} x0={x0} v0={v0} T={target} t={t}: {cx} vs {x}");
            assert!(ev < 1e-4, "v {s:?} x0={x0} v0={v0} T={target} t={t}: {cv} vs {v}");
            worst = worst.max(ex);
        }
    }
    worst
}

#[test]
fn closed_form_matches_rk4_in_all_regimes() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut regimes = [0usize; 3];
    for i in 0..1000 {
        let d = rng.f(0.05, 2.0);
        // a third each: overdamped, critical, underdamped
        let b = match i % 3 {
            0 => rng.f(-0.95, -0.001),
            1 => 0.0,
            _ => rng.f(0.001, 0.95),
        };
        let s = Spring::new(d, b);
        let zeta = s.damping_ratio();
        regimes[if zeta > 1.0 {
            0
        } else if zeta == 1.0 {
            1
        } else {
            2
        }] += 1;
        rk4_check(s, rng.f(-10.0, 10.0), rng.f(-50.0, 50.0), rng.f(-10.0, 10.0));
    }
    assert!(regimes.iter().all(|&n| n > 300), "{regimes:?}");
    // the edges of the range and near-critical damping
    for b in [-0.95, -1e-6, 1e-6, 0.95] {
        rk4_check(Spring::new(0.3, b), 5.0, -40.0, -3.0);
        rk4_check(Spring::new(0.01, b), 1.0, 0.0, 0.0);
        rk4_check(Spring::new(5.0, b), -8.0, 20.0, 2.0);
    }
}

#[test]
fn retargets_keep_position_and_velocity() {
    let mut rng = Rng(42);
    let prefs = MotionPrefs::DEFAULT;
    let curves = |rng: &mut Rng| match rng.next() % 5 {
        4 => Curve::Decay { k: rng.f(1.0, 10.0) },
        0 => Curve::Spring(Spring::new(rng.f(0.05, 1.0), rng.f(-0.9, 0.9))),
        1 => Curve::tween(rng.f(0.05, 1.0), Easing::EASE_IN_OUT),
        2 => Curve::tween(rng.f(0.05, 1.0), Easing::EASE_OUT),
        _ => Curve::DEFAULT,
    };
    for _ in 0..200 {
        let mut a = Animated::new(Edges::new(0.0, 0.0, 100.0, 40.0), Class::Spatial);
        let mut now = 0.0_f64;
        for _ in 0..50 {
            now += rng.range(0.0, 0.3);
            let target = Edges::new(rng.f(-500.0, 500.0), rng.f(-500.0, 500.0), rng.f(-500.0, 500.0), rng.f(-500.0, 500.0));
            let (x_before, v_before) = (a.sample_lanes(now), a.velocity_lanes(now));
            a.animate_to(target, curves(&mut rng), now, &prefs);
            let (x_after, v_after) = (a.sample_lanes(now), a.velocity_lanes(now));
            for i in 0..4 {
                let dx = (x_before[i] - x_after[i]).abs();
                let dv = (v_before[i] - v_after[i]).abs();
                assert!(dx < 1e-4, "position jumped by {dx} at {now}");
                assert!(dv < 1e-3 * (1.0 + v_before[i].abs()), "velocity jumped by {dv} at {now}: {v_before:?} {v_after:?}");
            }
            // and a spring moves on smoothly just after (finite difference matches; an eased
            // tween may bend sharply at its start)
            let Curve::Spring(spring) = a.curve() else { continue };
            // a step well inside the spring's fastest timescale
            let h = 1e-4_f64.min(0.05 / f64::from(spring.damping() + spring.stiffness().sqrt()));
            // (skipped when it settles within the step: it then snaps by less than eps)
            if a.is_settled(now + h) {
                continue;
            }
            let (x1, v1) = (a.sample_lanes(now + h), a.velocity_lanes(now + h));
            for i in 0..4 {
                // the mean velocity over the step (trapezoid) against the finite difference
                let fd = (x1[i] - x_after[i]) / h as f32;
                let mean = (v_after[i] + v1[i]) * 0.5;
                assert!((fd - mean).abs() < 0.02 * (1.0 + mean.abs()) + 5.0, "{fd} vs {mean}: {a:?} at {now}");
            }
        }
    }
}

#[test]
fn settle_is_never_early() {
    let mut rng = Rng(7);
    for i in 0..400 {
        let s = Spring::new(rng.f(0.05, 1.5), if i % 4 == 0 { 0.0 } else { rng.f(-0.95, 0.95) });
        let (x0, v0, target) = (rng.f(-500.0, 500.0), rng.f(-3000.0, 3000.0), rng.f(-500.0, 500.0));
        let eps = Class::Spatial.eps(rng.f(1.0, 3.0));
        let settle = s.settle_time(x0, v0, target, eps);
        assert!(settle.is_finite() && settle <= MAX_SETTLE);
        // dense sampling: the last unsettled time is not after `settle`
        let end = settle.max(0.1) * 3.0 + 1.0;
        let step = s.duration() / 400.0;
        let mut t = 0.0_f32;
        let mut last_unsettled = 0.0_f32;
        while t < end {
            let (x, v) = s.sample(x0, v0, target, t);
            if (x - target).abs() > eps.pos || v.abs() > eps.vel {
                last_unsettled = t;
            }
            t += step;
        }
        assert!(settle >= last_unsettled, "{s:?} x0={x0} v0={v0} T={target}: settle {settle} < {last_unsettled}");
        // and not wildly late: within a duration of the truth
        assert!(settle <= last_unsettled + s.duration() * 1.5 + 0.05, "{s:?}: settle {settle} vs {last_unsettled}");
    }
}

#[test]
fn hostile_inputs_are_finite() {
    let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.0, 0.0, 1e-30, f32::MAX, f32::MIN, -0.0];
    for &d in &bad {
        for &b in &bad {
            let s = Spring::new(d, b);
            for &x in &bad {
                for &t in &bad {
                    let (px, pv) = s.sample(x, x, 1.0, t);
                    assert!(px.is_finite() && pv.is_finite(), "{d} {b} {x} {t}");
                    let st = s.settle_time(x, x, -x, Eps::new(t, t));
                    assert!(st.is_finite() && st >= 0.0);
                    for c in [Curve::tween(d, Easing::cubic_bezier(b, x, t, d)), Curve::Decay { k: d }, Curve::Spring(s)] {
                        let (cx, cv) = c.sample(x, b, t, d);
                        assert!(cx.is_finite() && cv.is_finite(), "{c:?}");
                        assert!(c.settle_time(x, b, t, Eps::new(d, d)).is_finite());
                    }
                }
            }
        }
    }
    let mut a = Animated::new(f32::NAN, Class::Other);
    assert_eq!(a.sample(0.0), 0.0);
    for &x in &bad {
        a.animate_to(x, Curve::Spring(Spring::new(x, x)), f64::from(x), &MotionPrefs::new(false, x));
        a.release(x, x, Curve::Decay { k: x }, f64::NAN, &MotionPrefs::DEFAULT);
        for now in [f64::NAN, f64::INFINITY, -1.0, 0.0, 1e9] {
            assert!(a.sample(now).is_finite() && a.velocity(now).is_finite());
            let _ = a.is_settled(now);
        }
    }
    for &x in &bad {
        let c = color::oklch(x, x, x);
        assert!(c.in_gamut(0.0) && c.a.is_finite());
        let m = color::mix(Rgba::new(x, x, x, x), Rgba::WHITE, x);
        assert!([m.r, m.g, m.b, m.a].iter().all(|v| v.is_finite()));
        assert!(color::contrast_ratio(Rgba::new(x, 0.0, x, x), Rgba::new(x, x, 1.0, 1.0)).is_finite());
        assert!(color::from_oklab([x, x, x], x).r.is_finite());
        assert!(ashlar_motion::gesture::project(x, x, x).is_finite());
        assert!(ashlar_motion::gesture::rubber_band(x, x).is_finite());
    }
}

#[test]
fn oklab_round_trips() {
    let mut rng = Rng(0xdead_beef);
    let mut worst = 0.0_f32;
    for _ in 0..4096 {
        let c = Rgba::new(rng.f(0.0, 1.0), rng.f(0.0, 1.0), rng.f(0.0, 1.0), 1.0);
        let back = color::from_oklab(color::to_oklab(c), 1.0);
        for (a, b) in [(c.r, back.r), (c.g, back.g), (c.b, back.b)] {
            // per channel, in both linear and encoded sRGB
            worst = worst.max((a - b).abs()).max((color::linear_to_srgb(a) - color::linear_to_srgb(b.clamp(0.0, 1.0))).abs());
        }
    }
    assert!(worst < 1.0 / 512.0, "{worst}");
}

#[test]
fn oklch_stays_in_gamut() {
    let mut rng = Rng(99);
    for _ in 0..4096 {
        let (l, c, h) = (rng.f(-0.1, 1.1), rng.f(0.0, 0.5), rng.f(-720.0, 720.0));
        let col = color::oklch(l, c, h);
        assert!(col.in_gamut(0.0), "{l} {c} {h}: {col:?}");
        // lightness kept within the mapping's tolerance
        let [l2, ..] = color::to_oklch(col);
        assert!((l2 - l.clamp(0.0, 1.0)).abs() < 0.02, "{l} {c} {h}: {l2}");
    }
}

#[test]
fn reduced_motion_rules() {
    let reduced = MotionPrefs::REDUCED;
    let mut x = Animated::new(0.0_f32, Class::Spatial);
    x.animate_to(100.0, tokens::POP.enter.into(), 1.0, &reduced);
    assert!(x.is_settled(1.0));
    assert_eq!(x.sample(1.0), 100.0);

    let mut s = Animated::new(1.0_f32, Class::Scale);
    s.animate_to(0.97, tokens::POP.enter.into(), 1.0, &reduced);
    assert_eq!(s.sample(1.0), 0.97);

    let mut o = Animated::new(0.0_f32, Class::Opacity);
    o.animate_to(1.0, tokens::REFLOW.enter.into(), 1.0, &reduced);
    assert!((o.settle_at() - 1.18).abs() < 1e-6);
    let mid = o.sample(1.09);
    assert!(mid > 0.1 && mid < 0.9, "{mid}");

    let mut c = Animated::new(Rgba::BLACK, Class::Color);
    c.animate_to(Rgba::WHITE, tokens::REFLOW.enter.into(), 0.0, &reduced);
    assert!(c.settle_at() <= 0.18 + 1e-6);
    assert_eq!(c.sample(0.2), Rgba::WHITE);

    let mut g = Animated::new(0.0_f32, Class::Gesture);
    g.release(800.0, 0.0, tokens::SETTLE.enter.into(), 0.0, &reduced);
    assert!(g.velocity(0.0) == 800.0 && !g.is_settled(0.1));
}

#[test]
fn effects_hold_between_start_and_target() {
    let mut rng = Rng(5);
    for _ in 0..200 {
        let mut o = Animated::new(rng.f(0.0, 1.0), Class::Opacity);
        let mut now = 0.0;
        for _ in 0..10 {
            let target = rng.f(0.0, 1.0);
            o.animate_to(target, Spring::new(rng.f(0.1, 0.6), rng.f(0.0, 0.95)).into(), now, &MotionPrefs::DEFAULT);
            let end = o.settle_at().max(now) + 0.1;
            let mut t = now;
            while t < end {
                let x = o.sample(t);
                assert!((-1e-6..=1.0 + 1e-6).contains(&x), "{x}");
                t += 0.005;
            }
            now += rng.range(0.0, 0.4);
        }
    }
}

#[test]
fn multi_lane_settle_is_never_early() {
    let mut rng = Rng(11);
    for _ in 0..200 {
        let s = Spring::new(rng.f(0.05, 1.0), rng.f(-0.95, 0.95));
        let lanes: Vec<(f32, f32, f32)> = (0..4).map(|_| (rng.f(-300.0, 300.0), rng.f(-2000.0, 2000.0), rng.f(-300.0, 300.0))).collect();
        let eps = Class::Spatial.eps(2.0);
        let settle = s.settle_time_lanes(lanes.iter().copied(), eps);
        for &(x0, v0, target) in &lanes {
            assert!(settle >= s.settle_time(x0, v0, target, eps) - 1e-6);
            let mut t = settle;
            while t < settle + 2.0 {
                let (x, v) = s.sample(x0, v0, target, t);
                assert!((x - target).abs() <= eps.pos && v.abs() <= eps.vel, "{s:?} at {t}");
                t += 0.001;
            }
        }
    }
}
