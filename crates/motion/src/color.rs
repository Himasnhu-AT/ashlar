//! Colour on linear floats: OKLab and OKLCH, premultiplied mixing, sRGB bytes and WCAG contrast.
//!
//! [`Rgba`] is linear sRGB with straight (unmultiplied) alpha. OKLab is the perceptual space of
//! Björn Ottosson (2020): equal steps look about equally different, and a mix of two colours does
//! not pass through grey or shift hue the way an sRGB mix does.

use crate::lanes::{Animatable, Lanes};

/// A colour in linear sRGB with straight alpha, channels nominally 0..=1.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rgba {
    /// Red, linear.
    pub r: f32,
    /// Green, linear.
    pub g: f32,
    /// Blue, linear.
    pub b: f32,
    /// Alpha (coverage), not premultiplied.
    pub a: f32,
}

impl Rgba {
    /// Fully transparent black.
    pub const TRANSPARENT: Rgba = Rgba::new(0.0, 0.0, 0.0, 0.0);
    /// Opaque black.
    pub const BLACK: Rgba = Rgba::new(0.0, 0.0, 0.0, 1.0);
    /// Opaque white.
    pub const WHITE: Rgba = Rgba::new(1.0, 1.0, 1.0, 1.0);

    /// A colour from linear channels.
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Rgba {
        Rgba { r, g, b, a }
    }

    /// From sRGB-encoded bytes (as in `#rrggbbaa`); alpha is linear.
    pub fn from_srgb8(r: u8, g: u8, b: u8, a: u8) -> Rgba {
        let c = |v: u8| srgb_to_linear(f32::from(v) / 255.0);
        Rgba { r: c(r), g: c(g), b: c(b), a: f32::from(a) / 255.0 }
    }

    /// To sRGB-encoded bytes, rounded; channels are clamped to 0..=1 first.
    pub fn to_srgb8(self) -> [u8; 4] {
        let s = self.sanitized();
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [byte(linear_to_srgb(s.r)), byte(linear_to_srgb(s.g)), byte(linear_to_srgb(s.b)), byte(s.a)]
    }

    /// Non-finite channels replaced by 0 and alpha clamped to 0..=1.
    pub fn sanitized(self) -> Rgba {
        let f = |v: f32| if v.is_finite() { v } else { 0.0 };
        Rgba { r: f(self.r), g: f(self.g), b: f(self.b), a: f(self.a).clamp(0.0, 1.0) }
    }

    /// Every channel clamped to 0..=1 (NaN to 0).
    pub fn clamped(self) -> Rgba {
        let s = self.sanitized();
        Rgba { r: s.r.clamp(0.0, 1.0), g: s.g.clamp(0.0, 1.0), b: s.b.clamp(0.0, 1.0), a: s.a }
    }

    /// Whether r, g and b are within 0..=1 (allowing `tolerance` either side).
    pub fn in_gamut(self, tolerance: f32) -> bool {
        let ok = |v: f32| v >= -tolerance && v <= 1.0 + tolerance;
        ok(self.r) && ok(self.g) && ok(self.b)
    }

    /// The same colour with alpha `a` (clamped to 0..=1).
    pub fn with_alpha(self, a: f32) -> Rgba {
        Rgba { a: if a.is_nan() { 0.0 } else { a.clamp(0.0, 1.0) }, ..self }
    }

    /// WCAG 2 relative luminance (alpha ignored), 0..=1.
    pub fn luminance(self) -> f32 {
        let c = self.clamped();
        0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b
    }
}

/// sRGB transfer function: an encoded channel 0..=1 to linear.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c.is_nan() {
        0.0
    } else if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse sRGB transfer function: a linear channel to encoded.
pub fn linear_to_srgb(c: f32) -> f32 {
    if c.is_nan() {
        0.0
    } else if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// A colour in OKLab with straight alpha: `l` lightness 0..=1, `a` green–red, `b` blue–yellow.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Oklab {
    /// Lightness, 0..=1.
    pub l: f32,
    /// Green (negative) to red (positive).
    pub a: f32,
    /// Blue (negative) to yellow (positive).
    pub b: f32,
    /// Alpha, not premultiplied.
    pub alpha: f32,
}

