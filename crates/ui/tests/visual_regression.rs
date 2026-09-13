#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Paired same-machine comparisons: each production component is rendered next to an
//! independent reference painter with the same offscreen wgpu renderer, and the two must give
//! identical RGBA pixels and identical tessellated meshes. No baseline image is read from disk.
//!
//! `ASHLAR_UI_VISUAL_DIR=/abs/dir` saves `<case>-before.png` (reference), `-after.png`
//! (production), `-difference.png` (changed pixels in magenta) and a `comparison.tsv` per suite.

#[path = "visual/fixtures.rs"]
mod fixtures;

use std::cell::RefCell;
use std::io::Write as _;
use std::path::PathBuf;
use std::rc::Rc;

use ashlar_ui::icons::{SvgIconSet, SvgStyle};
use egui::epaint::{ClippedPrimitive, Primitive};
use egui::load::{Bytes, ImagePoll, SizeHint};
use egui::{Event, FontDefinitions, Modifiers, PointerButton, Rect, TexturesDelta, Theme, Ui};
use egui_kittest::wgpu::WgpuTestRenderer;
use egui_kittest::{Harness, TestRenderer};
use fixtures::{CANVAS, Component, Mutation, Probe, State};
use image::RgbaImage;

const SCALES: [f32; 3] = [1.0, 1.5, 2.0];
const THEMES: [Theme; 2] = [Theme::Light, Theme::Dark];
const MAX_IMAGE: (u32, u32) = (256, 144);

/// One wgpu device shared by every harness of a test (each new context re-uploads its fonts).
#[derive(Clone)]
struct Shared(Rc<RefCell<WgpuTestRenderer>>);

impl TestRenderer for Shared {
    fn handle_delta(&mut self, delta: &mut TexturesDelta) {
        self.0.borrow_mut().handle_delta(delta);
    }

    fn render(&mut self, ctx: &egui::Context, output: &egui::FullOutput) -> Result<RgbaImage, String> {
        self.0.borrow_mut().render(ctx, output)
    }
}

fn renderer() -> Shared {
    Shared(Rc::new(RefCell::new(WgpuTestRenderer::new())))
}

/// A rendered frame: pixels, meshes and what the first control reported.
struct Capture {
    image: RgbaImage,
    meshes: Vec<ClippedPrimitive>,
    probe: Probe,
}

/// Render `paint` on the canvas. Interactive captures run four frames with the state's inputs
/// injected before the second; static ones run two.
fn capture(
    shared: &Shared,
    theme: Theme,
    scale: f32,
    state: Option<(State, egui::Pos2)>,
    paint: impl FnMut(&mut Ui, &mut Probe) + 'static,
) -> Capture {
    let mut paint = paint;
    let mut h = Harness::builder().with_size(CANVAS).with_pixels_per_point(scale).with_theme(theme).renderer(shared.clone()).build_ui_state(
        move |ui, probe: &mut Probe| {
            let canvas = Rect::from_min_size(egui::Pos2::ZERO, CANVAS);
            ui.painter().rect_filled(canvas, 0.0, fixtures::surface(theme));
            paint(ui, probe);
            // Both sides cover the same area, so the harness frame around them is identical.
            ui.advance_cursor_after_rect(canvas);
        },
        Probe::default(),
    );
    h.ctx.set_fonts(FontDefinitions::empty());
    h.ctx.all_styles_mut(|s| {
        s.animation_time = 0.0;
        s.visuals.text_cursor.blink = false;
        s.visuals.disabled_alpha = 0.4;
    });
    h.input_mut().max_texture_side = Some(2048);
    h.input_mut().focused = true;

    let interactive = state.filter(|(s, _)| s.interactive());
    h.step();
    if let Some((state, c)) = interactive {
        if let (true, Some(id)) = (state.focuses(), h.state().target) {
            h.ctx.memory_mut(|m| m.request_focus(id));
        }
        if state.hovers() {
            h.input_mut().events.push(Event::PointerMoved(c));
        }
        if state.presses() {
            h.input_mut().events.push(Event::PointerButton { pos: c, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        }
        for _ in 0..3 {
            h.step();
        }
    } else {
        h.step();
    }

    let meshes = h.ctx.tessellate(h.output().shapes.clone(), scale);
    let image = h.render().unwrap_or_else(|e| panic!("renderer failed: {e}"));
    assert!(image.width() > 0 && image.height() > 0, "empty render");
    assert!(image.width() <= MAX_IMAGE.0 && image.height() <= MAX_IMAGE.1, "render too large: {}x{}", image.width(), image.height());
    Capture { image, meshes, probe: h.state().clone() }
}

fn meshes_equal(a: &[ClippedPrimitive], b: &[ClippedPrimitive]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.clip_rect == y.clip_rect
                && match (&x.primitive, &y.primitive) {
                    (Primitive::Mesh(m), Primitive::Mesh(n)) => {
                        m.texture_id == n.texture_id
                            && m.indices == n.indices
                            && m.vertices.len() == n.vertices.len()
                            && m.vertices.iter().zip(&n.vertices).all(|(v, w)| v.pos == w.pos && v.uv == w.uv && v.color == w.color)
                    }
                    _ => false,
                }
        })
}

