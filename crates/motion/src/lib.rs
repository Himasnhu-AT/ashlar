//! Headless motion maths for ashlar: closed-form springs, curves, animated values that keep their
//! velocity when retargeted, a reduced-motion policy per kind of value, OKLab/OKLCH colour and
//! gesture helpers.
//!
//! No UI, no dependencies, no allocation; builds for `wasm32-unknown-unknown`. The UI layer
//! (`ashlar-ui`) owns the clock and the frame requests; this crate only answers "where is this
//! value at time t, and when does it stop?".
//!
//! ```
//! use ashlar_motion::{Animated, Class, Curve, MotionPrefs, Spring};
//!
//! let prefs = MotionPrefs::DEFAULT;
//! let mut x = Animated::new(0.0_f32, Class::Spatial);
//! x.animate_to(100.0, Spring::new(0.3, 0.1).into(), 0.0, &prefs);
//! let mid = x.sample(0.1);
//! assert!(mid > 0.0 && mid < 110.0);
//! // retarget mid-flight: starts from where it is, at the speed it has
//! x.animate_to(-50.0, Curve::DEFAULT, 0.1, &prefs);
//! assert_eq!(x.sample(0.1), mid);
//! // settled values are exactly their target, and stop asking for frames
//! assert_eq!(x.sample(x.settle_at()), -50.0);
//! assert!(x.is_settled(x.settle_at()));
//! ```

mod animated;
mod class;
pub mod color;
mod curve;
mod easing;
pub mod gesture;
mod lanes;
mod spring;
pub mod tokens;

pub use animated::{Animated, DEFAULT_PIXELS_PER_POINT};
pub use class::{Class, Eps, MotionPrefs};
pub use curve::Curve;
pub use easing::Easing;
pub use lanes::{Animatable, Edges, Lanes, MAX_LANES};
pub use spring::{MAX_SETTLE, Spring};
pub use tokens::Pair;
