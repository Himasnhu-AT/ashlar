//! Screenshot encoding: crop a captured frame to a rect in points and encode it as PNG; base64 for
//! the wire (pictures travel inline, never through a predictable temp file).

use egui::Rect;

/// The pixel box `[x0, y0, x1, y1)` of `crop` (points) in an image of `size` pixels.
pub fn pixel_box(size: [usize; 2], crop: Option<Rect>, pixels_per_point: f32) -> Option<[usize; 4]> {
    let [w, h] = size;
    let b = match crop {
        Some(r) if r.is_finite() => {
            let s = if pixels_per_point.is_finite() && pixels_per_point > 0.0 { pixels_per_point } else { 1.0 };
            let c = |v: f32, max: usize| ((v * s).round().max(0.0) as usize).min(max);
            [c(r.min.x, w), c(r.min.y, h), c(r.max.x, w), c(r.max.y, h)]
        }
        _ => [0, 0, w, h],
    };
    (b[2] > b[0] && b[3] > b[1]).then_some(b)
}

/// Crop `image` to `crop` (in points) and encode it as PNG: `(png, width, height)`.
pub fn encode_png(image: &egui::ColorImage, crop: Option<Rect>, pixels_per_point: f32) -> Result<(Vec<u8>, u32, u32), String> {
    let w = image.size[0];
    let [x0, y0, x1, y1] = pixel_box(image.size, crop, pixels_per_point).ok_or("the element is not on screen")?;
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut rgba = Vec::with_capacity(cw.saturating_mul(ch).saturating_mul(4));
    for y in y0..y1 {
        let start = y.saturating_mul(w);
        let row = image.pixels.get(start.saturating_add(x0)..start.saturating_add(x1)).ok_or("screenshot smaller than its size")?;
        for p in row {
            rgba.extend_from_slice(&p.to_srgba_unmultiplied());
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, cw as u32, ch as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
    }
    Ok((out, cw as u32, ch as u32))
}

/// Standard base64 with padding.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk.first().copied().unwrap_or(0), chunk.get(1).copied().unwrap_or(0), chunk.get(2).copied().unwrap_or(0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, k) in idx.iter().enumerate() {
            if i <= chunk.len() {
                s.push(char::from(T.get(*k as usize).copied().unwrap_or(b'A')));
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")]
        {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn png_crops_in_pixels() {
        let img = egui::ColorImage::new([20, 10], vec![egui::Color32::RED; 200]);
        let (png, w, h) = encode_png(&img, Some(Rect::from_min_size(pos2(1.0, 1.0), vec2(4.0, 3.0))), 2.0).unwrap();
        assert_eq!((w, h), (8, 6));
        assert_eq!(&png[1..4], b"PNG");
        assert!(encode_png(&img, Some(Rect::from_min_size(pos2(50.0, 50.0), vec2(4.0, 3.0))), 1.0).is_err());
        assert_eq!(encode_png(&img, None, f32::NAN).map(|(_, w, h)| (w, h)), Ok((20, 10)));
        // an image whose pixels are fewer than its size claims fails instead of panicking
        let short = egui::ColorImage { size: [20, 10], source_size: vec2(20.0, 10.0), pixels: vec![egui::Color32::RED; 5] };
        assert!(encode_png(&short, None, 1.0).is_err());
    }
}
