//! The in-app inspector (F12 / `devtools.toggle`): pick an element (hover outline, click copies
//! its ref), the element tree, frame and wake counters, and panels the app provides (logs, undo
//! history). Drawn only while open; when closed the debugger's hooks return before doing anything.

use egui::{Color32, Event, FontId, Painter, PointerButton, Pos2, Rect, Stroke, StrokeKind};

use super::State;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Elements,
    Stats,
    Section(usize),
}

#[derive(Default)]
pub(super) struct Overlay {
    pub open: bool,
    picking: bool,
    /// Key of the selected element.
    selected: Option<u64>,
    tab: Tab,
    /// Frames run while open (the live repaint counter).
    pub frames: u64,
    pub sections: Vec<(&'static str, Vec<String>)>,
    window: Option<Rect>,
    pick: Option<Pos2>,
    copied: Option<String>,
}

pub(super) fn window_id() -> egui::Id {
    egui::Id::new("ashlar.devtools.window")
}

impl Overlay {
    pub fn set_open(&mut self, open: bool) {
        if open && !self.open {
            self.frames = 0;
        }
        if !open {
            self.picking = false;
            self.sections = Vec::new();
        }
        self.open = open;
    }

    /// While picking, a click outside the inspector picks instead of reaching the app.
    pub fn input(&mut self, raw: &mut egui::RawInput) {
        if !self.picking {
            return;
        }
        let window = self.window;
        let outside = |p: &Pos2| window.is_none_or(|w| !w.contains(*p));
        let mut pick = None;
        raw.events.retain(|e| match e {
            Event::PointerButton { pos, button: PointerButton::Primary, pressed, .. } if outside(pos) => {
                if *pressed {
                    pick = Some(*pos);
                }
                false
            }
            Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                self.picking = false;
                false
            }
            _ => true,
        });
        if pick.is_some() {
            self.pick = pick;
        }
    }
}

/// A small ref label at the top-left corner of `rect`, with an outline.
pub(super) fn tag(painter: &Painter, rect: Rect, label: &str, color: Color32) {
    painter.rect_stroke(rect, 1.0, Stroke::new(1.0, color), StrokeKind::Outside);
    let galley = painter.layout_no_wrap(label.to_string(), FontId::monospace(10.0), Color32::WHITE);
    // above the element when there is room, so the label does not cover what it labels
    let h = galley.size().y;
    let at = if rect.top() >= h { rect.left_top() - egui::vec2(0.0, h) } else { rect.left_top() };
    let bg = Rect::from_min_size(at, galley.size() + egui::vec2(4.0, 0.0));
    painter.rect_filled(bg, 2.0, color);
    painter.galley(at + egui::vec2(2.0, 0.0), galley, Color32::WHITE);
}

fn describe(n: &super::model::Node) -> String {
    let mut s = format!("{} ", n.role);
    if !n.name.is_empty() {
        let name: String = n.name.chars().take(40).collect();
        s.push_str(&format!("\"{name}\" "));
    }
    s.push_str(&n.ref_str());
    if let Some(id) = &n.id {
        s.push_str(&format!(" #{id}"));
    }
    s
}

pub(super) fn show(ui: &mut egui::Ui, st: &mut State) {
    let ctx = ui.ctx().clone();
    let accent = Color32::from_rgb(80, 160, 255);
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, egui::Id::new("ashlar.devtools.overlay")));

    if let Some(p) = st.overlay.pick.take()
        && let Some(n) = st.model.at(p).and_then(|i| st.model.get(i))
    {
        st.overlay.selected = Some(n.key);
        let r = format!("@{}", n.ref_str());
        ctx.copy_text(r.clone());
        st.overlay.copied = Some(r);
        st.overlay.picking = false;
        st.overlay.tab = Tab::Elements;
    }
    if st.overlay.picking
        && let Some(p) = ctx.input(|i| i.pointer.hover_pos())
        && !st.overlay.window.is_some_and(|w| w.contains(p))
        && let Some(n) = st.model.at(p).and_then(|i| st.model.get(i))
    {
        let r = n.rect;
        tag(&painter, r, &format!("{}  {:.0}×{:.0} at {:.0},{:.0}", describe(n), r.width(), r.height(), r.min.x, r.min.y), accent);
    }
    let selected = st.overlay.selected.and_then(|k| st.model.by_key(k));
    if let Some(n) = selected.and_then(|i| st.model.get(i)) {
        painter.rect_stroke(n.rect, 2.0, Stroke::new(2.0, Color32::from_rgb(255, 170, 0)), StrokeKind::Outside);
    }

    let mut open = true;
    let content = ctx.content_rect();
    let resp = egui::Window::new("Devtools")
        .id(window_id())
        .open(&mut open)
        .default_size([360.0, 460.0])
        .default_pos(egui::pos2((content.right() - 380.0).max(content.left()), content.top() + 40.0))
        .show(&ctx, |ui| body(ui, st, selected));
    st.overlay.window = resp.map(|r| r.response.rect);
    if !open {
        st.overlay.set_open(false);
    }
    st.overlay.sections.clear();
}

