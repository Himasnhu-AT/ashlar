#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Real pixels with no window: a `Driver` with the wgpu renderer. Each test skips (with a
//! message) on a machine with no wgpu adapter.

use ashlar_app::{App, Driver, Renderer};
use ashlar_ui::automation::{self, Role};
use egui::Color32;
use serde_json::{Value, json};

const ACCENT: Color32 = Color32::from_rgb(220, 40, 60);

/// A screen with one filled, accent-coloured button `t.go` on the default panel.
fn driver(ppp: f32) -> Option<Driver<i64>> {
    let renderer = Renderer::wgpu();
    if !renderer.is_gpu() {
        println!("skipped: no wgpu adapter ({})", renderer.reason().unwrap_or("unknown"));
        return None;
    }
    let app = App::<i64>::new("headless-render", "0.0.0").view(|ui, _cx| {
        ui.label("headless render");
        let r = ui.add(egui::Button::new(egui::RichText::new("Go").color(Color32::WHITE)).fill(ACCENT).min_size(egui::vec2(120.0, 40.0)));
        automation::register(ui, &r, "t.go", "Go", Role::Button);
    });
    let mut d = Driver::with_renderer(app.runtime(), renderer);
    d.set_screen(320.0, 200.0);
    d.pixels_per_point(ppp);
    d.settle(20);
    Some(d)
}

fn unbase64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, c)| n | (u32::from(val(*c)) << (18 - 6 * i)));
        out.extend_from_slice(&n.to_be_bytes()[1..chunk.len()]);
    }
    out
}

/// The RGBA pixels of a screenshot reply, with its size.
fn pixels(shot: &Value) -> (Vec<[u8; 4]>, u32, u32) {
    let png = unbase64(shot["png"].as_str().unwrap());
    let mut reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let px = buf[..info.buffer_size()].as_chunks::<4>().0.to_vec();
    (px, info.width, info.height)
}

fn close(a: [u8; 4], b: Color32, tol: u8) -> bool {
    a[..3].iter().zip([b.r(), b.g(), b.b()]).all(|(x, y)| x.abs_diff(y) <= tol)
}

#[test]
fn the_wgpu_renderer_draws_real_pixels() {
    let Some(mut d) = driver(1.0) else { return };
    let shot = d.call("ui.screenshot", json!({})).unwrap();
    assert_eq!(shot["renderer"], "wgpu", "{}", d.renderer().adapter().unwrap_or_default());
    let (px, w, h) = pixels(&shot);
    assert_eq!((w, h), (320, 200));
    assert!(px.iter().any(|p| *p != px[0]), "the frame is not one flat colour");

    // the button's own crop is mostly its fill; the panel around it is not
    let crop = d.call("ui.screenshot", json!({"id": "t.go"})).unwrap();
    let (bpx, bw, bh) = pixels(&crop);
    assert!(bw >= 120 && bh >= 40, "{bw}×{bh}");
    let accent = bpx.iter().filter(|p| close(**p, ACCENT, 3)).count();
    assert!(accent * 2 > bpx.len(), "{accent} of {} pixels are the accent", bpx.len());
    assert!(bpx.iter().any(|p| close(*p, Color32::WHITE, 40)), "the label's white text is drawn");
    let panel = d.ctx().global_style().visuals.panel_fill;
    assert!(px.iter().filter(|p| close(**p, panel, 2)).count() * 2 > px.len(), "the rest is the panel");
}

#[test]
fn renders_of_the_same_state_are_identical() {
    let Some(mut d) = driver(1.0) else { return };
    let a = d.call("ui.screenshot", json!({})).unwrap();
    let b = d.call("ui.screenshot", json!({})).unwrap();
    assert_eq!(a["png"], b["png"], "same state, same bytes");
}

#[test]
fn the_display_scale_is_respected() {
    let Some(mut d) = driver(2.0) else { return };
    let shot = d.call("ui.screenshot", json!({})).unwrap();
    assert_eq!(shot["renderer"], "wgpu");
    assert_eq!((shot["width"].as_u64(), shot["height"].as_u64()), (Some(640), Some(400)));
    let crop = d.call("ui.screenshot", json!({"id": "t.go", "annotate": true})).unwrap();
    let (bpx, bw, _) = pixels(&crop);
    assert!(bw >= 240, "the crop is in pixels: {bw}");
    assert!(bpx.iter().filter(|p| close(**p, ACCENT, 3)).count() * 2 > bpx.len());
}

#[test]
#[ignore]
fn perf_headless_screenshot() {
    let Some(mut d) = driver(2.0) else { return };
    d.set_screen(1280.0, 800.0);
    d.settle(20);
    d.call("ui.screenshot", json!({})).unwrap();
    let n = 20;
    let start = std::time::Instant::now();
    for _ in 0..n {
        assert_eq!(d.call("ui.screenshot", json!({})).unwrap()["renderer"], "wgpu");
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0 / f64::from(n);
    println!("bench: headless_screenshot_ms {ms:.2} ms");
}