fn changed_pixels(a: &RgbaImage, b: &RgbaImage) -> usize {
    if a.dimensions() != b.dimensions() {
        return (a.width() * a.height()).max(b.width() * b.height()) as usize;
    }
    a.pixels().zip(b.pixels()).filter(|(x, y)| x != y).count()
}

/// Optional on-disk report of a suite.
struct Report {
    dir: Option<PathBuf>,
}

impl Report {
    fn new(suite: &str) -> Self {
        let dir = std::env::var_os("ASHLAR_UI_VISUAL_DIR").map(|d| PathBuf::from(d).join(suite));
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("comparison.tsv"), "case\twidth\theight\tchanged_pixels\tmesh_equal\texpected_change\n").unwrap();
        }
        Self { dir }
    }

    fn row(&self, case: &str, before: &RgbaImage, after: &RgbaImage, changed: usize, mesh_equal: Option<bool>, expected_change: bool) {
        let Some(dir) = &self.dir else { return };
        before.save(dir.join(format!("{case}-before.png"))).unwrap();
        after.save(dir.join(format!("{case}-after.png"))).unwrap();
        if before.dimensions() == after.dimensions() {
            let mut diff = after.clone();
            for (d, (x, y)) in diff.pixels_mut().zip(before.pixels().zip(after.pixels())) {
                if x != y {
                    *d = image::Rgba([255, 0, 128, 255]);
                }
            }
            diff.save(dir.join(format!("{case}-difference.png"))).unwrap();
        }
        let mesh = mesh_equal.map_or("n/a".to_owned(), |m| m.to_string());
        let mut f = std::fs::OpenOptions::new().append(true).open(dir.join("comparison.tsv")).unwrap();
        writeln!(f, "{case}\t{}\t{}\t{changed}\t{mesh}\t{expected_change}", after.width(), after.height()).unwrap();
    }
}

fn theme_name(t: Theme) -> &'static str {
    match t {
        Theme::Light => "light",
        Theme::Dark => "dark",
    }
}

fn size_name(compact: bool) -> &'static str {
    if compact { "compact" } else { "normal" }
}

/// Check that the production case really reached `state`.
fn assert_state(case: &str, component: Component, state: State, p: &Probe) {
    match state {
        State::Disabled => assert!(!p.enabled && !p.hovered && !p.focused, "{case}: {p:?}"),
        State::Normal | State::Selected => assert!(p.enabled && !p.hovered && !p.focused, "{case}: {p:?}"),
        State::Hover => assert!(p.enabled && p.hovered, "{case}: {p:?}"),
        State::Press | State::SelectedPress if component.is_split() => assert!(p.enabled && p.down && p.dragged, "{case}: {p:?}"),
        State::Press | State::SelectedPress => assert!(p.enabled && p.down, "{case}: {p:?}"),
        State::Focus => assert!(p.enabled && p.focused, "{case}: {p:?}"),
    }
}

#[test]
fn controls_match_their_references() {
    let shared = renderer();
    let report = Report::new("controls");
    let mut pairs = 0;
    for component in Component::ALL {
        for state in component.states() {
            for theme in THEMES {
                for scale in SCALES {
                    for compact in [false, true] {
                        let case = format!("{}-{}-{}-{scale}x-{}", component.name(), state.name(), theme_name(theme), size_name(compact));
                        let at = fixtures::control_center(component, compact);
                        let after = capture(&shared, theme, scale, Some((state, at)), move |ui, probe| {
                            fixtures::production(ui, component, state, theme, compact, probe)
                        });
                        let before = capture(&shared, theme, scale, Some((state, at)), move |ui, _| {
                            fixtures::reference(ui, component, state, theme, compact, Mutation::None)
                        });
                        assert_state(&case, component, state, &after.probe);
                        let changed = changed_pixels(&before.image, &after.image);
                        let mesh_equal = meshes_equal(&before.meshes, &after.meshes);
                        report.row(&case, &before.image, &after.image, changed, Some(mesh_equal), false);
                        assert_eq!(changed, 0, "{case}: {changed} pixels differ");
                        assert!(mesh_equal, "{case}: meshes differ");
                        pairs += 1;
                    }
                }
            }
        }
    }
    assert_eq!(pairs, 372);
}

