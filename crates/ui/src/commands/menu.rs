//! The in-window menu bar: an egui renderer for [`ashlar_command::menu::MenuSpec`].
//!
//! The model (built from the commands' menu paths with [`MenuSpec::from_commands`], plus
//! separators and check marks) lives in `ashlar-command`, so one spec feeds both this bar and the
//! native menu bar of `ashlar-native`. Its types are re-exported here so
//! `ashlar_ui::commands::MenuSpec` keeps working. OS roles (About, Services…) have no in-window
//! form and are left out; activating an item returns its id (the command id).

use ashlar_command::shortcut::Platform;
use egui::{Align2, Rect, Sense, Stroke, Ui, pos2, text::LayoutJob, vec2};

pub use ashlar_command::menu::{Item, MAX_DEPTH, MenuNode, MenuSpec, Submenu};

use crate::automation::{self, Details, Role};
use crate::cache;
use crate::tokens::{self, Tokens};

/// Deprecated: menu items are [`ashlar_command::menu::Item`] (`id` is the command id).
#[deprecated(note = "use ashlar_command::menu::Item")]
pub type MenuItem = Item;

/// Draw the menu bar; returns the command chosen this frame.
pub fn menu_bar(ui: &mut Ui, spec: &MenuSpec, platform: Platform) -> Option<String> {
    let mut chosen = None;
    egui::MenuBar::new().ui(ui, |ui| {
        for (i, menu) in spec.menus.iter().enumerate() {
            let r = ui
                .add_enabled_ui(menu.enabled, |ui| ui.menu_button(menu.label.as_str(), |ui| menu_items(ui, &menu.children, platform, &mut chosen, 1)))
                .inner;
            automation::register_fmt(ui, r.response.rect, format_args!("menu.{i}"), &menu.label, Role::Menu, menu.enabled, Details::default());
        }
    });
    chosen
}

fn menu_items(ui: &mut Ui, nodes: &[MenuNode], platform: Platform, chosen: &mut Option<String>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for node in nodes {
        match node {
            MenuNode::Item(item) => {
                if item_row(ui, item, platform).clicked() && item.enabled {
                    *chosen = Some(item.id.clone());
                    ui.close();
                }
            }
            MenuNode::Separator => {
                ui.separator();
            }
            MenuNode::Submenu(sub) => {
                ui.add_enabled_ui(sub.enabled, |ui| {
                    ui.menu_button(sub.label.as_str(), |ui| menu_items(ui, &sub.children, platform, chosen, depth + 1))
                });
            }
            // Performed by the OS; nothing to draw in a window.
            MenuNode::Role { .. } => {}
        }
    }
}

