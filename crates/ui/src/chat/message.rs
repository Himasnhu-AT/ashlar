//! One message: who wrote it and its markdown body.

use egui::{Frame, Margin, RichText, Ui};

use super::{ChatLabels, ChatRole, default_labels};
use crate::markdown::{self, MarkdownResponse, MarkdownStyle};
use crate::tokens;

/// A message view: the role, then the body as markdown. User messages sit on a raised surface.
/// While [`Message::streaming`] is set, a "Writing…" line follows and the message reports itself
/// busy to assistive tech.
pub struct Message<'a> {
    id: &'a str,
    role: ChatRole,
    body: &'a str,
    streaming: bool,
    labels: Option<&'a ChatLabels>,
    style: Option<&'a MarkdownStyle>,
}

impl<'a> Message<'a> {
    /// `id` prefixes the markdown's automation ids and names its cache (unique per message).
    pub fn new(id: &'a str, role: ChatRole, body: &'a str) -> Self {
        Message { id, role, body, streaming: false, labels: None, style: None }
    }
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.streaming = streaming;
        self
    }
    pub fn labels(mut self, labels: &'a ChatLabels) -> Self {
        self.labels = Some(labels);
        self
    }
    /// The markdown style (default: from the theme's visuals and body size).
    pub fn style(mut self, style: &'a MarkdownStyle) -> Self {
        self.style = Some(style);
        self
    }

    pub fn show(self, ui: &mut Ui) -> MarkdownResponse {
        let labels: &ChatLabels = match self.labels {
            Some(l) => l,
            None => default_labels(),
        };
        let t = tokens::current(ui.ctx());
        let owned;
        let style = match self.style {
            Some(s) => s,
            None => {
                owned = MarkdownStyle::from_visuals(ui.visuals(), t.type_scale.body);
                &owned
            }
        };
        let fill = match self.role {
            ChatRole::User => t.surface.raised,
            _ => egui::Color32::TRANSPARENT,
        };
        let pad = t.space.sm as i8;
        let inner = Frame::new().fill(fill).corner_radius(t.radius.md).inner_margin(Margin::same(pad)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(self.role.label(labels)).size(t.type_scale.small).color(t.text.secondary).strong());
            let out = markdown::show(ui, self.id, self.body, style);
            if self.streaming {
                ui.label(RichText::new(labels.streaming.as_str()).size(t.type_scale.small).color(t.text.secondary).italics());
            }
            out
        });
        if self.streaming {
            ui.ctx().accesskit_node_builder(inner.response.id, |node| node.set_busy());
        }
        inner.inner
    }
}
