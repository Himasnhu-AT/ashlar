//! Modal dialog shell: title, body, and a row of buttons with keyboard defaults.
//!
//! - Enter activates the default button (unless a multi-line editor in the body has the focus,
//!   or the body consumed the key).
//! - Escape activates the cancel button (or closes with [`DialogOutcome::Dismissed`] when there
//!   is none). Only the topmost dialog reacts, and not while one of its popups is open.
//! - A click on the backdrop dismisses only when [`Dialog::dismiss_on_backdrop`] is set.
//! - On open the keyboard focus moves into the dialog ([`InitialFocus`]): to the first field of
//!   the body, else to the default button. When the dialog stops being shown, by any path, the
//!   focus returns to the widget that had it before (or to [`Dialog::return_focus_to`]).
//! - Screen readers get a modal dialog node named by the title, with [`Dialog::description`] as
//!   its description.
//! - On open the backdrop fades in and the panel fades in while growing from 97 % (no growing
//!   under reduced motion). When the app stops showing it, its last frame fades out, a little
//!   faster, without taking input. Hit testing always uses the resting layout.
//!
//! The app keeps whether the dialog is open and acts on the outcome:
//!
//! ```
//! # use ashlar_ui::dialog::{Dialog, DialogButton, DialogOutcome};
//! # fn ui(ctx: &egui::Context, open: &mut bool) {
//! if *open {
//!     let buttons = [DialogButton::cancel("Cancel"), DialogButton::destructive("Delete").default()];
//!     match Dialog::new("confirm-delete", "Delete 3 files?", &buttons).show(ctx, |ui| {
//!         ui.label("This cannot be undone.");
//!     }) {
//!         Some(DialogOutcome::Button(1)) => { /* delete */ *open = false; }
//!         Some(_) => *open = false,
//!         None => {}
//!     }
//! }
//! # }
//! ```

use egui::{Color32, FocusDirection, Id, Key, Margin, Modifiers, Rect, Stroke, Ui};

use ashlar_motion::tokens as mt;

use crate::automation::{self, Details, Role};
use crate::controls::{Button, Size, Variant};
use crate::motion;
use crate::tokens::{self, Tokens};

/// What a button does in the dialog's keyboard model and how it looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ButtonKind {
    #[default]
    Normal,
    /// Escape activates it.
    Cancel,
    /// Painted as dangerous (deleting, discarding).
    Destructive,
}

/// One button of the row (left to right).
#[derive(Clone, Copy, Debug)]
pub struct DialogButton<'a> {
    pub label: &'a str,
    pub kind: ButtonKind,
    /// Enter activates it; painted as the primary button.
    pub default: bool,
    pub enabled: bool,
}

impl<'a> DialogButton<'a> {
    pub fn new(label: &'a str) -> Self {
        DialogButton { label, kind: ButtonKind::Normal, default: false, enabled: true }
    }
    pub fn cancel(label: &'a str) -> Self {
        DialogButton { kind: ButtonKind::Cancel, ..Self::new(label) }
    }
    pub fn destructive(label: &'a str) -> Self {
        DialogButton { kind: ButtonKind::Destructive, ..Self::new(label) }
    }
    pub fn default(mut self) -> Self {
        self.default = true;
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// How the dialog was closed this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogOutcome {
    /// A button was activated (by click, Enter or Escape): its index.
    Button(usize),
    /// Escape with no cancel button, or a backdrop click when allowed.
    Dismissed,
}

/// Where the keyboard focus goes when a dialog opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InitialFocus {
    /// The first focusable widget of the body (a field to fill in), else the default button.
    #[default]
    Auto,
    /// The default button (else the cancel button, else the first enabled one), even when the
    /// body has fields.
    DefaultButton,
    /// Leave the focus alone (the body requests it itself).
    Keep,
}

/// The dialog. See the [module docs](self).
pub struct Dialog<'a> {
    id: &'a str,
    title: &'a str,
    buttons: &'a [DialogButton<'a>],
    width: f32,
    backdrop_dismisses: bool,
    description: Option<&'a str>,
    initial_focus: InitialFocus,
    return_to: Option<Id>,
}