#[test]
fn mutated_references_are_detected() {
    let shared = renderer();
    let report = Report::new("negative");
    let mut pairs = 0;
    for mutation in [Mutation::Fill, Mutation::Spacing, Mutation::StrokeWidth] {
        for theme in THEMES {
            for scale in SCALES {
                for compact in [false, true] {
                    let case = format!("button-{}-{}-{scale}x-{}", mutation.name(), theme_name(theme), size_name(compact));
                    let after = capture(&shared, theme, scale, None, move |ui, probe| {
                        fixtures::production(ui, Component::Button, State::Normal, theme, compact, probe)
                    });
                    let before = capture(&shared, theme, scale, None, move |ui, _| {
                        fixtures::reference(ui, Component::Button, State::Normal, theme, compact, mutation)
                    });
                    let changed = changed_pixels(&before.image, &after.image);
                    let mesh_equal = meshes_equal(&before.meshes, &after.meshes);
                    report.row(&case, &before.image, &after.image, changed, Some(mesh_equal), true);
                    assert!(changed > 0, "{case}: the mutation changed no pixel");
                    assert!(!mesh_equal, "{case}: the mutation left the meshes equal");
                    pairs += 1;
                }
            }
        }
    }
    assert_eq!(pairs, 36);
}

#[test]
fn toolbar_overflow_matches_literal_rects() {
    let shared = renderer();
    let report = Report::new("toolbar");
    let mut pairs = 0;
    for available in fixtures::TOOLBAR_AVAILABLE {
        for theme in THEMES {
            for scale in SCALES {
                let case = format!("toolbar-{available}-{}-{scale}x", theme_name(theme));
                let after = capture(&shared, theme, scale, None, move |ui, _| fixtures::toolbar_production(ui, theme, available));
                let before = capture(&shared, theme, scale, None, move |ui, _| fixtures::toolbar_reference(ui, theme, available));
                let changed = changed_pixels(&before.image, &after.image);
                let mesh_equal = meshes_equal(&before.meshes, &after.meshes);
                report.row(&case, &before.image, &after.image, changed, Some(mesh_equal), false);
                assert_eq!(changed, 0, "{case}");
                assert!(mesh_equal, "{case}");
                pairs += 1;
            }
        }
    }
    assert_eq!(pairs, 18);
}

#[test]
fn widget_palette_matches_literal_painting() {
    let shared = renderer();
    let report = Report::new("palette");
    let mut pairs = 0;
    for theme in THEMES {
        for scale in SCALES {
            for compact in [false, true] {
                let case = format!("palette-{}-{scale}x-{}", theme_name(theme), size_name(compact));
                let after = capture(&shared, theme, scale, None, move |ui, _| fixtures::palette_production(ui, theme, compact));
                let before = capture(&shared, theme, scale, None, move |ui, _| fixtures::palette_reference(ui, theme, compact));
                let changed = changed_pixels(&before.image, &after.image);
                let mesh_equal = meshes_equal(&before.meshes, &after.meshes);
                report.row(&case, &before.image, &after.image, changed, Some(mesh_equal), false);
                assert_eq!(changed, 0, "{case}");
                assert!(mesh_equal, "{case}");
                pairs += 1;
            }
        }
    }
    assert_eq!(pairs, 12);
}

fn rasterize(ctx: &egui::Context, uri: &str, bytes: Bytes, px: u32) -> RgbaImage {
    ctx.include_bytes(uri.to_owned(), bytes);
    let poll = ctx.try_load_image(uri, SizeHint::Size { width: px, height: px, maintain_aspect_ratio: true }).unwrap();
    let ImagePoll::Ready { image } = poll else { panic!("{uri} not ready") };
    let [w, h] = image.size;
    RgbaImage::from_raw(w as u32, h as u32, image.pixels.iter().flat_map(|c| c.to_array()).collect()).unwrap()
}

#[test]
fn svg_icons_match_literal_svgs() {
    static ICONS: &[(&str, &[u8])] = &[("box", fixtures::SVG_SOURCE)];
    let report = Report::new("svg");
    let ctx = egui::Context::default();
    egui_extras::install_image_loaders(&ctx);
    let mut pairs = 0;
    for theme in THEMES {
        let style = SvgStyle { current_color: fixtures::svg_colour(theme), stroke_width: Some("1.75") };
        let prefix = match theme {
            Theme::Light => "bytes://visual/light/",
            Theme::Dark => "bytes://visual/dark/",
        };
        let set = SvgIconSet::new(ICONS, style, prefix, "box");
        let egui::ImageSource::Bytes { uri, bytes } = set.source("box") else { panic!("not bytes") };
        for points in [12.0f32, 24.0] {
            for scale in SCALES {
                let px = (points * scale).round() as u32;
                let case = format!("svg-{}-{points}pt-{scale}x", theme_name(theme));
                let after = rasterize(&ctx, &uri, bytes.clone(), px);
                let literal = format!("bytes://visual/literal/{}/{px}.svg", theme_name(theme));
                let before = rasterize(&ctx, &literal, Bytes::Static(fixtures::svg_expected(theme)), px);
                assert_eq!(after.dimensions(), (px, px), "{case}");
                assert!(after.pixels().any(|p| p.0[3] > 0), "{case}: no visible pixels");
                let changed = changed_pixels(&before, &after);
                report.row(&case, &before, &after, changed, None, false);
                assert_eq!(changed, 0, "{case}");
                pairs += 1;
            }
        }
    }
    assert_eq!(pairs, 12);
}