/// One menu row: check gutter, label, shortcut on the right; disabled rows explain why on hover.
fn item_row(ui: &mut Ui, item: &Item, platform: Platform) -> egui::Response {
    let t = tokens::current(ui.ctx());
    let font = Tokens::font(t.type_scale.body);
    let color = if item.enabled { t.text.primary } else { t.text.disabled };
    let label = cache::galley(ui, ("ashlar_ui::menu", &item.id), cache::hash_of(&(&item.label, color, font.size.to_bits())), || {
        LayoutJob::simple_singleline(item.label.clone(), font.clone(), color)
    });
    let shortcut = item.accelerator.as_deref().map(|s| {
        cache::galley(ui, ("ashlar_ui::menu-key", &item.id), cache::hash_of(&(s, platform == Platform::Mac, t.text.secondary)), || {
            let font = Tokens::font(t.type_scale.body);
            LayoutJob::simple_singleline(super::keys::shortcut_label(ui.ctx(), s, platform, &font).unwrap_or_default(), font, t.text.secondary)
        })
    });
    let gutter = t.control_height * 0.8;
    let key_w = shortcut.as_ref().map_or(0.0, |g| g.size().x + t.space.xl * 2.0);
    let width = (gutter + label.size().x + key_w + t.space.md * 2.0).max(ui.available_width());
    let (rect, resp) = ui.allocate_exact_size(vec2(width, t.control_height), Sense::click());
    let hot = resp.hovered() && item.enabled;
    if hot {
        ui.painter().rect_filled(rect, t.radius.sm, t.accent);
    }
    let text_color = if hot { t.text.on_accent } else { color };
    if item.checked == Some(true) {
        let c = pos2(rect.left() + gutter / 2.0 + 2.0, rect.center().y);
        let s = Stroke::new(1.6, text_color);
        ui.painter().line_segment([c + vec2(-4.0, 0.0), c + vec2(-1.0, 3.0)], s);
        ui.painter().line_segment([c + vec2(-1.0, 3.0), c + vec2(4.5, -3.5)], s);
    }
    ui.painter().galley(pos2(rect.left() + gutter + t.space.md, rect.center().y - label.size().y / 2.0), label, text_color);
    if let Some(g) = shortcut {
        let r = Rect::from_min_max(rect.min, pos2(rect.right() - t.space.md, rect.bottom()));
        ui.painter().galley(Align2::RIGHT_CENTER.align_size_within_rect(g.size(), r).min, g, if hot { t.text.on_accent } else { t.text.secondary });
    }
    let resp = match (&item.disabled_reason, item.enabled) {
        (Some(reason), false) => resp.on_hover_text(reason.as_str()),
        _ => resp,
    };
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, item.enabled, item.checked == Some(true), &item.label));
    let id = &item.id;
    automation::register_fmt(
        ui,
        rect,
        format_args!("menu.item.{id}"),
        &item.label,
        Role::MenuItem,
        item.enabled,
        Details { value: None, selected: item.checked },
    );
    resp
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_support::click;
    use ashlar_command::CommandInfo;
    use egui_kittest::Harness;

    pub(crate) fn info(id: &str, label: &str, menu: &[&str], shortcut: Option<&str>, enabled: bool) -> CommandInfo {
        CommandInfo {
            id: id.into(),
            label: label.into(),
            menu: menu.iter().map(|s| (*s).to_owned()).collect(),
            shortcut: shortcut.map(Into::into),
            params_schema: serde_json::Value::Null,
            params_text: None,
            enabled,
            disabled_reason: (!enabled).then(|| "Nothing selected".to_owned()),
            mutates: false,
        }
    }

    fn commands() -> Vec<CommandInfo> {
        vec![
            info("file.open", "Open…", &["File"], Some("cmd+o"), true),
            info("file.export.png", "PNG", &["File", "Export"], None, true),
            info("file.quit", "Quit", &["File"], Some("Mod+Q"), true),
            info("edit.cut", "Cut", &["Edit"], Some("Mod+X"), false),
            info("view.grid", "Grid", &["View"], None, true),
            info("palette.only", "Not in menus", &[], None, true),
        ]
    }

    #[test]
    fn menu_bar_opens_menus_and_returns_the_chosen_command() {
        let mut spec = MenuSpec::from_commands(&commands(), &[]);
        spec.separator_before("file.quit");
        spec.set_checked("view.grid", Some(true));
        let mut h = Harness::builder().with_size(vec2(600.0, 400.0)).build_ui_state(
            move |ui, chosen: &mut Vec<String>| {
                chosen.extend(menu_bar(ui, &spec, Platform::Other));
            },
            Vec::new(),
        );
        automation::install(&h.ctx);
        h.step();
        h.step();
        let file = automation::find(&h.ctx, "menu.0").unwrap();
        assert_eq!(file.label, "File");
        click(&mut h, file.center());
        h.step();
        let open = automation::find(&h.ctx, "menu.item.file.open").unwrap();
        assert!(open.enabled);
        click(&mut h, open.center());
        assert_eq!(h.state(), &["file.open"]);
        // disabled items do nothing
        let edit = automation::find(&h.ctx, "menu.1").unwrap();
        click(&mut h, edit.center());
        h.step();
        let cut = automation::find(&h.ctx, "menu.item.edit.cut").unwrap();
        assert!(!cut.enabled);
        click(&mut h, cut.center());
        assert_eq!(h.state().len(), 1);
    }
}
