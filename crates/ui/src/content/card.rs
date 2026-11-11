//! A bordered group of related content.

use egui::{InnerResponse, Margin, RichText, Stroke, Ui};

use crate::automation::{self, Details, Role};
use crate::tokens;

/// A panel-coloured frame with an optional title around content the app draws.
pub struct Card<'a> {
    id: &'a str,
    title: Option<&'a str>,
}

impl<'a> Card<'a> {
    pub fn new(id: &'a str) -> Self {
        Card { id, title: None }
    }
    /// A heading at the top, also the group's accessible name.
    pub fn title(mut self, title: &'a str) -> Self {
        self.title = Some(title);
        self
    }

    pub fn show<R>(self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
        let t = tokens::current(ui.ctx());
        let title = self.title;
        let inner = egui::Frame::new()
            .fill(t.surface.panel)
            .stroke(Stroke::new(1.0, t.border))
            .corner_radius(t.radius.lg)
            .inner_margin(Margin::same(t.space.lg.round().clamp(0.0, 127.0) as i8))
            .show(ui, |ui| {
                ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
                    node.set_role(egui::accesskit::Role::Group);
                    if let Some(title) = title {
                        node.set_label(title);
                    }
                });
                if let Some(title) = title {
                    ui.label(RichText::new(title).size(t.type_scale.heading).strong().color(t.text.primary));
                    ui.add_space(t.space.sm);
                }
                add(ui)
            });
        automation::register_with(ui, &inner.response, self.id, title.unwrap_or_default(), Role::Group, Details::default());
        inner
    }
}
