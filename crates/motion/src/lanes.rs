//! Values that animate: anything that maps to up to four f32 lanes.

/// The most lanes a value can have.
pub const MAX_LANES: usize = 4;

/// The lanes of a value; lanes past `Animatable::LANES` are unused (0).
pub type Lanes = [f32; MAX_LANES];

/// A value that animates lane by lane. Each lane moves independently along the same curve.
pub trait Animatable: Copy {
    /// How many lanes the value uses (at most [`MAX_LANES`]; more are ignored).
    const LANES: usize;
    /// The value as lanes.
    fn to_lanes(self) -> Lanes;
    /// The value from lanes.
    fn from_lanes(lanes: Lanes) -> Self;
}

impl Animatable for f32 {
    const LANES: usize = 1;
    fn to_lanes(self) -> Lanes {
        [self, 0.0, 0.0, 0.0]
    }
    fn from_lanes([x, ..]: Lanes) -> Self {
        x
    }
}

impl Animatable for [f32; 2] {
    const LANES: usize = 2;
    fn to_lanes(self) -> Lanes {
        let [x, y] = self;
        [x, y, 0.0, 0.0]
    }
    fn from_lanes([x, y, ..]: Lanes) -> Self {
        [x, y]
    }
}

impl Animatable for [f32; 3] {
    const LANES: usize = 3;
    fn to_lanes(self) -> Lanes {
        let [x, y, z] = self;
        [x, y, z, 0.0]
    }
    fn from_lanes([x, y, z, _]: Lanes) -> Self {
        [x, y, z]
    }
}

impl Animatable for [f32; 4] {
    const LANES: usize = 4;
    fn to_lanes(self) -> Lanes {
        self
    }
    fn from_lanes(lanes: Lanes) -> Self {
        lanes
    }
}

/// A rectangle by its four edges, in points. Animating by edges keeps an edge that does not
/// move still, where animating by position and size would wobble it.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Edges {
    /// Left edge (min x).
    pub left: f32,
    /// Top edge (min y).
    pub top: f32,
    /// Right edge (max x).
    pub right: f32,
    /// Bottom edge (max y).
    pub bottom: f32,
}

impl Edges {
    /// Edges from left, top, right, bottom.
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Edges {
        Edges { left, top, right, bottom }
    }
}

impl Animatable for Edges {
    const LANES: usize = 4;
    fn to_lanes(self) -> Lanes {
        [self.left, self.top, self.right, self.bottom]
    }
    fn from_lanes([left, top, right, bottom]: Lanes) -> Self {
        Edges { left, top, right, bottom }
    }
}

/// The lane count of `T`, at most [`MAX_LANES`].
pub(crate) const fn lane_count<T: Animatable>() -> usize {
    if T::LANES < MAX_LANES { T::LANES } else { MAX_LANES }
}
