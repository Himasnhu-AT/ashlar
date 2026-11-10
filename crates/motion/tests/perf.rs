#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Perf tests (`cargo xtask bench`): each prints `bench: <metric> <value> ns`.

use std::hint::black_box;
use std::time::Instant;

use ashlar_motion::color::{self, Rgba};
use ashlar_motion::{Animated, Class, Curve, Edges, MotionPrefs, Spring};

/// Nanoseconds per call of `f` over `n` calls, best of 5 rounds.
fn ns_per(n: u32, mut f: impl FnMut(u32)) -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let start = Instant::now();
        for i in 0..n {
            f(i);
        }
        best = best.min(start.elapsed().as_nanos() as f64 / f64::from(n));
    }
    best
}

#[test]
#[ignore]
fn perf_spring_sample() {
    let springs = [Spring::new(0.3, 0.2), Spring::new(0.3, 0.0), Spring::new(0.3, -0.3)];
    let mut acc = 0.0_f32;
    let ns = ns_per(1_000_000, |i| {
        let s = springs[(i % 3) as usize];
        let (x, v) = s.sample(black_box(0.0), black_box(10.0), black_box(100.0), (i % 1000) as f32 * 1e-3);
        acc += x + v;
    });
    black_box(acc);
    println!("bench: spring_sample {ns:.1} ns");
}

#[test]
#[ignore]
fn perf_animated_retarget_rect() {
    let prefs = MotionPrefs::DEFAULT;
    let mut a = Animated::new(Edges::new(0.0, 0.0, 100.0, 100.0), Class::Spatial);
    let curve: Curve = Spring::new(0.3, 0.1).into();
    let ns = ns_per(200_000, |i| {
        let f = (i % 97) as f32;
        let target = Edges::new(f, f * 0.5, 100.0 + f, 80.0 + f);
        a.animate_to(black_box(target), curve, f64::from(i) * 0.004, &prefs);
    });
    black_box(a.sample(1.0));
    println!("bench: animated_retarget_rect {ns:.1} ns");
}

#[test]
#[ignore]
fn perf_oklab_mix() {
    let a = Rgba::from_srgb8(0x33, 0x66, 0xcc, 255);
    let b = Rgba::from_srgb8(0xee, 0x88, 0x11, 200);
    let mut acc = 0.0_f32;
    let ns = ns_per(500_000, |i| {
        let m = color::mix(black_box(a), black_box(b), (i % 100) as f32 * 0.01);
        acc += m.r;
    });
    black_box(acc);
    println!("bench: oklab_mix {ns:.1} ns");
}

#[test]
#[ignore]
fn perf_settle_time() {
    let springs = [Spring::new(0.3, 0.2), Spring::new(0.3, 0.0), Spring::new(0.3, -0.3)];
    let eps = Class::Spatial.eps(2.0);
    let mut acc = 0.0_f32;
    let ns = ns_per(500_000, |i| {
        let s = springs[(i % 3) as usize];
        acc += s.settle_time(black_box((i % 50) as f32), black_box(300.0), black_box(200.0), eps);
    });
    black_box(acc);
    println!("bench: settle_time {ns:.1} ns");
}
