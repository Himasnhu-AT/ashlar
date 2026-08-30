//! Modal dialog shell: title, body, and a row of buttons with keyboard defaults.
//!
//! - Enter activates the default button (unless a multi-line editor in the body has the focus,
//!   or the body consumed the key).
//! - Escape activates the cancel button (or closes with [`DialogOutcome::Dismissed`] when there
//!   is none). Only the topmost dialog reacts, and not while one of its popups is open.
//! - A click on the backdrop dismisses only when [`Dialog::dismiss_on_backdrop`] is set.
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

use egui::{Align2, Color32, Id, Key, Margin, Modifiers, Sense, Stroke, StrokeKind, Ui, vec2};

use crate::automation::{self, Details, Role};
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

/// The dialog. See the [module docs](self).
pub struct Dialog<'a> {
    id: &'a str,
    title: &'a str,
    buttons: &'a [DialogButton<'a>],
    width: f32,
    backdrop_dismisses: bool,
}

impl<'a> Dialog<'a> {
    pub fn new(id: &'a str, title: &'a str, buttons: &'a [DialogButton<'a>]) -> Self {
        Dialog { id, title, buttons, width: 380.0, backdrop_dismisses: false }
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
        let mut clicked = None;
        let modal = egui::Modal::new(modal_id).frame(frame).backdrop_color(backdrop).show(ctx, |ui| {
            ui.set_width(self.width.min(ctx.content_rect().width() - 32.0).max(120.0));
            ui.label(egui::RichText::new(self.title).size(t.type_scale.heading).strong().color(t.text.primary));
            ui.add_space(t.space.md);
            body(ui);
            ui.add_space(t.space.lg);
            clicked = button_row(ui, &t, self.id, self.buttons);
            automation::register_fmt(ui, ui.min_rect(), format_args!("{}", self.id), self.title, Role::Dialog, true, Details::default());
        });
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

    fn cancel_outcome(&self) -> DialogOutcome {
        match self.buttons.iter().position(|b| b.kind == ButtonKind::Cancel && b.enabled) {
            Some(i) => DialogOutcome::Button(i),
            None => DialogOutcome::Dismissed,
        }
    }
}

fn button_row(ui: &mut Ui, t: &Tokens, id: &str, buttons: &[DialogButton<'_>]) -> Option<usize> {
    let mut clicked = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = t.space.md;
        for (i, b) in buttons.iter().enumerate().rev() {
            if dialog_button(ui, t, id, i, b).clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

fn dialog_button(ui: &mut Ui, t: &Tokens, id: &str, index: usize, b: &DialogButton<'_>) -> egui::Response {
    let font = Tokens::font(t.type_scale.button);
    let galley = crate::cache::galley(ui, ("ashlar_ui::dialog_button", id, index), crate::cache::hash_of(&(b.label, font.size.to_bits())), || {
        egui::text::LayoutJob::simple_singleline(b.label.to_owned(), font, Color32::PLACEHOLDER)
    });
    let width = (galley.size().x + 2.0 * t.space.lg).max(t.control_height * 3.0);
    let sense = if b.enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(width, t.control_height + 4.0), sense);
    let hot = resp.hovered() && b.enabled;
    let (fill, text, border) = match (b.default, b.kind) {
        (_, ButtonKind::Destructive) => (if hot { t.status.danger.gamma_multiply(0.85) } else { t.status.danger }, Color32::WHITE, t.status.danger),
        (true, _) => (if hot { t.accent_hover } else { t.accent }, t.text.on_accent, t.accent),
        (false, _) => (if hot { t.surface.hover } else { t.surface.sunken }, t.text.primary, t.border),
    };
    let (fill, text) = if b.enabled { (fill, text) } else { (t.surface.sunken, t.text.disabled) };
    let painter = ui.painter();
    painter.rect_filled(rect, t.radius.md, fill);
    painter.rect_stroke(rect, t.radius.md, Stroke::new(1.0, border), StrokeKind::Inside);
    if resp.has_focus() {
        painter.rect_stroke(rect.expand(2.0), t.radius.md, Stroke::new(1.5, t.focus), StrokeKind::Outside);
    }
    painter.galley(Align2::CENTER_CENTER.align_size_within_rect(galley.size(), rect).min, galley, text);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, b.enabled, b.label));
    automation::register_fmt(
        ui,
        rect,
        format_args!("{id}.button.{index}"),
        b.label,
        Role::Button,
        b.enabled,
        Details { value: None, selected: Some(b.default) },
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