impl<'a> Dialog<'a> {
    pub fn new(id: &'a str, title: &'a str, buttons: &'a [DialogButton<'a>]) -> Self {
        Dialog { id, title, buttons, width: 380.0, backdrop_dismisses: false, description: None, initial_focus: InitialFocus::Auto, return_to: None }
    }
    /// A sentence under the title, also the dialog's accessible description.
    pub fn description(mut self, text: &'a str) -> Self {
        self.description = Some(text);
        self
    }
    pub fn initial_focus(mut self, focus: InitialFocus) -> Self {
        self.initial_focus = focus;
        self
    }
    /// The widget that gets the focus back when the dialog closes, when it is not the one that had
    /// the focus as the dialog opened (a dialog opened by a pointer click: pass the clicked
    /// button's `Response::id`).
    pub fn return_focus_to(mut self, id: Id) -> Self {
        self.return_to = Some(id);
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        if width.is_finite() {
            self.width = width.clamp(160.0, 2000.0);
        }
        self
    }
    pub fn dismiss_on_backdrop(mut self, yes: bool) -> Self {
        self.backdrop_dismisses = yes;
        self
    }

    pub fn show(self, ctx: &egui::Context, body: impl FnOnce(&mut Ui)) -> Option<DialogOutcome> {
        let t = tokens::current(ctx);
        let modal_id = Id::new(("ashlar_ui::dialog", self.id));
        let frame = egui::Frame::new()
            .fill(t.surface.raised)
            .stroke(Stroke::new(1.0, t.border))
            .corner_radius(t.radius.lg)
            .inner_margin(Margin::same(t.space.xl as i8))
            .shadow(ctx.global_style().visuals.window_shadow);
        let backdrop = if t.dark { Color32::from_black_alpha(140) } else { Color32::from_black_alpha(90) };
        // enter: nothing animates on the context's first pass (a dialog restored at launch)
        let fx = modal_id.with("ashlar_ui::dialog_fx");
        if !focus_return::is_open(ctx, modal_id) && ctx.cumulative_pass_nr() > 0 {
            motion::set(ctx, fx.with("panel"), 0.0_f32);
            motion::set(ctx, fx.with("backdrop"), 0.0_f32);
        }
        let panel_in = motion::presence(ctx, fx.with("panel"), true, mt::POP).unwrap_or(1.0);
        let backdrop_in = motion::presence(ctx, fx.with("backdrop"), true, mt::SETTLE).unwrap_or(1.0);
        let mut frame = frame.fill(t.surface.raised.gamma_multiply(panel_in)).stroke(Stroke::new(1.0, t.border.gamma_multiply(panel_in)));
        frame.shadow.color = frame.shadow.color.gamma_multiply(panel_in);
        let mut clicked = None;
        let mut panel_rect = Rect::NOTHING;
        let focus_pending = focus_return::track(ctx, modal_id, self.return_to);
        let area = egui::Modal::default_area(modal_id).fade_in(false);
        let modal = egui::Modal::new(modal_id).area(area).frame(frame).backdrop_color(backdrop.gamma_multiply(backdrop_in)).show(ctx, |ui| {
            ui.multiply_opacity(panel_in);
            ui.set_width(self.width.min(ctx.content_rect().width() - 32.0).max(120.0));
            ui.label(egui::RichText::new(self.title).size(t.type_scale.heading).strong().color(t.text.primary));
            if let Some(d) = self.description {
                ui.add_space(t.space.xs);
                ui.label(egui::RichText::new(d).color(t.text.secondary));
            }
            let (title, description) = (self.title, self.description);
            ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
                node.set_role(egui::accesskit::Role::Dialog);
                node.set_label(title);
                node.set_modal();
                if let Some(d) = description {
                    node.set_description(d);
                }
            });
            ui.add_space(t.space.md);
            // the first focusable widget of the body takes the focus (egui's "Tab with nothing
            // focused" rule, limited to this modal's layer)
            // the first pass of a modal lays it out invisibly (and disabled): place the focus on
            // the first pass it can take it
            let opening = focus_pending && ui.is_visible() && ui.is_enabled();
            if opening {
                focus_return::focus_placed(ctx, modal_id);
            }
            let field_first = opening && self.initial_focus == InitialFocus::Auto;
            if field_first {
                ui.memory_mut(|m| {
                    if let Some(f) = m.focused() {
                        m.surrender_focus(f);
                    }
                    m.move_focus(FocusDirection::Next);
                });
            }
            body(ui);
            let mut focus_button = None;
            if opening && self.initial_focus != InitialFocus::Keep {
                let field = field_first && ui.memory(|m| m.focused()).is_some();
                if field_first && !field {
                    ui.memory_mut(|m| m.move_focus(FocusDirection::None));
                }
                if !field {
                    focus_button = self.first_button();
                }
            }
            ui.add_space(t.space.lg);
            clicked = button_row(ui, &t, self.id, self.buttons, focus_button);
            automation::register_fmt(ui, ui.min_rect(), format_args!("{}", self.id), self.title, Role::Dialog, true, Details::default());
            panel_rect = ui.min_rect();
        });
        let layer = modal.response.layer_id;
        focus_return::snapshot(ctx, modal_id, layer, panel_rect.center());
        if panel_in < 1.0 && !motion::prefs(ctx).reduced {
            grow(ctx, layer, panel_rect.center(), panel_in);
        }
        if let Some(i) = clicked {
            return Some(DialogOutcome::Button(i));
        }
        if !modal.is_top_modal {
            return None;
        }
        if !modal.any_popup_open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            return Some(self.cancel_outcome());
        }
        if self.backdrop_dismisses && modal.backdrop_response.clicked() {
            return Some(DialogOutcome::Dismissed);
        }
        // Enter: a focused multi-line editor keeps it (a single-line one has already given up the
        // focus when Enter arrives).
        if !ctx.egui_wants_keyboard_input()
            && let Some(i) = self.buttons.iter().position(|b| b.default && b.enabled)
            && ctx.input_mut(|inp| inp.consume_key(Modifiers::NONE, Key::Enter))
        {
            return Some(DialogOutcome::Button(i));
        }
        None
    }

    /// The button that takes the focus when there is no field: the default, else the cancel
    /// button, else the first enabled one.
    fn first_button(&self) -> Option<usize> {
        let b = self.buttons;
        b.iter()
            .position(|b| b.default && b.enabled)
            .or_else(|| b.iter().position(|b| b.kind == ButtonKind::Cancel && b.enabled))
            .or_else(|| b.iter().position(|b| b.enabled))
    }

    fn cancel_outcome(&self) -> DialogOutcome {
        match self.buttons.iter().position(|b| b.kind == ButtonKind::Cancel && b.enabled) {
            Some(i) => DialogOutcome::Button(i),
            None => DialogOutcome::Dismissed,
        }
    }
}

