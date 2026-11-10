#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A drawing snapshot of markdown: every shape a fixed document paints (text runs with their
//! fonts, colours, backgrounds and underlines; code frames; rules; quote bars; focus rings; table
//! stripes), written as text and compared with `tests/snapshots/markdown.shapes.txt`. It is
//! independent of the GPU, so it runs everywhere; a change to how markdown draws shows up as a
//! readable diff.
//!
//! `ASHLAR_UPDATE_SNAPSHOTS=1` writes the baseline instead of comparing.

use std::fmt::Write as _;
use std::path::PathBuf;

use ashlar_ui::markdown::{Markdown, MarkdownStyle};
use egui::epaint::Shape;
use egui::{Color32, Key, Pos2, Rect, Stroke};
use egui_kittest::Harness;

const DOC: &str = "# Markdown v2\n\nBody with **bold**, *italic*, `code`, ~~gone~~ and a [link that wraps \
onto the next row](https://example.com/a) plus https://example.com/b.\n\n- [ ] open task\n- [x] done task\n  1. nested \
number\n\n> a quote with a needle\n\n---\n\n```rust\nfn main() { let needle = 1; }\n```\n\n| name | value |\n|---|---:|\n\
| alpha | 1 |\n| a much longer cell that has to wrap | 22 |\n\nStreaming **tail";

fn hex(c: Color32) -> String {
    let [r, g, b, a] = c.to_array();
    format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
}

fn p(v: Pos2) -> String {
    format!("{:.1},{:.1}", v.x, v.y)
}

fn r(v: Rect) -> String {
    format!("{}..{}", p(v.min), p(v.max))
}

fn stroke(s: Stroke) -> String {
    if s.is_empty() { "-".into() } else { format!("{:.1} {}", s.width, hex(s.color)) }
}

fn describe(s: &Shape, out: &mut String) {
    match s {
        Shape::Noop => {}
        Shape::Vec(v) => v.iter().for_each(|s| describe(s, out)),
        Shape::Text(t) => {
            let g = &t.galley;
            let text: String = g.text().chars().take(60).collect();
            let _ = writeln!(out, "text @{} {}x{} rows={} {:?}", p(t.pos), g.rect.width().round(), g.rect.height().round(), g.rows.len(), text);
            for sec in &g.job.sections {
                let f = &sec.format;
                let _ = write!(
                    out,
                    "  [{}..{}] {:.1} {:?} {}",
                    sec.byte_range.start.0,
                    sec.byte_range.end.0,
                    f.font_id.size,
                    f.font_id.family,
                    hex(f.color)
                );
                if f.background != Color32::TRANSPARENT {
                    let _ = write!(out, " bg={}", hex(f.background));
                }
                if !f.underline.is_empty() {
                    let _ = write!(out, " underline={}", stroke(f.underline));
                }
                if !f.strikethrough.is_empty() {
                    let _ = write!(out, " strike={}", stroke(f.strikethrough));
                }
                if f.italics {
                    let _ = write!(out, " italic");
                }
                out.push('\n');
            }
        }
        Shape::Rect(x) => {
            let _ = writeln!(out, "rect {} fill={} stroke={} r={}", r(x.rect), hex(x.fill), stroke(x.stroke), x.corner_radius.nw);
        }
        Shape::LineSegment { points, stroke: st } => {
            let _ = writeln!(out, "line {}..{} {}", p(points[0]), p(points[1]), stroke(*st));
        }
        Shape::Circle(c) => {
            let _ = writeln!(out, "circle {} r={:.1} fill={}", p(c.center), c.radius, hex(c.fill));
        }
        Shape::Path(x) => {
            let _ = writeln!(out, "path {} points={} fill={} stroke={:?}", r(x.visual_bounding_rect()), x.points.len(), hex(x.fill), x.stroke.width);
        }
        other => {
            let _ = writeln!(out, "shape {}", r(other.visual_bounding_rect()));
        }
    }
}

fn render() -> String {
    let mut h = Harness::builder().with_size((360.0, 900.0)).with_pixels_per_point(1.0).with_theme(egui::Theme::Light).build_ui(|ui| {
        let style = MarkdownStyle::from_visuals(ui.visuals(), 14.0);
        Markdown::new("md", DOC, &style).find("needle").streaming(true).on_task(|_, _| {}).show(ui);
    });
    h.run();
    // the first link has keyboard focus, so its ring (on both rows) is in the drawing
    h.key_press(Key::Tab);
    h.run();
    let mut out = String::new();
    for c in &h.output().shapes {
        describe(&c.shape, &mut out);
    }
    out
}

#[test]
fn markdown_draws_as_recorded() {
    let got = render();
    assert_eq!(got, render(), "drawing is deterministic");
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/markdown.shapes.txt");
    if std::env::var_os("ASHLAR_UPDATE_SNAPSHOTS").is_some_and(|v| v == "1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e} (record it with ASHLAR_UPDATE_SNAPSHOTS=1)", path.display()));
    if got != want {
        let new = path.with_extension("new.txt");
        let _ = std::fs::write(&new, &got);
        let first = got.lines().zip(want.lines()).position(|(a, b)| a != b).unwrap_or(got.lines().count().min(want.lines().count()));
        panic!(
            "markdown drawing changed at line {}: got {:?}, recorded {:?}\nwhole drawing written to {}",
            first + 1,
            got.lines().nth(first),
            want.lines().nth(first),
            new.display()
        );
    }
}

/// The snapshot is only worth having if it notices a change: the same document drawn without
/// the find query or streaming repair gives a different drawing.
#[test]
fn the_snapshot_notices_changes() {
    let got = render();
    assert!(got.contains("bg="), "find highlights are in the drawing");
    assert!(got.contains("underline="), "links are underlined");
    assert!(!got.contains("Streaming **"), "the streaming tail is repaired");
    let rings = got.lines().filter(|l| l.starts_with("rect") && l.contains("fill=#00000000")).count();
    assert!(rings >= 2, "a focus ring on each row of the focused link ({rings})");
}
