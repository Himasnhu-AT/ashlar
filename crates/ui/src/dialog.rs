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

use egui::{Color32, FocusDirection, Id, Key, Margin, Modifiers, Stroke, Ui};

use crate::automation::{self, Details, Role};
use crate::controls::{Button, Size, Variant};
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
        let mut clicked = None;
        let focus_pending = focus_return::track(ctx, modal_id, self.return_to);
        let modal = egui::Modal::new(modal_id).frame(frame).backdrop_color(backdrop).show(ctx, |ui| {
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

/// Returning the focus to the opener: open dialogs are tracked by the pass they were last shown
/// in, and one that was not shown in a completed pass is closed.
mod focus_return {
    use egui::{Context, Id, Ui};

    /// Dialogs open at once (nested confirmations), at most.
    const MAX_OPEN: usize = 32;

    struct Open {
        modal: Id,
        opener: Option<Id>,
        seen: u64,
        /// The initial focus is still to be placed.
        pending: bool,
    }

    #[derive(Default)]
    struct Plugin {
        open: Vec<Open>,
    }

    impl egui::Plugin for Plugin {
        fn debug_name(&self) -> &'static str {
            "ashlar_ui::dialog::focus_return"
        }
        fn on_end_pass(&mut self, ui: &mut Ui) {
            let ctx = ui.ctx();
            if ctx.will_discard() || self.open.is_empty() {
                return;
            }
            let pass = ctx.cumulative_pass_nr();
            let mut restore = None;
            self.open.retain(|o| {
                let shown = o.seen == pass;
                if !shown && restore.is_none() {
                    restore = o.opener;
                }
                shown
            });
            if let Some(id) = restore {
                ctx.memory_mut(|m| m.request_focus(id));
            }
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
                p.open.push(Open { modal, opener: return_to.or(focused), seen: pass, pending: true });
                true
            }
        };
        if let Some(pending) = ctx.with_plugin(|p: &mut Plugin| update(p)) {
            return pending;
        }
        ctx.add_plugin(Plugin::default());
        ctx.with_plugin(|p: &mut Plugin| update(p)).unwrap_or(true)
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
}
