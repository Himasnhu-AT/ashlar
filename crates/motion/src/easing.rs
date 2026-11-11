//! Cubic-bezier easing, as in CSS `cubic-bezier(x1, y1, x2, y2)`.

/// A cubic-bezier easing from (0, 0) to (1, 1) through two control points. The x coordinates are
/// clamped to 0..=1 (so time only moves forward) and the y coordinates to −1..=2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Easing {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
}

impl Default for Easing {
    fn default() -> Self {
        Easing::EASE
    }
}

impl Easing {
    /// Constant speed.
    pub const LINEAR: Easing = Easing { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 };
    /// CSS `ease`.
    pub const EASE: Easing = Easing { x1: 0.25, y1: 0.1, x2: 0.25, y2: 1.0 };
    /// CSS `ease-in`.
    pub const EASE_IN: Easing = Easing { x1: 0.42, y1: 0.0, x2: 1.0, y2: 1.0 };
    /// CSS `ease-out`.
    pub const EASE_OUT: Easing = Easing { x1: 0.0, y1: 0.0, x2: 0.58, y2: 1.0 };
    /// CSS `ease-in-out`.
    pub const EASE_IN_OUT: Easing = Easing { x1: 0.42, y1: 0.0, x2: 0.58, y2: 1.0 };

    /// `cubic-bezier(x1, y1, x2, y2)`. NaN coordinates take the linear curve's; others are clamped.
    pub const fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32) -> Easing {
        Easing { x1: coord(x1, 0.0, 0.0, 1.0), y1: coord(y1, 0.0, -1.0, 2.0), x2: coord(x2, 1.0, 0.0, 1.0), y2: coord(y2, 1.0, -1.0, 2.0) }
    }

    /// The control points `[x1, y1, x2, y2]`.
    pub const fn points(self) -> [f32; 4] {
        [self.x1, self.y1, self.x2, self.y2]
    }

    fn is_linear(self) -> bool {
        self.x1 == self.y1 && self.x2 == self.y2
    }

    /// Progress at time fraction `u` (clamped to 0..=1; NaN is 0).
    pub fn ease(self, u: f32) -> f32 {
        let u = unit(u);
        if self.is_linear() {
            return u;
        }
        let s = self.solve(f64::from(u));
        bez(f64::from(self.y1), f64::from(self.y2), s) as f32
    }

    /// The slope d(progress)/d(time fraction) at `u` (clamped to 0..=1), bounded to ±10⁴.
    pub fn slope(self, u: f32) -> f32 {
        let u = unit(u);
        if self.is_linear() {
            return 1.0;
        }
        let s = self.solve(f64::from(u));
        let (x1, x2, y1, y2) = (f64::from(self.x1), f64::from(self.x2), f64::from(self.y1), f64::from(self.y2));
        let dx = bez_d(x1, x2, s);
        let slope = if dx.abs() >= 1e-9 {
            bez_d(y1, y2, s) / dx
        } else {
            // x stalls (a control point on an end point): the limit is the ratio of the second
            // derivatives (L'Hôpital)
            let ddx = bez_dd(x1, x2, s);
            if ddx.abs() < 1e-12 {
                return 0.0;
            }
            bez_dd(y1, y2, s) / ddx
        };
        if slope.is_finite() { slope.clamp(-1e4, 1e4) as f32 } else { 0.0 }
    }

    /// The curve parameter whose x is `u`: Newton, then bisection when it does not converge.
    fn solve(self, u: f64) -> f64 {
        let (x1, x2) = (f64::from(self.x1), f64::from(self.x2));
        let mut s = u;
        for _ in 0..8 {
            let err = bez(x1, x2, s) - u;
            if err.abs() < 1e-7 {
                return s.clamp(0.0, 1.0);
            }
            let d = bez_d(x1, x2, s);
            if d.abs() < 1e-6 {
                break;
            }
            s -= err / d;
            if !(0.0..=1.0).contains(&s) {
                break;
            }
        }
        // x(s) is monotone for x1, x2 in 0..=1
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        s = u;
        for _ in 0..40 {
            let x = bez(x1, x2, s);
            if (x - u).abs() < 1e-7 {
                break;
            }
            if x < u {
                lo = s;
            } else {
                hi = s;
            }
            s = (lo + hi) * 0.5;
        }
        s.clamp(0.0, 1.0)
    }
}

