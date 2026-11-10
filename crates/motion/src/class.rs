//! What kind of value moves: its settle thresholds and its reduced-motion rule.

use crate::curve::Curve;

/// Settle thresholds: a motion is settled once its distance to the target stays within `pos`
/// and its speed within `vel` (units per second) for good.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Eps {
    /// Largest distance from the target, in the value's units.
    pub pos: f32,
    /// Largest speed, in the value's units per second.
    pub vel: f32,
}

const MIN_EPS: f32 = 1e-7;

impl Eps {
    /// Thresholds, clamped to be positive and finite (NaN takes 10⁻³).
    pub const fn new(pos: f32, vel: f32) -> Eps {
        Eps { pos: eps_value(pos), vel: eps_value(vel) }
    }

    /// The same thresholds in range.
    pub const fn sanitized(self) -> Eps {
        Eps::new(self.pos, self.vel)
    }
}

const fn eps_value(v: f32) -> f32 {
    if v.is_nan() { 1e-3 } else { v.clamp(MIN_EPS, f32::MAX) }
}

/// The kind of a moving value. It sets how close counts as settled and what reduced motion does.
/// Effect classes (opacity, colour) never overshoot: their samples stay between the start and
/// the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Class {
    /// Positions and sizes in points.
    Spatial,
    /// A scale factor around 1.
    Scale,
    /// An alpha 0..=1. An effect: never overshoots.
    Opacity,
    /// Colour lanes (OKLab). An effect: never overshoots.
    Color,
    /// A value under the user's hand (a drag or fling): reduced motion leaves it alone.
    Gesture,
    /// Anything else (a progress number…).
    #[default]
    Other,
}

/// The pixels-per-point range [`Class::eps`] accepts.
const PPP_RANGE: (f32, f32) = (0.25, 16.0);

impl Class {
    /// Settle thresholds for this class on a display with `pixels_per_point` (NaN is 1, clamped
    /// to 0.25..=16). Spatial and gesture values settle within a quarter of a physical pixel.
    pub fn eps(self, pixels_per_point: f32) -> Eps {
        let ppp = if pixels_per_point.is_nan() { 1.0 } else { pixels_per_point.clamp(PPP_RANGE.0, PPP_RANGE.1) };
        match self {
            Class::Spatial | Class::Gesture => Eps::new(0.25 / ppp, 4.0 / ppp),
            Class::Scale => Eps::new(5e-4, 5e-3),
            Class::Opacity | Class::Color => Eps::new(1.0 / 1024.0, 1.0 / 64.0),
            Class::Other => Eps::new(1e-3, 1e-2),
        }
    }

    /// Whether this class is an effect (opacity, colour) that must not overshoot.
    pub const fn is_effect(self) -> bool {
        matches!(self, Class::Opacity | Class::Color)
    }

    /// The curve to use instead of `curve` under reduced motion: spatial, scale and other values
    /// jump; opacity and colour take a short fade ([`crate::tokens::REDUCED_FADE`]); a gesture
    /// keeps its curve (the user's own hand is never slowed or removed).
    pub const fn reduce(self, curve: Curve) -> Curve {
        match self {
            Class::Spatial | Class::Scale | Class::Other => Curve::Instant,
            Class::Opacity | Class::Color => {
                if curve.is_instant() {
                    Curve::Instant
                } else {
                    crate::tokens::REDUCED_FADE
                }
            }
            Class::Gesture => curve,
        }
    }
}

/// App-wide motion policy, plain data. The UI layer reads it; `Animated` takes it per retarget.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionPrefs {
    /// Reduce motion (the OS accessibility setting or a user override): see [`Class::reduce`].
    pub reduced: bool,
    /// Clock speed for motion: 1 normal, 0 paused, 0.25 slow motion. The caller's clock applies
    /// it; read it through [`MotionPrefs::scale`], which keeps it within 0..=10.
    pub time_scale: f32,
}

impl Default for MotionPrefs {
    fn default() -> Self {
        MotionPrefs::DEFAULT
    }
}

impl MotionPrefs {
    /// Full motion at normal speed.
    pub const DEFAULT: MotionPrefs = MotionPrefs { reduced: false, time_scale: 1.0 };
    /// Reduced motion at normal speed.
    pub const REDUCED: MotionPrefs = MotionPrefs { reduced: true, time_scale: 1.0 };
    /// The largest time scale.
    pub const MAX_TIME_SCALE: f32 = 10.0;

    /// Preferences with `time_scale` clamped to 0..=10 (NaN is 1).
    pub const fn new(reduced: bool, time_scale: f32) -> MotionPrefs {
        MotionPrefs { reduced, time_scale: scale(time_scale) }
    }

    /// The time scale in range.
    pub const fn scale(self) -> f32 {
        scale(self.time_scale)
    }
}

const fn scale(s: f32) -> f32 {
    if s.is_nan() { 1.0 } else { s.clamp(0.0, MotionPrefs::MAX_TIME_SCALE) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Easing, Spring, tokens};

    #[test]
    fn reduced_rules() {
        let s = Curve::Spring(Spring::new(0.4, 0.2));
        assert_eq!(Class::Spatial.reduce(s), Curve::Instant);
        assert_eq!(Class::Scale.reduce(s), Curve::Instant);
        assert_eq!(Class::Opacity.reduce(s), tokens::REDUCED_FADE);
        assert_eq!(Class::Color.reduce(s), tokens::REDUCED_FADE);
        assert_eq!(Class::Opacity.reduce(Curve::Instant), Curve::Instant);
        assert_eq!(Class::Gesture.reduce(s), s);
        assert!(matches!(tokens::REDUCED_FADE, Curve::Tween { duration, easing } if (duration - 0.18).abs() < 1e-6 && easing == Easing::EASE_IN_OUT));
    }

    #[test]
    fn eps_and_prefs_clamp() {
        let e = Class::Spatial.eps(f32::NAN);
        assert_eq!(e, Class::Spatial.eps(1.0));
        assert!(Class::Spatial.eps(2.0).pos < e.pos);
        assert!(Class::Spatial.eps(f32::INFINITY).pos > 0.0);
        assert_eq!(Eps::new(f32::NAN, -1.0), Eps { pos: 1e-3, vel: MIN_EPS });
        assert_eq!(MotionPrefs::new(false, f32::NAN).time_scale, 1.0);
        assert_eq!(MotionPrefs::new(false, 99.0).scale(), 10.0);
        assert_eq!(MotionPrefs { reduced: false, time_scale: -1.0 }.scale(), 0.0);
    }
}