fn button_row(ui: &mut Ui, t: &Tokens, id: &str, buttons: &[DialogButton<'_>], focus: Option<usize>) -> Option<usize> {
    let mut clicked = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = t.space.md;
        for (i, b) in buttons.iter().enumerate().rev() {
            let r = dialog_button(ui, t, id, i, b);
            if focus == Some(i) {
                r.request_focus();
            }
            if r.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// Scale the panel's shapes on `layer` (all but the backdrop, its first shape) for a presence
/// `p`: 97 % at 0, full size at 1. Paint only.
fn grow(ctx: &egui::Context, layer: egui::LayerId, center: egui::Pos2, p: f32) {
    let s = 0.97 + 0.03 * p.clamp(0.0, 1.0);
    let ts = egui::emath::TSTransform::new(center.to_vec2() * (1.0 - s), s);
    ctx.graphics_mut(|g| {
        let list = g.entry(layer);
        let end = list.next_idx();
        if end.0 > 1 {
            list.transform_range(egui::layers::ShapeIdx(1), end, ts);
        }
    });
}

/// Returning the focus to the opener: open dialogs are tracked by the pass they were last shown
/// in, and one that was not shown in a completed pass is closed (and fades out from a snapshot of
/// its last frame).
mod focus_return {
    use ashlar_motion::tokens as mt;
    use egui::epaint::ClippedShape;
    use egui::{Context, Id, LayerId, Painter, Pos2, Shape, Ui};

    use crate::motion;

    /// Shapes kept per dialog for its exit, at most (a bigger dialog just disappears).
    const MAX_SNAPSHOT: usize = 4096;
    /// Dialogs fading out at once, at most.
    const MAX_LEAVING: usize = 4;

    /// A dialog's last frame: the backdrop (the layer's first shape) and the panel.
    struct Snapshot {
        layer: LayerId,
        center: Pos2,
        shapes: Vec<ClippedShape>,
    }

    /// Dialogs open at once (nested confirmations), at most.
    const MAX_OPEN: usize = 32;

    struct Open {
        modal: Id,
        opener: Option<Id>,
        seen: u64,
        /// The initial focus is still to be placed.
        pending: bool,
        last_frame: Option<Snapshot>,
    }

    #[derive(Default)]
    struct Plugin {
        open: Vec<Open>,
        leaving: Vec<(Id, Snapshot)>,
    }

    impl egui::Plugin for Plugin {
        fn debug_name(&self) -> &'static str {
            "ashlar_ui::dialog::focus_return"
        }
        fn on_end_pass(&mut self, ui: &mut Ui) {
            let ctx = ui.ctx().clone();
            if ctx.will_discard() || (self.open.is_empty() && self.leaving.is_empty()) {
                return;
            }
            let pass = ctx.cumulative_pass_nr();
            let mut restore = None;
            let mut closed = Vec::new();
            self.open.retain_mut(|o| {
                let shown = o.seen == pass;
                if !shown {
                    if restore.is_none() {
                        restore = o.opener;
                    }
                    if let Some(snap) = o.last_frame.take() {
                        closed.push((o.modal, snap));
                    }
                }
                shown
            });
            for c in closed {
                if self.leaving.len() >= MAX_LEAVING {
                    self.leaving.remove(0);
                }
                self.leaving.push(c);
            }
            if let Some(id) = restore {
                ctx.memory_mut(|m| m.request_focus(id));
            }
            self.leaving.retain(|(modal, snap)| paint_exit(&ctx, *modal, snap));
        }
    }

    /// Note that `modal` is shown this pass (remembering the focused widget when it opens).
    /// True while its initial focus is still to be placed.
    pub(super) fn track(ctx: &Context, modal: Id, return_to: Option<Id>) -> bool {
        let pass = ctx.cumulative_pass_nr();
        let focused = ctx.memory(|m| m.focused());
        let update = |p: &mut Plugin| match p.open.iter_mut().find(|o| o.modal == modal) {
            Some(o) => {
                o.seen = pass;
                if return_to.is_some() {
                    o.opener = return_to;
                }
                o.pending
            }
            None => {
                if p.open.len() >= MAX_OPEN {
                    p.open.remove(0);
                }
                // reopened while still fading out: the new one takes over
                p.leaving.retain(|(m, _)| *m != modal);
                p.open.push(Open { modal, opener: return_to.or(focused), seen: pass, pending: true, last_frame: None });
                true
            }
        };
        if let Some(pending) = ctx.with_plugin(|p: &mut Plugin| update(p)) {
            return pending;
        }
        ctx.add_plugin(Plugin::default());
        ctx.with_plugin(|p: &mut Plugin| update(p)).unwrap_or(true)
    }

    /// `modal` was shown in an earlier pass and is still open.
    pub(super) fn is_open(ctx: &Context, modal: Id) -> bool {
        ctx.with_plugin(|p: &mut Plugin| p.open.iter().any(|o| o.modal == modal)).unwrap_or(false)
    }

    /// Keep this pass's shapes of `modal` (on `layer`, resting layout) for its exit.
    ///
    /// The copy is made only when it can be shown: not in a pass egui will discard, and only
    /// when the layer differs from the kept copy (a pointer move over a settled dialog changes
    /// nothing, so it copies nothing). The kept buffer is reused, so a changed frame clones the
    /// shapes without reallocating the list.
    pub(super) fn snapshot(ctx: &Context, modal: Id, layer: LayerId, center: Pos2) {
        if ctx.will_discard() {
            return;
        }
        // the kept copy, or an empty one; nothing when the dialog is not tracked
        let Some(kept) = ctx.with_plugin(|p: &mut Plugin| p.open.iter_mut().find(|o| o.modal == modal).map(|o| o.last_frame.take())).flatten() else {
            return;
        };
        let mut snap = kept.unwrap_or(Snapshot { layer, center, shapes: Vec::new() });
        let kept = ctx.graphics(|g| {
            let Some(l) = g.get(layer).filter(|l| l.next_idx().0 <= MAX_SNAPSHOT) else { return false };
            let same =
                snap.layer == layer && snap.shapes.len() == l.next_idx().0 && snap.shapes.iter().zip(l.all_entries()).all(|(a, b)| same_shape(a, b));
            if !same {
                snap.shapes.clear();
                snap.shapes.extend(l.all_entries().cloned());
            }
            true
        });
        snap.layer = layer;
        snap.center = center;
        ctx.with_plugin(|p: &mut Plugin| {
            if let Some(o) = p.open.iter_mut().find(|o| o.modal == modal) {
                o.last_frame = kept.then_some(snap);
            }
        });
    }

    /// The same shape, comparing text by its galley's identity (cached galleys are shared) rather
    /// than glyph by glyph.
    fn same_shape(a: &ClippedShape, b: &ClippedShape) -> bool {
        if a.clip_rect != b.clip_rect {
            return false;
        }
        match (&a.shape, &b.shape) {
            (Shape::Text(x), Shape::Text(y)) => {
                std::sync::Arc::ptr_eq(&x.galley, &y.galley)
                    && x.pos == y.pos
                    && x.underline == y.underline
                    && x.fallback_color == y.fallback_color
                    && x.override_text_color == y.override_text_color
                    && x.opacity_factor == y.opacity_factor
                    && x.angle == y.angle
            }
            (x, y) => x == y,
        }
    }

    /// Paint one pass of a closed dialog's exit; false once it has faded out.
    fn paint_exit(ctx: &Context, modal: Id, snap: &Snapshot) -> bool {
        let fx = modal.with("ashlar_ui::dialog_fx");
        let panel = motion::presence(ctx, fx.with("panel"), false, mt::POP);
        let backdrop = motion::presence(ctx, fx.with("backdrop"), false, mt::SNAP);
        if panel.is_none() && backdrop.is_none() {
            return false;
        }
        let (panel, backdrop) = (panel.unwrap_or(0.0), backdrop.unwrap_or(0.0));
        let s = if motion::prefs(ctx).reduced { 1.0 } else { 0.97 + 0.03 * panel };
        let ts = egui::emath::TSTransform::new(snap.center.to_vec2() * (1.0 - s), s);
        for (n, shape) in snap.shapes.iter().enumerate() {
            let mut painter = Painter::new(ctx.clone(), snap.layer, shape.clip_rect);
            let mut shape = shape.shape.clone();
            if n == 0 {
                painter.multiply_opacity(backdrop);
            } else {
                painter.multiply_opacity(panel);
                shape.transform(ts);
            }
            painter.add(shape);
        }
        true
    }

    /// The initial focus of `modal` has been placed.
    pub(super) fn focus_placed(ctx: &Context, modal: Id) {
        ctx.with_plugin(|p: &mut Plugin| {
            if let Some(o) = p.open.iter_mut().find(|o| o.modal == modal) {
                o.pending = false;
            }
        });
    }
}

fn dialog_button(ui: &mut Ui, t: &Tokens, id: &str, index: usize, b: &DialogButton<'_>) -> egui::Response {
    let variant = match (b.default, b.kind) {
        (_, ButtonKind::Destructive) => Variant::Destructive,
        (true, _) => Variant::Primary,
        (false, _) => Variant::Secondary,
    };
    Button::new(id, b.label).indexed("button", index).variant(variant).size(Size::Lg).min_width(t.control_height * 3.0).enabled(b.enabled).show(ui)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;
    use egui_kittest::Harness;

    struct State {
        outcomes: Vec<DialogOutcome>,
        text: String,
        multiline: bool,
        has_cancel: bool,
        save_enabled: bool,
        editor: Option<egui::Id>,
    }

    fn harness(multiline: bool, has_cancel: bool) -> Harness<'static, State> {
        let mut h = Harness::builder().with_size(vec2(800.0, 600.0)).build_ui_state(
            |ui, s: &mut State| {
                let mut buttons = vec![DialogButton::new("Don't Save"), DialogButton::new("Save").default().enabled(s.save_enabled)];
                if s.has_cancel {
                    buttons.insert(1, DialogButton::cancel("Cancel"));
                }
                let multiline = s.multiline;
                let text = &mut s.text;
                let editor = &mut s.editor;
                if let Some(o) = Dialog::new("unsaved", "Save changes?", &buttons).show(ui.ctx(), |ui| {
                    let r = if multiline { ui.text_edit_multiline(text) } else { ui.text_edit_singleline(text) };
                    *editor = Some(r.id);
                }) {
                    s.outcomes.push(o);
                }
            },
            State { outcomes: Vec::new(), text: String::new(), multiline, has_cancel, save_enabled: true, editor: None },
        );
        automation::install(&h.ctx);
        h.step();
        h.step();
        h
    }

    #[test]
    fn enter_activates_the_default_and_escape_the_cancel_button() {
        let mut h = harness(false, true);
        h.key_press(Key::Enter);
        h.step();
        assert_eq!(h.state().outcomes, [DialogOutcome::Button(2)]);
        h.key_press(Key::Escape);
        h.step();
        assert_eq!(h.state().outcomes, [DialogOutcome::Button(2), DialogOutcome::Button(1)]);
        let mut h = harness(false, false);
        h.key_press(Key::Escape);
        h.step();
        assert_eq!(h.state().outcomes, [DialogOutcome::Dismissed]);
    }

    #[test]
    fn disabled_default_ignores_enter_and_clicks() {
        let mut h = harness(false, true);
        h.state_mut().save_enabled = false;
        h.step();
        h.key_press(Key::Enter);
        h.step();
        let save = automation::find(&h.ctx, "unsaved.button.2").unwrap();
        assert!(!save.enabled);
        h.hover_at(save.center());
        h.step();
        h.drag_at(save.center());
        h.step();
        h.drop_at(save.center());
        h.step();
        assert!(h.state().outcomes.is_empty());
    }

    #[test]
    fn a_multiline_editor_keeps_enter_and_buttons_click() {
        let mut h = harness(true, true);
        let editor = h.state().editor.unwrap();
        h.ctx.memory_mut(|m| m.request_focus(editor));
        h.step();
        assert!(h.ctx.egui_wants_keyboard_input());
        h.key_press(Key::Enter);
        h.step();
        assert!(h.state().outcomes.is_empty(), "Enter went to the editor");
        assert_eq!(h.state().text, "\n");
        // a single-line editor gives Enter to the dialog
        let mut single = harness(false, true);
        let editor = single.state().editor.unwrap();
        single.ctx.memory_mut(|m| m.request_focus(editor));
        single.step();
        single.key_press(Key::Enter);
        single.step();
        assert_eq!(single.state().outcomes, [DialogOutcome::Button(2)]);
        let dont = automation::find(&h.ctx, "unsaved.button.0").unwrap();
        h.hover_at(dont.center());
        h.step();
        h.drag_at(dont.center());
        h.step();
        h.drop_at(dont.center());
        h.step();
        assert_eq!(h.state().outcomes.last(), Some(&DialogOutcome::Button(0)));
        assert_eq!(automation::find(&h.ctx, "unsaved").map(|e| e.role), Some(Role::Dialog));
    }

    struct Flow {
        open: bool,
        field: bool,
        default_enabled: bool,
        initial: InitialFocus,
        explicit: bool,
        text: String,
        opener: Option<Id>,
        other: Option<Id>,
        field_id: Option<Id>,
        outcomes: Vec<DialogOutcome>,
    }

    fn flow(field: bool) -> Harness<'static, Flow> {
        let state = Flow {
            open: false,
            field,
            default_enabled: true,
            initial: InitialFocus::Auto,
            explicit: false,
            text: String::new(),
            opener: None,
            other: None,
            field_id: None,
            outcomes: Vec::new(),
        };
        let mut h = Harness::builder().with_size(vec2(800.0, 600.0)).build_ui_state(
            |ui, s: &mut Flow| {
                let r = ui.button("Open");
                s.opener = Some(r.id);
                if r.clicked() {
                    s.open = true;
                }
                s.other = Some(ui.button("Other").id);
                if s.open {
                    let buttons = [DialogButton::cancel("Cancel"), DialogButton::new("OK").default().enabled(s.default_enabled)];
                    let mut d = Dialog::new("rename", "Rename layer", &buttons).description("Names show in the layer list.").initial_focus(s.initial);
                    if let (true, Some(other)) = (s.explicit, s.other) {
                        d = d.return_focus_to(other);
                    }
                    let (field, text, field_id) = (s.field, &mut s.text, &mut s.field_id);
                    if let Some(o) = d.show(ui.ctx(), |ui| {
                        ui.label("Body");
                        if field {
                            *field_id = Some(ui.text_edit_singleline(text).id);
                        }
                    }) {
                        s.outcomes.push(o);
                        s.open = false;
                    }
                }
            },
            state,
        );
        automation::install(&h.ctx);
        h.step();
        h
    }

    fn focused(h: &Harness<'_, Flow>) -> Option<Id> {
        h.ctx.memory(|m| m.focused())
    }

    fn steps(h: &mut Harness<'_, Flow>, n: usize) {
        for _ in 0..n {
            h.step();
        }
    }

    /// Open the dialog from the keyboard: focus "Open" and press Enter.
    fn open_by_keyboard(h: &mut Harness<'_, Flow>) {
        let opener = h.state().opener.unwrap();
        h.ctx.memory_mut(|m| m.request_focus(opener));
        steps(h, 2);
        h.key_press(Key::Enter);
        steps(h, 3);
        assert!(h.state().open);
    }

    fn ok_focused(h: &Harness<'_, Flow>, label: &str) -> bool {
        use egui_kittest::kittest::{NodeT, Queryable};
        h.get_by_label(label).accesskit_node().is_focused()
    }

    #[test]
    fn opening_focuses_the_first_field_and_closing_returns_to_the_opener() {
        let mut h = flow(true);
        open_by_keyboard(&mut h);
        assert_eq!(focused(&h), h.state().field_id, "the field has the focus");
        h.key_press(Key::Escape);
        steps(&mut h, 3);
        assert_eq!(h.state().outcomes, [DialogOutcome::Button(0)]);
        assert_eq!(focused(&h), h.state().opener, "back on the opener");
    }

    #[test]
    fn without_a_field_the_default_button_then_the_cancel_button_gets_the_focus() {
        let mut h = flow(false);
        open_by_keyboard(&mut h);
        assert!(ok_focused(&h, "OK"));
        // Enter on the focused default button activates it once
        h.key_press(Key::Enter);
        steps(&mut h, 3);
        assert_eq!(h.state().outcomes, [DialogOutcome::Button(1)]);
        assert_eq!(focused(&h), h.state().opener);

        let mut h = flow(false);
        h.state_mut().default_enabled = false;
        open_by_keyboard(&mut h);
        assert!(ok_focused(&h, "Cancel"));
        assert!(!ok_focused(&h, "OK"));
    }

    #[test]
    fn the_default_button_policy_skips_fields_and_keep_leaves_the_focus() {
        let mut h = flow(true);
        h.state_mut().initial = InitialFocus::DefaultButton;
        open_by_keyboard(&mut h);
        assert!(ok_focused(&h, "OK"));
        assert_ne!(focused(&h), h.state().field_id);

        let mut h = flow(true);
        h.state_mut().initial = InitialFocus::Keep;
        open_by_keyboard(&mut h);
        assert_ne!(focused(&h), h.state().field_id);
        assert!(!ok_focused(&h, "OK"));
    }

    #[test]
    fn a_pointer_opened_dialog_returns_the_focus_where_it_is_told() {
        for explicit in [false, true] {
            let mut h = flow(false);
            h.state_mut().explicit = explicit;
            let open = h.state().opener.unwrap();
            let at = h.ctx.read_response(open).unwrap().rect.center();
            crate::test_support::click(&mut h, at);
            steps(&mut h, 2);
            assert!(h.state().open);
            assert!(ok_focused(&h, "OK"));
            h.key_press(Key::Escape);
            steps(&mut h, 3);
            assert!(!h.state().open);
            let expected = if explicit { h.state().other } else { None };
            assert_eq!(focused(&h), expected, "explicit {explicit}");
        }
    }

    #[test]
    fn screen_readers_get_a_modal_dialog_with_a_description() {
        use egui::accesskit::Role as AkRole;
        use egui_kittest::kittest::{NodeT, Queryable};
        let mut h = flow(true);
        open_by_keyboard(&mut h);
        let node = h.get_by_role(AkRole::Dialog);
        let node = node.accesskit_node();
        assert_eq!(node.label().as_deref(), Some("Rename layer"));
        assert_eq!(node.description().as_deref(), Some("Names show in the layer list."));
        assert!(node.is_modal());
        let e = automation::find(&h.ctx, "rename").unwrap();
        assert_eq!(e.role, Role::Dialog);
    }
    const DT: f64 = 1.0 / 60.0;
    const SCREEN: Rect = Rect { min: egui::Pos2::ZERO, max: egui::pos2(800.0, 600.0) };

    /// One pass at time `t`, showing the dialog or not; the painted shapes' bounds (backdrop
    /// left out) and how many shapes were painted.
    fn frame(ctx: &egui::Context, t: f64, show: bool) -> (Rect, usize) {
        let input = egui::RawInput { time: Some(t), predicted_dt: DT as f32, screen_rect: Some(SCREEN), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            if show {
                let buttons = [DialogButton::cancel("Cancel"), DialogButton::new("OK").default()];
                Dialog::new("anim", "Title", &buttons).show(ui.ctx(), |ui| {
                    ui.label("body");
                });
            }
        });
        out.textures_delta.clear();
        let mut bounds = Rect::NOTHING;
        for c in &out.shapes {
            let r = c.shape.visual_bounding_rect();
            if r.is_positive() && r != SCREEN {
                bounds = bounds.union(r);
            }
        }
        (bounds, out.shapes.len())
    }

    fn animation_requests(ctx: &egui::Context) -> u64 {
        crate::wake::Scheduler::of(ctx).stats().requests_for(crate::wake::Reason::ANIMATION)
    }

    /// Opens the dialog at pass 2 and returns the panel bounds of each pass, the automation rect
    /// of each pass, and the time reached.
    fn open(ctx: &egui::Context) -> (Vec<Rect>, Vec<[f32; 4]>, f64) {
        automation::install(ctx);
        let mut t = 0.0;
        for _ in 0..2 {
            frame(ctx, t, false);
            t += DT;
        }
        let mut bounds = Vec::new();
        let mut rects = Vec::new();
        for _ in 0..60 {
            bounds.push(frame(ctx, t, true).0);
            t += DT;
            rects.push(automation::find(ctx, "anim").expect("dialog").rect);
        }
        (bounds, rects, t)
    }

    #[test]
    fn the_exit_shows_the_last_frame_after_unchanged_frames_are_skipped() {
        let ctx = egui::Context::default();
        let texts = |body: Option<&str>, t: f64| {
            let input = egui::RawInput { time: Some(t), predicted_dt: DT as f32, screen_rect: Some(SCREEN), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                if let Some(body) = body {
                    Dialog::new("swap", "Title", &[DialogButton::new("OK")]).show(ui.ctx(), |ui| {
                        ui.label(body);
                    });
                }
            });
            out.textures_delta.clear();
            let mut seen: Vec<String> = Vec::new();
            for c in &out.shapes {
                if let egui::Shape::Text(t) = &c.shape {
                    seen.push(t.galley.job.text.clone());
                }
            }
            seen
        };
        let mut t = 0.0;
        for _ in 0..3 {
            texts(None, t);
            t += DT;
        }
        for _ in 0..40 {
            texts(Some("first"), t);
            t += DT;
        }
        texts(Some("second"), t);
        t += DT;
        let exit = texts(None, t);
        assert!(exit.iter().any(|s| s == "second"), "the exit paints the latest content: {exit:?}");
        assert!(!exit.iter().any(|s| s == "first"));
    }

    #[test]
    fn opening_grows_and_fades_in_closing_fades_out_and_stops() {
        let ctx = egui::Context::default();
        let (bounds, rects, mut t) = open(&ctx);
        // hit testing and automation use the resting layout while it grows (egui places an area
        // on its second pass)
        let rest = *rects.last().unwrap();
        assert!(rects[1..].iter().all(|r| *r == rest), "{rects:?}");
        let settled = *bounds.last().unwrap();
        let growing = bounds.iter().find(|b| b.width() > 0.0 && b.width() < settled.width() - 0.5);
        assert!(growing.is_some(), "the panel never painted smaller than at rest: {bounds:?}");
        // settled: no more frames
        let flat = animation_requests(&ctx);
        for _ in 0..100 {
            frame(&ctx, t, true);
            t += DT;
        }
        assert_eq!(animation_requests(&ctx), flat);

        // closed: still painted a moment, then gone, then idle
        let (_, painted) = frame(&ctx, t, false);
        t += DT;
        assert!(painted > 0, "the dialog vanished at once");
        assert!(automation::find(&ctx, "anim").is_none());
        let mut gone = None;
        for k in 0..60 {
            let (_, n) = frame(&ctx, t, false);
            t += DT;
            if n == 0 {
                gone = Some(k);
                break;
            }
        }
        assert!(gone.is_some(), "the exit never finished");
        let flat = animation_requests(&ctx);
        for _ in 0..100 {
            frame(&ctx, t, false);
            t += DT;
        }
        assert_eq!(animation_requests(&ctx), flat);
    }

    #[test]
    fn reduced_motion_fades_without_growing() {
        let ctx = egui::Context::default();
        motion::set_prefs(&ctx, ashlar_motion::MotionPrefs::REDUCED);
        let (bounds, _, _) = open(&ctx);
        let settled = *bounds.last().unwrap();
        for b in bounds.iter().filter(|b| b.is_positive()).skip(1) {
            assert!((b.width() - settled.width()).abs() < 0.5, "grew under reduced motion: {b:?} vs {settled:?}");
        }
    }
}