const fn coord(v: f32, nan: f32, lo: f32, hi: f32) -> f32 {
    if v.is_nan() { nan } else { v.clamp(lo, hi) }
}

fn unit(u: f32) -> f32 {
    if u.is_nan() { 0.0 } else { u.clamp(0.0, 1.0) }
}

/// One coordinate of the bezier from 0 to 1 with control values `p1`, `p2`, at parameter `s`.
fn bez(p1: f64, p2: f64, s: f64) -> f64 {
    let c = 3.0 * p1;
    let b = 3.0 * (p2 - p1) - c;
    let a = 1.0 - c - b;
    ((a * s + b) * s + c) * s
}

fn bez_d(p1: f64, p2: f64, s: f64) -> f64 {
    let c = 3.0 * p1;
    let b = 3.0 * (p2 - p1) - c;
    let a = 1.0 - c - b;
    (3.0 * a * s + 2.0 * b) * s + c
}

fn bez_dd(p1: f64, p2: f64, s: f64) -> f64 {
    let c = 3.0 * p1;
    let b = 3.0 * (p2 - p1) - c;
    let a = 1.0 - c - b;
    6.0 * a * s + 2.0 * b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_is_identity() {
        for e in [Easing::LINEAR, Easing::cubic_bezier(0.0, 0.0, 1.0, 1.0), Easing::cubic_bezier(0.3, 0.3, 0.7, 0.7)] {
            for i in 0..=100 {
                let u = i as f32 / 100.0;
                assert!((e.ease(u) - u).abs() < 1e-5, "{e:?} {u}");
                assert!((e.slope(u) - 1.0).abs() < 1e-3, "{e:?} {u}");
            }
        }
    }

    #[test]
    fn ends_and_monotone() {
        for e in [Easing::LINEAR, Easing::EASE, Easing::EASE_IN, Easing::EASE_OUT, Easing::EASE_IN_OUT] {
            assert!(e.ease(0.0).abs() < 1e-6);
            assert!((e.ease(1.0) - 1.0).abs() < 1e-6);
            let mut last = 0.0;
            for i in 0..=1000 {
                let y = e.ease(i as f32 / 1000.0);
                assert!(y >= last - 1e-6);
                last = y;
            }
        }
        assert!((Easing::EASE_IN_OUT.ease(0.5) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn slope_matches_finite_difference() {
        for e in [Easing::EASE, Easing::EASE_IN, Easing::EASE_OUT, Easing::EASE_IN_OUT, Easing::cubic_bezier(0.3, -0.5, 0.6, 1.6)] {
            for i in 1..100 {
                let u = i as f32 / 100.0;
                let h = 1e-3;
                let fd = (e.ease(u + h) - e.ease(u - h)) / (2.0 * h);
                assert!((fd - e.slope(u)).abs() < 2e-2 * (1.0 + fd.abs()), "{e:?} {u} {fd} {}", e.slope(u));
            }
        }
        // ease-out starts with a finite slope y2/x2 limit… and ease-in with 0
        assert!(Easing::EASE_IN.slope(0.0).abs() < 1e-2);
        assert!(Easing::EASE_OUT.slope(0.0) > 1.0);
    }

    #[test]
    fn hostile_inputs() {
        let e = Easing::cubic_bezier(f32::NAN, f32::INFINITY, -3.0, f32::NEG_INFINITY);
        assert_eq!(e.points(), [0.0, 2.0, 0.0, -1.0]);
        for u in [f32::NAN, f32::INFINITY, -1.0, 0.5, 2.0] {
            assert!(e.ease(u).is_finite());
            assert!(e.slope(u).is_finite());
        }
    }
}
