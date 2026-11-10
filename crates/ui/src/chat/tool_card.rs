//! A tool call: title, status, optional details and an approve / deny step.

use egui::accesskit::Role as NodeRole;
use egui::{Color32, Frame, Margin, RichText, Sense, Stroke, Ui, UiBuilder, vec2};

use super::{ChatLabels, default_labels};
use crate::automation::{self, Details, Role};
use crate::controls::{Button, Size, Variant};
use crate::tokens::{self, Tokens};

/// Where a tool call stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ToolStatus {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    /// Waiting for the person to approve or deny it.
    NeedsApproval,
}

impl ToolStatus {
    pub fn label(self, labels: &ChatLabels) -> &str {
        match self {
            ToolStatus::Pending => &labels.pending,
            ToolStatus::Running => &labels.running,
            ToolStatus::Succeeded => &labels.succeeded,
            ToolStatus::Failed => &labels.failed,
            ToolStatus::NeedsApproval => &labels.needs_approval,
        }
    }
    fn color(self, t: &Tokens) -> Color32 {
        match self {
            ToolStatus::Pending => t.text.secondary,
            ToolStatus::Running => t.status.info,
            ToolStatus::Succeeded => t.status.success,
            ToolStatus::Failed => t.status.danger,
            ToolStatus::NeedsApproval => t.status.warning,
        }
    }
}

/// The person's answer to an approval request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Approval {
    Approved,
    Denied,
}

/// What [`ToolCard::show`] reports.
pub struct ToolCardResponse {
    pub response: egui::Response,
    /// Set on the frame the person approved or denied (the `pending` flag is cleared then).
    pub approval: Option<Approval>,
    /// The details were opened or closed.
    pub toggled: bool,
}

type DetailsFn<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

/// A card for one tool call. Its details fold (`<id>.toggle`, closed by default); while
/// `pending` is set it shows Approve and Deny buttons (`<id>.approve`, `<id>.deny`), reachable
/// with Tab and pressed with Enter or Space.
pub struct ToolCard<'a> {
    id: &'a str,
    title: &'a str,
    status: ToolStatus,
    details: Option<DetailsFn<'a>>,
    pending: Option<&'a mut bool>,
    labels: Option<&'a ChatLabels>,
}

impl<'a> ToolCard<'a> {
    pub fn new(id: &'a str, title: &'a str, status: ToolStatus) -> Self {
        ToolCard { id, title, status, details: None, pending: None, labels: None }
    }
    /// Draws the folded details (arguments, output…).
    pub fn details(mut self, details: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.details = Some(Box::new(details));
        self
    }
    /// Ask for approval while `*pending` is true; the answer clears it.
    pub fn approval(mut self, pending: &'a mut bool) -> Self {
        self.pending = Some(pending);
        self
    }
    pub fn labels(mut self, labels: &'a ChatLabels) -> Self {
        self.labels = Some(labels);
        self
    }

    pub fn show(self, ui: &mut Ui) -> ToolCardResponse {
        let labels: &ChatLabels = match self.labels {
            Some(l) => l,
            None => default_labels(),
        };
        let t = tokens::current(ui.ctx());
        let base = ui.make_persistent_id(("ashlar_ui::chat::tool_card", self.id));
        let open_id = base.with("open");
        let mut open: bool = ui.data(|d| d.get_temp(open_id)).unwrap_or(false);
        let status_text = self.status.label(labels);
        ui.ctx().accesskit_node_builder(base, |node| {
            node.set_role(NodeRole::Group);
            node.set_label(self.title);
            node.set_description(status_text);
        });
        let mut approval = None;
        let mut toggled = false;
        let mut details = self.details;
        let pending = self.pending;
        let inner = ui.scope_builder(UiBuilder::new().accessibility_parent(base), |ui| {
            Frame::new()
                .fill(t.surface.panel)
                .stroke(Stroke::new(1.0, t.border))
                .corner_radius(t.radius.md)
                .inner_margin(Margin::same(t.space.sm as i8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 4.0, self.status.color(&t));
                        ui.label(RichText::new(self.title).strong());
                        ui.label(RichText::new(status_text).size(t.type_scale.small).color(t.text.secondary));
                        if details.is_some() {
                            let label = if open { &labels.hide_details } else { &labels.show_details };
                            if Button::new(self.id, label).part("toggle").variant(Variant::Ghost).size(Size::Sm).show(ui).clicked() {
                                open = !open;
                                toggled = true;
                            }
                        }
                    });
                    if open && let Some(d) = details.take() {
                        ui.add_space(t.space.xs);
                        d(ui);
                    }
                    if let Some(p) = pending
                        && *p
                    {
                        ui.add_space(t.space.xs);
                        ui.horizontal(|ui| {
                            if Button::new(self.id, &labels.approve).part("approve").variant(Variant::Primary).size(Size::Sm).show(ui).clicked() {
                                approval = Some(Approval::Approved);
                            }
                            if Button::new(self.id, &labels.deny).part("deny").variant(Variant::Secondary).size(Size::Sm).show(ui).clicked() {
                                approval = Some(Approval::Denied);
                            }
                        });
                        if approval.is_some() {
                            *p = false;
                        }
                    }
                })
                .response
        });
        if toggled {
            ui.data_mut(|d| d.insert_temp(open_id, open));
        }
        let response = inner.inner;
        automation::register_with(ui, &response, self.id, self.title, Role::Status, Details { value: Some(status_text), selected: None });
        ToolCardResponse { response, approval, toggled }
    }
}
