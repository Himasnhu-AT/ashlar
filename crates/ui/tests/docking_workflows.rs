#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Offscreen acceptance of the docking renderer: a document canvas and three populated panels,
//! driven by real pointer and keyboard input through merge, detach and redock, at two widths,
//! two scales and both themes. Nothing opens an OS window.
//!
//! `ASHLAR_UI_DOCK_VISUAL_DIR=/abs/dir` saves a PNG per stage and an `actions.txt` per
//! configuration.

use std::collections::HashMap;
use std::path::PathBuf;

use ashlar_ui::docking::{Action, DockArea, DockStyle, Floating, Layout, Node, Permissions, Placement};
use ashlar_ui::layout::{SplitAxis, SplitSize};
use egui::{Color32, Event, Id, Key, Modifiers, Pos2, Rect, Stroke, Theme, Ui, pos2, vec2};
use egui_kittest::Harness;

const CANVAS: u8 = 0;
const LAYERS: u8 = 1;
const PROPERTIES: u8 = 2;
const BRUSHES: u8 = 3;
const HEIGHT: f32 = 520.0;

fn name(p: u8) -> &'static str {
    match p {
        CANVAS => "Document",
        LAYERS => "Layers",
        PROPERTIES => "Properties",
        _ => "Brushes",
    }
}

fn permissions(p: &u8) -> Permissions {
    if *p == CANVAS { Permissions::PROTECTED } else { Permissions::default() }
}

fn initial() -> Layout<u8> {
    Layout {
        root: Some(Node::Split {
            axis: SplitAxis::Horizontal,
            size: SplitSize::FixedSecond(240.0),
            first: Box::new(Node::Tabs { panels: vec![CANVAS], active: 0 }),
            second: Box::new(Node::Tabs { panels: vec![LAYERS, PROPERTIES], active: 0 }),
        }),
        floating: vec![Floating { panels: vec![BRUSHES], active: 0, rect: [38.0, 76.0, 210.0, 250.0] }],
    }
}

/// App state: the layout, per-panel text values and what the renderer reported.
struct App {
    layout: Layout<u8>,
    values: HashMap<u8, String>,
    body_ids: HashMap<u8, Id>,
    actions: Vec<Action<u8>>,
}

fn area() -> Id {
    Id::new("workflow-dock")
}

fn field(p: u8) -> Id {
    Id::new(("field", p))
}

fn canvas_body(ui: &mut Ui) {
    let rect = ui.max_rect();
    let page = rect.shrink(16.0);
    let painter = ui.painter();
    painter.rect_filled(page, 2.0, Color32::from_gray(250));
    painter.rect_stroke(page, 2.0, Stroke::new(1.0, Color32::from_gray(160)), egui::StrokeKind::Inside);
    let left = page.left() + 18.0;
    painter.text(pos2(left, page.top() + 14.0), egui::Align2::LEFT_TOP, "Untitled page", egui::FontId::proportional(16.0), Color32::from_gray(30));
    for i in 0..5 {
        let y = page.top() + 48.0 + i as f32 * 18.0;
        painter.line_segment([pos2(left, y), pos2(page.right() - 18.0, y)], Stroke::new(1.0, Color32::from_gray(190)));
    }
    let art = Rect::from_min_size(pos2(left, page.top() + 150.0), vec2((page.width() - 36.0).max(10.0) * 0.6, 80.0));
    painter.rect_filled(art, 4.0, Color32::from_rgb(90, 140, 210));
}

fn panel_body(ui: &mut Ui, p: u8, values: &mut HashMap<u8, String>) {
    egui::ScrollArea::vertical().id_salt(("scroll", p)).show(ui, |ui| {
        ui.heading(name(p));
        let value = values.entry(p).or_default();
        ui.add(egui::TextEdit::singleline(value).id(field(p)).desired_width(150.0));
        ui.separator();
        for i in 0..12 {
            ui.label(format!("{} item {i}", name(p)));
        }
    });
}

impl App {
    fn show(&mut self, ui: &mut Ui) {
        let style = DockStyle::from_ui(ui);
        let (values, ids) = (&mut self.values, &mut self.body_ids);
        let out = DockArea::new(area()).show(
            ui,
            &self.layout,
            &style,
            |p| name(*p).to_owned(),
            permissions,
            |ui, p| {
                ids.insert(*p, ui.id());
                if *p == CANVAS { canvas_body(ui) } else { panel_body(ui, *p, values) }
            },
        );
        assert_eq!(out.error, None);
        for action in out.actions {
            let action = round_trip(action);
            self.layout.apply_with_permissions(action.clone(), permissions).unwrap_or_else(|e| panic!("{action:?}: {e}"));
            self.layout.validate().unwrap();
            self.actions.push(action);
        }
    }
}

#[cfg(feature = "serde")]
fn round_trip(action: Action<u8>) -> Action<u8> {
    let json = serde_json::to_string(&action).unwrap();
    serde_json::from_str(&json).unwrap()
}

#[cfg(not(feature = "serde"))]
fn round_trip(action: Action<u8>) -> Action<u8> {
    action
}

fn tab_rect(h: &Harness<'_, App>, p: u8) -> Rect {
    h.ctx.read_response(area().with(("tab", p))).map(|r| r.rect).unwrap_or_else(|| panic!("tab {} not rendered", name(p)))
}

