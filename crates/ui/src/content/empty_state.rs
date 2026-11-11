//! What a view shows when it has nothing to show.

use egui::{Response, RichText, Sense, Ui, vec2};

use crate::automation::{self, Details, Role};
use crate::controls::{Button, IconPainter, Variant};
use crate::tokens;

/// A centred icon, title, explanation and optional action: an empty list, a search with no
/// results, a document that failed to load.
pub struct EmptyState<'a> {
    id: &'a str,
    title: &'a str,
    body: Option<&'a str>,
    action: Option<&'a str>,
    icon: Option<IconPainter<'a>>,
}

/// What [`EmptyState::show`] reports.
pub struct EmptyStateOutput {
    /// The whole block.
    pub response: Response,
    /// The action button was clicked.
    pub action: bool,
}

impl<'a> EmptyState<'a> {
    pub fn new(id: &'a str, title: &'a str) -> Self {
        EmptyState { id, title, body: None, action: None, icon: None }
    }
    /// A sentence or two under the title.
    pub fn body(mut self, text: &'a str) -> Self {
        self.body = Some(text);
        self
    }
    /// A primary button under the text (registered as `<id>.action`).
    pub fn action(mut self, label: &'a str) -> Self {
        self.action = Some(label);
        self
    }
    /// An icon above the title, painted in the secondary text colour.
    pub fn icon(mut self, paint: IconPainter<'a>) -> Self {
        self.icon = Some(paint);
        self
    }

    pub fn show(self, ui: &mut Ui) -> EmptyStateOutput {
        let t = tokens::current(ui.ctx());
        let mut action = false;
        let inner = ui.vertical_centered(|ui| {
            let title = self.title;
            ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
                node.set_role(egui::accesskit::Role::Group);
                node.set_label(title);
            });
            ui.add_space(t.space.xl);
            if let Some(paint) = self.icon {
                let side = t.control_height * 1.5;
                let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
                paint(ui.painter(), rect, t.text.secondary);
                ui.add_space(t.space.md);
            }
            ui.label(RichText::new(self.title).size(t.type_scale.heading).strong().color(t.text.primary));
            if let Some(body) = self.body {
                ui.add_space(t.space.xs);
                ui.label(RichText::new(body).color(t.text.secondary));
            }
            if let Some(label) = self.action {
                ui.add_space(t.space.lg);
                action = Button::new(self.id, label).part("action").variant(Variant::Primary).show(ui).clicked();
            }
            ui.add_space(t.space.xl);
        });
        let response = inner.response;
        automation::register_with(ui, &response, self.id, self.title, Role::Group, Details { value: self.body, selected: None });
        EmptyStateOutput { response, action }
    }
}
