# ashlar-motion

Headless motion maths for ashlar apps: where a value is at time `t`, and when it stops.

- `Spring`: closed-form damped springs set by `(duration, bounce)` (SwiftUI-compatible), exact in
  all three damping regimes, with a `settle_time` that is never early. Constructors clamp; nothing
  panics on NaN or infinite input.
- `Curve`: a spring, a cubic-bezier tween (`Easing`), momentum decay, or an instant jump.
- `Animated<T>`: a value you own. Retargeting mid-flight starts from the current position *and
  velocity*, so motion never jumps; a settled value is exactly its target. Works for `f32`,
  `[f32; 2]`, `Edges` (rectangles by edge), and colours (premultiplied OKLab).
- `Class` and `MotionPrefs`: settle thresholds per kind of value and the reduced-motion policy
  (spatial values jump, opacity and colour fade briefly, gestures are untouched; effects never
  overshoot).
- `tokens`: named enter/exit spring pairs (`SNAP`, `SETTLE`, `REFLOW`…), exits a little shorter.
- `color`: linear `Rgba`, OKLab/OKLCH with gamut mapping, premultiplied mixing, sRGB bytes, WCAG
  contrast.
- `gesture`: a velocity tracker, fling projection and rubber-band resistance.

```rust,ignore
use ashlar_motion::{Animated, Class, MotionPrefs, tokens};

let mut x = Animated::new(0.0_f32, Class::Spatial);
x.animate_to(120.0, tokens::SNAP.enter.into(), now, &MotionPrefs::DEFAULT);
let shown = x.sample(now); // draw this; ask for another frame while !x.is_settled(now)
```

No dependencies, no allocation, no UI; builds for `wasm32-unknown-unknown`. The egui integration
lives in `ashlar-ui`.

Part of [ashlar](https://github.com/Himasnhu-AT/ashlar), a lightweight application framework for Rust powered by egui.
Licensed under the [BSD 3-Clause License](https://github.com/Himasnhu-AT/ashlar/blob/main/LICENSE).
