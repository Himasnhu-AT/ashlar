//! Help text on hover.

use egui::Response;

use crate::automation::{self, Details, Role};

/// Show `text` in a tooltip once the pointer rests on `response` for the theme's
/// `motion.tooltip_delay` (see [`crate::tokens::MotionTokens`]). Screen readers get the text as
/// the control's description right away.
///
/// ```
/// # fn frame(ui: &mut egui::Ui) {
/// ashlar_ui::help(ui.button("Sync"), "Upload local changes and fetch remote ones");
/// # }
/// ```
pub fn help(response: Response, text: &str) -> Response {
    response.ctx.accesskit_node_builder(response.id, |node| node.set_description(text));
    response.on_hover_ui(|ui| {
        let label = ui.label(text);
        ui.ctx().accesskit_node_builder(label.id, |node| {
            node.set_role(egui::accesskit::Role::Tooltip);
            node.set_label(text);
        });
        automation::register_fmt(ui, label.rect, format_args!("tooltip"), text, Role::Tooltip, true, Details { value: Some(text), selected: None });
    })
}