impl Oklab {
    /// The OKLab form of `c`.
    pub fn from_rgba(c: Rgba) -> Oklab {
        let [l, a, b] = to_oklab(c);
        Oklab { l, a, b, alpha: c.sanitized().a }
    }

    /// Back to linear RGB (unclamped: may be out of gamut).
    pub fn to_rgba(self) -> Rgba {
        from_oklab([self.l, self.a, self.b], self.alpha)
    }
}

impl Animatable for Oklab {
    const LANES: usize = 4;
    fn to_lanes(self) -> Lanes {
        [self.l, self.a, self.b, self.alpha]
    }
    fn from_lanes([l, a, b, alpha]: Lanes) -> Self {
        Oklab { l, a, b, alpha }
    }
}

/// An `Rgba` animates in premultiplied OKLab, so a fade to transparent does not darken and a
/// colour change does not pass through grey. Samples are clamped into gamut.
impl Animatable for Rgba {
    const LANES: usize = 4;
    fn to_lanes(self) -> Lanes {
        let c = self.sanitized();
        let [l, a, b] = to_oklab(c);
        [l * c.a, a * c.a, b * c.a, c.a]
    }
    fn from_lanes([l, a, b, alpha]: Lanes) -> Self {
        let alpha = if alpha.is_nan() { 0.0 } else { alpha.clamp(0.0, 1.0) };
        if alpha <= 1e-6 {
            return Rgba::TRANSPARENT;
        }
        from_oklab([l / alpha, a / alpha, b / alpha], alpha).clamped()
    }
}

