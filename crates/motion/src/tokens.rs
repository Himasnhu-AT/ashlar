//! Named motion tokens: enter/exit spring pairs. Exits are 10–25% shorter than enters, so things
//! leave a little faster than they arrive.

use crate::curve::Curve;
use crate::easing::Easing;
use crate::spring::Spring;

/// A spring for things arriving and one for things leaving.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pair {
    /// For a value moving in (appearing, opening, growing).
    pub enter: Spring,
    /// For a value moving out; shorter than `enter`.
    pub exit: Spring,
}

impl Pair {
    /// A pair.
    pub const fn new(enter: Spring, exit: Spring) -> Pair {
        Pair { enter, exit }
    }

    /// `enter` when `entering`, else `exit`.
    pub const fn pick(self, entering: bool) -> Spring {
        if entering { self.enter } else { self.exit }
    }
}

/// 0.10 s: press and hover feedback.
pub const INSTANT: Pair = Pair::new(Spring::new(0.10, 0.0), Spring::new(0.08, 0.0));
/// 0.25 s: small state changes (a toggle, a tab indicator).
pub const SNAP: Pair = Pair::new(Spring::new(0.25, 0.0), Spring::new(0.20, 0.0));
/// 0.30 s: the default.
pub const SETTLE: Pair = Pair::new(Spring::new(0.30, 0.0), Spring::new(0.24, 0.0));
/// 0.40 s: layout moving to make room.
pub const REFLOW: Pair = Pair::new(Spring::new(0.40, 0.0), Spring::new(0.32, 0.0));
/// 0.32 s: a surface sliding in (a sheet, a panel).
pub const GLIDE: Pair = Pair::new(Spring::new(0.32, 0.0), Spring::new(0.26, 0.0));
/// 0.25 s with a slight bounce: something popping into view (a dialog, a popover). Exits do not
/// bounce.
pub const POP: Pair = Pair::new(Spring::new(0.25, 0.15), Spring::new(0.20, 0.0));

/// The default pair, [`SETTLE`].
pub const DEFAULT: Pair = SETTLE;

/// The short fade that opacity and colour take under reduced motion.
pub const REDUCED_FADE: Curve = Curve::Tween { duration: 0.18, easing: Easing::EASE_IN_OUT };

/// Every pair by name, for tuning panels and the control port.
pub const ALL: &[(&str, Pair)] = &[("instant", INSTANT), ("snap", SNAP), ("settle", SETTLE), ("reflow", REFLOW), ("glide", GLIDE), ("pop", POP)];

/// The pair called `name` (as listed in [`ALL`]).
pub fn by_name(name: &str) -> Option<Pair> {
    ALL.iter().find(|(n, _)| *n == name).map(|&(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exits_are_shorter() {
        for (name, p) in ALL {
            let ratio = p.exit.duration() / p.enter.duration();
            assert!((0.75..=0.9 + 1e-6).contains(&ratio), "{name}: {ratio}");
            assert!(p.exit.bounce() <= p.enter.bounce(), "{name}");
        }
        assert_eq!(by_name("settle"), Some(DEFAULT));
        assert_eq!(by_name("nope"), None);
    }
}