fn settle(h: &mut Harness<'_, App>) {
    for _ in 0..3 {
        h.step();
    }
}

fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    for t in [0.1, 0.4, 0.7, 1.0] {
        h.hover_at(from + (to - from) * t);
        h.step();
    }
    h.drop_at(to);
    settle(h);
}

fn type_into(h: &mut Harness<'_, App>, p: u8, text: &str) {
    let rect = h.ctx.read_response(field(p)).map(|r| r.rect).unwrap();
    h.hover_at(rect.center());
    h.step();
    h.drag_at(rect.center());
    h.step();
    h.drop_at(rect.center());
    h.step();
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.step();
    h.event(Event::Text(text.to_owned()));
    settle(h);
}

struct Capture {
    dir: Option<PathBuf>,
}

impl Capture {
    fn stage(&self, h: &mut Harness<'_, App>, stage: &str) {
        let Some(dir) = &self.dir else { return };
        let image = h.render().unwrap_or_else(|e| panic!("render failed: {e}"));
        image.save(dir.join(format!("{stage}.png"))).unwrap();
    }

    fn actions(&self, h: &Harness<'_, App>) {
        let Some(dir) = &self.dir else { return };
        let text: String = h.state().actions.iter().map(|a| format!("{a:?}\n")).collect();
        std::fs::write(dir.join("actions.txt"), text).unwrap();
    }
}

fn workflow(theme: Theme, width: f32, scale: f32) {
    let config = format!("{}-{width}-{scale}x", if theme == Theme::Dark { "dark" } else { "light" });
    let dir = std::env::var_os("ASHLAR_UI_DOCK_VISUAL_DIR").map(|d| PathBuf::from(d).join(&config));
    if let Some(dir) = &dir {
        std::fs::create_dir_all(dir).unwrap();
    }
    let capture = Capture { dir };
    let app = App { layout: initial(), values: HashMap::new(), body_ids: HashMap::new(), actions: Vec::new() };
    let mut h = Harness::builder()
        .with_size(vec2(width, HEIGHT))
        .with_pixels_per_point(scale)
        .with_theme(theme)
        .with_step_dt(1.0 / 60.0)
        .wgpu()
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.ctx.all_styles_mut(|s| {
        s.animation_time = 0.0;
        s.visuals.text_cursor.blink = false;
    });
    h.input_mut().focused = true;
    settle(&mut h);

    // Edit the floating panel's value.
    type_into(&mut h, BRUSHES, "Round 12px");
    assert_eq!(h.state().values[&BRUSHES], "Round 12px", "{config}");
    type_into(&mut h, LAYERS, "Ink");
    let brushes_body = h.state().body_ids[&BRUSHES];
    capture.stage(&mut h, "01-floating-brushes");

    // Merge into the main tab group, before Properties.
    let target = tab_rect(&h, PROPERTIES);
    let from = tab_rect(&h, BRUSHES).center();
    drag(&mut h, from, pos2(target.left() + 4.0, target.center().y));
    let s = h.state();
    assert!(s.actions.iter().any(|a| matches!(a, Action::Move { panel: BRUSHES, .. })), "{config}: {:?}", s.actions);
    assert!(s.layout.floating.is_empty(), "{config}: {:?}", s.layout);
    settle(&mut h);
    assert_eq!(h.state().body_ids[&BRUSHES], brushes_body, "{config}");
    capture.stage(&mut h, "02-merged-main-tabs");

    // Detach it again over the document.
    let from = tab_rect(&h, BRUSHES).center();
    drag(&mut h, from, pos2((width - 240.0) / 2.0, 270.0));
    let s = h.state();
    assert!(matches!(s.layout.floating.as_slice(), [g] if g.panels == vec![BRUSHES]), "{config}: {:?}", s.layout);
    assert!(s.actions.iter().any(|a| matches!(a, Action::Float { panel: BRUSHES, .. })));
    capture.stage(&mut h, "03-detached-again");

    // Redock in front of Layers.
    let target = tab_rect(&h, LAYERS);
    let from = tab_rect(&h, BRUSHES).center();
    drag(&mut h, from, pos2(target.left() + 4.0, target.center().y));
    settle(&mut h);
    let s = h.state();
    assert!(s.layout.floating.is_empty(), "{config}: {:?}", s.layout);
    assert!(
        s.actions.iter().rev().find_map(|a| match a {
            Action::Move { panel: BRUSHES, placement: Placement::Tab { before }, .. } => Some(*before),
            _ => None,
        }) == Some(Some(LAYERS)),
        "{config}: {:?}",
        s.actions
    );
    assert_eq!(s.body_ids[&BRUSHES], brushes_body, "{config}");
    assert_eq!(s.values[&BRUSHES], "Round 12px");
    assert_eq!(s.values[&LAYERS], "Ink");
    assert_eq!(s.layout.panels().len(), 4);
    s.layout.validate().unwrap();
    capture.stage(&mut h, "04-redocked");
    capture.actions(&h);
}

#[test]
fn merge_detach_and_redock_by_pointer() {
    for theme in [Theme::Dark, Theme::Light] {
        for width in [640.0, 1000.0] {
            for scale in [1.0, 2.0] {
                workflow(theme, width, scale);
            }
        }
    }
}