fn body(ui: &mut egui::Ui, st: &mut State, selected: Option<usize>) {
    let ov = &mut st.overlay;
    ui.horizontal(|ui| {
        if ui.selectable_label(ov.picking, "Pick").on_hover_text("Click an element in the app to select it and copy its ref").clicked() {
            ov.picking = !ov.picking;
        }
        ui.separator();
        if ui.selectable_label(ov.tab == Tab::Elements, "Elements").clicked() {
            ov.tab = Tab::Elements;
        }
        if ui.selectable_label(ov.tab == Tab::Stats, "Stats").clicked() {
            ov.tab = Tab::Stats;
        }
        for (i, (title, _)) in ov.sections.iter().enumerate() {
            if ui.selectable_label(ov.tab == Tab::Section(i), *title).clicked() {
                ov.tab = Tab::Section(i);
            }
        }
    });
    ui.label(format!("frames while open: {}", ov.frames));
    if let Some(c) = &ov.copied {
        ui.weak(format!("copied {c}"));
    }
    ui.separator();
    match ov.tab {
        Tab::Elements => elements(ui, st, selected),
        Tab::Stats => stats(ui, st),
        Tab::Section(i) => match ov.sections.get(i) {
            Some((_, lines)) => {
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    for l in lines {
                        ui.monospace(l);
                    }
                });
            }
            None => ov.tab = Tab::Elements,
        },
    }
}

fn elements(ui: &mut egui::Ui, st: &mut State, selected: Option<usize>) {
    let order = st.model.preorder(None);
    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 2.0;
    let mut pick = None;
    egui::ScrollArea::vertical().max_height(260.0).auto_shrink([false, true]).show_rows(ui, row_h, order.len(), |ui, range| {
        for i in order.get(range).unwrap_or(&[]) {
            let Some(n) = st.model.get(*i) else { continue };
            ui.horizontal(|ui| {
                ui.add_space(n.depth.min(40) as f32 * 10.0);
                let text = egui::RichText::new(describe(n)).monospace();
                let text = if n.visible { text } else { text.weak() };
                if ui.selectable_label(selected == Some(*i), text).clicked() {
                    pick = Some(n.key);
                }
            });
        }
    });
    if let Some(k) = pick {
        st.overlay.selected = Some(k);
    }
    ui.separator();
    let Some(n) = selected.and_then(|i| st.model.get(i)) else {
        ui.weak("Select an element, or Pick one in the app.");
        return;
    };
    egui::Grid::new("ashlar.devtools.details").num_columns(2).show(ui, |ui| {
        let mut row = |k: &str, v: String| {
            ui.weak(k);
            ui.monospace(v);
            ui.end_row();
        };
        row("ref", n.ref_str());
        row("id", n.id.clone().unwrap_or_default());
        row("role", n.role.to_string());
        row("name", n.name.clone());
        row("value", n.value.clone().unwrap_or_default());
        let r = n.rect;
        row("rect", format!("{:.1}, {:.1}  {:.1}×{:.1}", r.min.x, r.min.y, r.width(), r.height()));
        let mut flags = Vec::new();
        for (on, f) in [(n.enabled, "enabled"), (n.focused, "focused"), (n.visible, "visible"), (n.interactive, "interactive")] {
            if on {
                flags.push(f);
            }
        }
        if n.selected == Some(true) {
            flags.push("selected");
        }
        if let Some(c) = n.checked {
            flags.push(if c { "checked" } else { "unchecked" });
        }
        row("state", flags.join(" "));
    });
    if ui.button("Copy ref").clicked() {
        let r = format!("@{}", n.ref_str());
        ui.ctx().copy_text(r.clone());
        st.overlay.copied = Some(r);
    }
}

fn stats(ui: &mut egui::Ui, st: &State) {
    let w = crate::wake::Scheduler::of(ui.ctx()).stats();
    let dt = ui.ctx().input(|i| i.stable_dt);
    egui::Grid::new("ashlar.devtools.stats").num_columns(2).show(ui, |ui| {
        let mut row = |k: &str, v: String| {
            ui.weak(k);
            ui.monospace(v);
            ui.end_row();
        };
        row("frames", w.frames.to_string());
        row("frames while open", st.overlay.frames.to_string());
        row("last frame interval", format!("{:.1} ms", dt * 1000.0));
        row("elements", st.model.nodes.len().to_string());
        row("pending input frames", st.script.frames.len().to_string());
        row("pending waits", st.waits.len().to_string());
    });
    ui.separator();
    ui.label("Frames by wake reason (requests)");
    egui::Grid::new("ashlar.devtools.wakes").num_columns(3).show(ui, |ui| {
        let mut reasons: Vec<&String> = w.frames_by.keys().chain(w.requests_by.keys()).collect();
        reasons.sort();
        reasons.dedup();
        for r in reasons {
            ui.monospace(r);
            ui.monospace(w.frames_by.get(r).copied().unwrap_or(0).to_string());
            ui.weak(format!("({})", w.requests_by.get(r).copied().unwrap_or(0)));
            ui.end_row();
        }
    });
}
