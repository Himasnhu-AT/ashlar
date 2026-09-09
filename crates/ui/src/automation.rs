//! Automation ids: every interactive component registers a stable id, label and on-screen rect
//! each frame, so an agent can find and click controls by id and `ui.inspect` can report what is
//! on screen without per-app code.
//!
//! Ids are dotted paths the app chooses (`inspector.opacity`, `files.row.12`). Components derive
//! sub-ids from the id they are given (`.row.<n>`, `.button.<n>`). Only what is visible is
//! registered: a virtualized list reports the rows it painted, not all of them. An empty id
//! registers nothing (for a component the app registers under its own id).
//!
//! The registry lives in an egui plugin, added on first use. Each completed pass replaces the
//! previous snapshot; a pass egui discards is not published. Recording reuses its buffers, so a
//! steady frame registers its elements without allocating.

use std::fmt::Write as _;

use egui::{Pos2, Rect, Response, Ui};

/// What kind of control an element is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Button,
    Toggle,
    Field,
    Slider,
    Knob,
    Swatch,
    List,
    Row,
    Table,
    Cell,
    Header,
    Tree,
    TreeItem,
    Menu,
    MenuItem,
    Dialog,
    Toast,
    Search,
    Status,
    Palette,
    Tab,
    #[default]
    Other,
}

/// One registered element.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Element {
    pub id: String,
    pub label: String,
    pub role: Role,
    /// Visible part of the element in points: `[x, y, width, height]`.
    pub rect: [f32; 4],
    pub enabled: bool,
    /// Current value as text (fields, sliders, toggles), empty when not applicable.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// Selected or checked state, when the element has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected: Option<bool>,
    /// The egui widget this element was registered from (its `Id` value), when known: lets the
    /// debugger ([`crate::devtools`]) merge it with egui's own AccessKit node for the widget.
    #[serde(skip)]
    pub egui_id: Option<u64>,
}

impl Element {
    /// The rect as an egui `Rect`.
    pub fn egui_rect(&self) -> Rect {
        let [x, y, w, h] = self.rect;
        Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
    }
    /// Where a click on this element should land.
    pub fn center(&self) -> Pos2 {
        self.egui_rect().center()
    }
}

/// The snapshot `ui.inspect` returns: elements of the last completed pass, in paint order.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Inspect {
    pub pass: u64,
    pub elements: Vec<Element>,
}

/// Optional details for [`register_with`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Details<'a> {
    pub value: Option<&'a str>,
    pub selected: Option<bool>,
}

#[derive(Default)]
struct Registry {
    /// Elements of the pass being recorded; `len` of them are live, the rest are reusable slots.
    recording: Vec<Element>,
    len: usize,
    published: Vec<Element>,
    published_len: usize,
    published_pass: u64,
    disabled: bool,
}

impl Registry {
    /// The next reusable slot (its strings keep their capacity from earlier passes).
    fn slot(&mut self) -> Option<&mut Element> {
        if self.len == self.recording.len() {
            self.recording.push(Element::default());
        }
        let i = self.len;
        self.len += 1;
        self.recording.get_mut(i)
    }

    fn publish(&mut self, pass: u64) {
        std::mem::swap(&mut self.recording, &mut self.published);
        self.published_len = self.len;
        self.published_pass = pass;
        self.len = 0;
    }

    fn live(&self) -> &[Element] {
        self.published.get(..self.published_len).unwrap_or(&[])
    }
}

/// Hard cap on elements per pass (a runaway loop must not grow memory without bound).
pub const MAX_ELEMENTS: usize = 50_000;

#[derive(Default)]
struct Plugin {
    registry: Registry,
}

impl egui::Plugin for Plugin {
    fn debug_name(&self) -> &'static str {
        "ashlar_ui::automation"
    }
    fn on_begin_pass(&mut self, _ui: &mut Ui) {
        self.registry.len = 0;
    }
    fn on_end_pass(&mut self, ui: &mut Ui) {
        if !ui.ctx().will_discard() {
            self.registry.publish(ui.ctx().cumulative_pass_nr());
        }
    }
}

fn with_registry<R>(ctx: &egui::Context, f: impl FnOnce(&mut Registry) -> R) -> Option<R> {
    let mut f = Some(f);
    if let Some(r) = ctx.with_plugin::<Plugin, _>(|p| f.take().map(|f| f(&mut p.registry))) {
        return r;
    }
    ctx.add_plugin(Plugin::default());
    ctx.with_plugin::<Plugin, _>(|p| f.take().map(|f| f(&mut p.registry))).flatten()
}

/// Add the registry to a context before its first frame. Optional: the first registration adds
/// it too, but a registry added during a pass starts publishing from the next pass.
pub fn install(ctx: &egui::Context) {
    ctx.add_plugin(Plugin::default());
}

/// Turn recording on or off (on by default).
pub fn set_enabled(ctx: &egui::Context, enabled: bool) {
    with_registry(ctx, |r| r.disabled = !enabled);
}