/// Linear sRGB to OKLab `[l, a, b]` (alpha ignored). Non-finite channels count as 0.
pub fn to_oklab(c: Rgba) -> [f32; 3] {
    let c = c.sanitized();
    let l = 0.412_221_46 * c.r + 0.536_332_55 * c.g + 0.051_445_995 * c.b;
    let m = 0.211_903_5 * c.r + 0.680_699_5 * c.g + 0.107_396_96 * c.b;
    let s = 0.088_302_46 * c.r + 0.281_718_85 * c.g + 0.629_978_7 * c.b;
    let (l, m, s) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// OKLab `[l, a, b]` and an alpha to linear sRGB. Unclamped: the result may be out of gamut
/// (see [`oklch`] for a mapped colour), but it is always finite.
pub fn from_oklab(lab: [f32; 3], alpha: f32) -> Rgba {
    let f = |v: f32| if v.is_finite() { v.clamp(-10.0, 10.0) } else { 0.0 };
    let [ll, a, b] = lab.map(f);
    let l = ll + 0.396_337_78 * a + 0.215_803_76 * b;
    let m = ll - 0.105_561_346 * a - 0.063_854_17 * b;
    let s = ll - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    Rgba {
        r: 4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        g: -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        b: -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
        a: if alpha.is_nan() { 0.0 } else { alpha.clamp(0.0, 1.0) },
    }
}

/// The largest chroma `oklch` accepts; no sRGB colour has more.
pub const MAX_CHROMA: f32 = 0.4;

/// An opaque colour from OKLCH: lightness 0..=1, chroma 0..=0.4, hue in degrees. Out-of-gamut
/// requests keep their lightness and hue and lose chroma until they fit. Never NaN.
pub fn oklch(l: f32, c: f32, h_deg: f32) -> Rgba {
    let l = if l.is_nan() { 0.0 } else { l.clamp(0.0, 1.0) };
    let c = if c.is_nan() { 0.0 } else { c.clamp(0.0, MAX_CHROMA) };
    let h = if h_deg.is_finite() { h_deg.rem_euclid(360.0) } else { 0.0 }.to_radians();
    let (sin, cos) = h.sin_cos();
    let at = |c: f32| from_oklab([l, c * cos, c * sin], 1.0);
    let full = at(c);
    if full.in_gamut(1e-5) {
        return full.clamped();
    }
    // chroma 0 is a grey, always in gamut: bisect for the most chroma that fits
    let (mut lo, mut hi) = (0.0_f32, c);
    for _ in 0..24 {
        let mid = (lo + hi) * 0.5;
        if at(mid).in_gamut(0.0) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    at(lo).clamped()
}

/// The OKLCH form `[l, c, h_deg]` of `c` (hue 0 for greys).
pub fn to_oklch(c: Rgba) -> [f32; 3] {
    let [l, a, b] = to_oklab(c);
    let chroma = (a * a + b * b).sqrt();
    let h = if chroma < 1e-6 { 0.0 } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
    [l, chroma, h]
}

/// `a` and `b` mixed `t` of the way (0 = `a`, 1 = `b`, clamped) in premultiplied OKLab; the
/// result is clamped into gamut.
pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
    let (la, lb) = (a.to_lanes(), b.to_lanes());
    let mut out = [0.0; 4];
    for (o, (x, y)) in out.iter_mut().zip(la.iter().zip(lb.iter())) {
        *o = x + (y - x) * t;
    }
    Rgba::from_lanes(out)
}

/// WCAG 2 contrast ratio of `fg` on `bg`, 1..=21. A translucent `fg` is composited over `bg`
/// first (in linear light); `bg` counts as opaque.
pub fn contrast_ratio(fg: Rgba, bg: Rgba) -> f32 {
    let (fg, bg) = (fg.clamped(), bg.clamped());
    let over = |f: f32, b: f32| f * fg.a + b * (1.0 - fg.a);
    let shown = Rgba::new(over(fg.r, bg.r), over(fg.g, bg.g), over(fg.b, bg.b), 1.0);
    let (l1, l2) = (shown.luminance(), bg.luminance());
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_values() {
        let w = to_oklab(Rgba::WHITE);
        assert!((w[0] - 1.0).abs() < 1e-3 && w[1].abs() < 1e-3 && w[2].abs() < 1e-3);
        assert!((contrast_ratio(Rgba::BLACK, Rgba::WHITE) - 21.0).abs() < 1e-3);
        assert!((contrast_ratio(Rgba::WHITE, Rgba::WHITE) - 1.0).abs() < 1e-6);
        assert_eq!(Rgba::from_srgb8(0x12, 0x34, 0x56, 0x78).to_srgb8(), [0x12, 0x34, 0x56, 0x78]);
        // #777 on white is the classic 4.48
        let grey = Rgba::from_srgb8(0x77, 0x77, 0x77, 255);
        assert!((contrast_ratio(grey, Rgba::WHITE) - 4.48).abs() < 0.01);
    }

    #[test]
    fn mix_ends_and_transparent() {
        let red = Rgba::from_srgb8(255, 0, 0, 255);
        let blue = Rgba::from_srgb8(0, 0, 255, 255);
        let close = |a: Rgba, b: Rgba| a.to_srgb8().iter().zip(b.to_srgb8().iter()).all(|(x, y)| x.abs_diff(*y) <= 1);
        assert!(close(mix(red, blue, 0.0), red));
        assert!(close(mix(red, blue, 1.0), blue));
        // fading to transparent keeps the colour, only alpha drops
        let half = mix(red, Rgba::TRANSPARENT, 0.5);
        assert!((half.a - 0.5).abs() < 1e-5);
        assert!(close(half.with_alpha(1.0), red));
        assert!(mix(red, blue, f32::NAN).r.is_finite());
    }

    #[test]
    fn oklch_maps_into_gamut() {
        for &(l, c, h) in &[(0.7, 0.4, 145.0), (0.0, 0.4, 0.0), (1.0, 0.4, 270.0), (f32::NAN, f32::INFINITY, f32::NAN), (0.5, -1.0, -720.0)] {
            let col = oklch(l, c, h);
            assert!(col.in_gamut(0.0) && col.r.is_finite(), "{l} {c} {h} {col:?}");
        }
        // hue kept when mapping
        let col = oklch(0.7, 0.4, 145.0);
        let [_, _, h] = to_oklch(col);
        assert!((h - 145.0).abs() < 1.0, "{h}");
    }
}