fn record(ui: &Ui, rect: Rect, egui_id: Option<egui::Id>, id: std::fmt::Arguments<'_>, label: &str, role: Role, enabled: bool, details: Details<'_>) {
    let visible = rect.intersect(ui.clip_rect());
    if !visible.is_positive() || !visible.is_finite() {
        return;
    }
    with_registry(ui.ctx(), |r| {
        if r.disabled || r.len >= MAX_ELEMENTS {
            return;
        }
        let Some(e) = r.slot() else { return };
        e.id.clear();
        let _ = e.id.write_fmt(id);
        if e.id.is_empty() {
            r.len = r.len.saturating_sub(1);
            return;
        }
        e.label.clear();
        e.label.push_str(label);
        e.role = role;
        e.rect = [visible.min.x, visible.min.y, visible.width(), visible.height()];
        e.enabled = enabled;
        e.value.clear();
        if let Some(v) = details.value {
            e.value.push_str(v);
        }
        e.selected = details.selected;
        e.egui_id = egui_id.map(|i| i.value());
    });
}

/// Register a widget's response under `id`.
pub fn register(ui: &Ui, response: &Response, id: &str, label: &str, role: Role) {
    record(ui, response.rect, Some(response.id), format_args!("{id}"), label, role, response.enabled(), Details::default());
}

/// Register with a value or selection state.
pub fn register_with(ui: &Ui, response: &Response, id: &str, label: &str, role: Role, details: Details<'_>) {
    record(ui, response.rect, Some(response.id), format_args!("{id}"), label, role, response.enabled(), details);
}

/// Register a formatted id (`format_args!("{list}.row.{i}")`) without allocating a `String`.
pub fn register_fmt(ui: &Ui, rect: Rect, id: std::fmt::Arguments<'_>, label: &str, role: Role, enabled: bool, details: Details<'_>) {
    record(ui, rect, None, id, label, role, enabled, details);
}

/// Elements of the last completed pass (what `ui.inspect` reports).
pub fn inspect(ctx: &egui::Context) -> Inspect {
    with_registry(ctx, |r| Inspect { pass: r.published_pass, elements: r.live().to_vec() }).unwrap_or_default()
}

/// The element with exactly this id in the last completed pass.
pub fn find(ctx: &egui::Context, id: &str) -> Option<Element> {
    with_registry(ctx, |r| r.live().iter().find(|e| e.id == id).cloned()).flatten()
}

/// Elements whose id starts with `prefix`.
pub fn query(ctx: &egui::Context, prefix: &str) -> Vec<Element> {
    with_registry(ctx, |r| r.live().iter().filter(|e| e.id.starts_with(prefix)).cloned().collect()).unwrap_or_default()
}

/// Where to click to hit element `id`, if it was on screen.
pub fn click_point(ctx: &egui::Context, id: &str) -> Option<Pos2> {
    find(ctx, id).map(|e| e.center())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::run;

    fn input() -> egui::RawInput {
        egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 300.0))), ..Default::default() }
    }

    #[test]
    fn registers_visible_elements_and_publishes_complete_passes() {
        let ctx = egui::Context::default();
        install(&ctx);
        let mut rows = 3;
        let frame = |rows: usize| {
            run(&ctx, input(), |ui| {
                let r = ui.button("Save");
                register(ui, &r, "toolbar.save", "Save", Role::Button);
                // an empty id registers nothing
                register(ui, &r, "", "Save", Role::Button);
                for i in 0..rows {
                    let r = ui.label("row");
                    register_fmt(
                        ui,
                        r.rect,
                        format_args!("list.row.{i}"),
                        "row",
                        Role::Row,
                        true,
                        Details { selected: Some(i == 1), ..Default::default() },
                    );
                }
                // fully clipped: not registered
                register_fmt(
                    ui,
                    Rect::from_min_size(egui::pos2(1000.0, 1000.0), egui::vec2(10.0, 10.0)),
                    format_args!("hidden"),
                    "",
                    Role::Other,
                    true,
                    Details::default(),
                );
            });
        };
        frame(rows);
        let snap = inspect(&ctx);
        assert_eq!(snap.elements.len(), 4);
        assert_eq!(snap.elements[0].id, "toolbar.save");
        assert_eq!(snap.elements[0].role, Role::Button);
        assert_eq!(query(&ctx, "list.row.").len(), 3);
        assert_eq!(find(&ctx, "list.row.1").and_then(|e| e.selected), Some(true));
        assert!(find(&ctx, "hidden").is_none());
        let p = click_point(&ctx, "toolbar.save").unwrap();
        assert!(Rect::from_min_size(Pos2::ZERO, egui::vec2(400.0, 300.0)).contains(p));
        rows = 1;
        frame(rows);
        assert_eq!(inspect(&ctx).elements.len(), 2, "slots from the larger pass are not reported");
        let json = serde_json::to_value(inspect(&ctx)).unwrap();
        assert_eq!(json["elements"][0]["id"], "toolbar.save");
        assert_eq!(json["elements"][0]["role"], "button");
    }

    #[test]
    fn disabled_recording_and_empty_context_are_harmless() {
        let ctx = egui::Context::default();
        assert!(inspect(&ctx).elements.is_empty());
        // added lazily by the first call: publishes from the next pass on
        set_enabled(&ctx, false);
        run(&ctx, input(), |ui| {
            let r = ui.button("x");
            register(ui, &r, "x", "x", Role::Button);
        });
        assert!(find(&ctx, "x").is_none());
        set_enabled(&ctx, true);
        run(&ctx, input(), |ui| {
            let r = ui.button("x");
            register(ui, &r, "x", "x", Role::Button);
        });
        assert!(find(&ctx, "x").is_some());
    }
}
