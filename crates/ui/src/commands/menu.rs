//! The menu bar as a plain, serializable model built from the commands' menu paths, and an egui
//! renderer for it.
//!
//! [`MenuSpec::from_commands`] turns each command's `menu` path (`["File", "Export"]`) into nested
//! submenus, in the order the commands are declared. A native renderer (a macOS NSMenu) consumes
//! the same spec: it rebuilds its menus when [`MenuSpec::structure`] changes and otherwise only
//! updates labels, enabled and check state when [`MenuSpec::state`] changes. Item ids are
//! positional (`m.2.0.5`), stable while the structure is unchanged.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use ashlar_command::CommandInfo;
use ashlar_command::shortcut::{Platform, normalize};
use egui::{Align2, Rect, Sense, Stroke, Ui, pos2, text::LayoutJob, vec2};

use crate::automation::{self, Details, Role};
use crate::cache;
use crate::tokens::{self, Tokens};

/// Deepest submenu nesting accepted from menu paths.
pub const MAX_DEPTH: usize = 8;

/// One clickable item.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MenuItem {
    /// Positional id (`m.0.3`).
    pub id: String,
    /// The command it runs.
    pub command: String,
    pub label: String,
    /// Portable shortcut (`Mod+Shift+S`), normalized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// Check mark: `None` for plain items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
}

/// A node of the menu tree.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MenuNode {
    Item(MenuItem),
    Separator,
    Submenu { label: String, children: Vec<MenuNode> },
}

/// The whole menu bar: top-level menus in order.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MenuSpec {
    pub menus: Vec<MenuNode>,
}

/// The children of submenu `label` in `nodes`, created at the end when missing.
fn submenu<'a>(nodes: &'a mut Vec<MenuNode>, label: &str) -> Option<&'a mut Vec<MenuNode>> {
    let pos = nodes.iter().position(|n| matches!(n, MenuNode::Submenu { label: l, .. } if l == label));
    let i = match pos {
        Some(i) => i,
        None => {
            nodes.push(MenuNode::Submenu { label: label.to_owned(), children: Vec::new() });
            nodes.len() - 1
        }
    };
    match nodes.get_mut(i) {
        Some(MenuNode::Submenu { children, .. }) => Some(children),
        _ => None,
    }
}

impl MenuSpec {
    /// Build the bar from command menu paths (commands with an empty path are not in menus).
    pub fn from_commands(commands: &[CommandInfo]) -> Self {
        let mut spec = MenuSpec::default();
        for c in commands {
            let Some((first, rest)) = c.menu.split_first() else { continue };
            let mut nodes = submenu(&mut spec.menus, first);
            for label in rest.iter().take(MAX_DEPTH) {
                nodes = nodes.and_then(|n| submenu(n, label));
            }
            let Some(nodes) = nodes else { continue };
            nodes.push(MenuNode::Item(MenuItem {
                id: String::new(),
                command: c.id.clone(),
                label: c.label.clone(),
                shortcut: c.shortcut.as_deref().and_then(normalize),
                enabled: c.enabled,
                disabled_reason: if c.enabled { None } else { c.disabled_reason.clone() },
                checked: None,
            }));
        }
        spec.renumber();
        spec
    }

    fn renumber(&mut self) {
        fn rec(nodes: &mut [MenuNode], path: &mut String) {
            for (i, n) in nodes.iter_mut().enumerate() {
                let len = path.len();
                path.push('.');
                path.push_str(&i.to_string());
                match n {
                    MenuNode::Item(item) => item.id.clone_from(path),
                    MenuNode::Submenu { children, .. } => rec(children, path),
                    MenuNode::Separator => {}
                }
                path.truncate(len);
            }
        }
        rec(&mut self.menus, &mut String::from("m"));
    }

    fn walk_mut(&mut self, f: &mut dyn FnMut(&mut Vec<MenuNode>) -> bool) -> bool {
        fn rec(nodes: &mut Vec<MenuNode>, f: &mut dyn FnMut(&mut Vec<MenuNode>) -> bool) -> bool {
            if f(nodes) {
                return true;
            }
            nodes.iter_mut().any(|n| match n {
                MenuNode::Submenu { children, .. } => rec(children, f),
                _ => false,
            })
        }
        rec(&mut self.menus, f)
    }

    /// Insert a separator before the item running `command`. Returns whether it was found.
    pub fn separator_before(&mut self, command: &str) -> bool {
        let done = self.walk_mut(&mut |nodes| {
            let Some(i) = nodes.iter().position(|n| matches!(n, MenuNode::Item(it) if it.command == command)) else { return false };
            if i > 0 && !matches!(nodes.get(i - 1), Some(MenuNode::Separator)) {
                nodes.insert(i, MenuNode::Separator);
            }
            true
        });
        self.renumber();
        done
    }

    /// Set the check mark of the item running `command`.
    pub fn set_checked(&mut self, command: &str, checked: Option<bool>) -> bool {
        self.walk_mut(&mut |nodes| {
            for n in nodes.iter_mut() {
                if let MenuNode::Item(it) = n
                    && it.command == command
                {
                    it.checked = checked;
                    return true;
                }
            }
            false
        })
    }

    /// Every item, depth first.
    pub fn items(&self) -> Vec<&MenuItem> {
        fn rec<'a>(nodes: &'a [MenuNode], out: &mut Vec<&'a MenuItem>) {
            for n in nodes {
                match n {
                    MenuNode::Item(i) => out.push(i),
                    MenuNode::Submenu { children, .. } => rec(children, out),
                    MenuNode::Separator => {}
                }
            }
        }
        let mut out = Vec::new();
        rec(&self.menus, &mut out);
        out
    }

    /// The item with positional id `id`.
    pub fn find(&self, id: &str) -> Option<&MenuItem> {
        self.items().into_iter().find(|i| i.id == id)
    }

    /// The item running `command`.
    pub fn find_command(&self, command: &str) -> Option<&MenuItem> {
        self.items().into_iter().find(|i| i.command == command)
    }

    /// Fingerprint of what needs a rebuild of native menus: tree shape, commands, shortcuts,
    /// submenu labels, whether items are checkable.
    pub fn structure(&self) -> u64 {
        fn rec(nodes: &[MenuNode], h: &mut DefaultHasher) {
            for n in nodes {
                match n {
                    MenuNode::Item(i) => {
                        0u8.hash(h);
                        i.command.hash(h);
                        i.shortcut.hash(h);
                        i.checked.is_some().hash(h);
                    }
                    MenuNode::Separator => 1u8.hash(h),
                    MenuNode::Submenu { label, children } => {
                        2u8.hash(h);
                        label.hash(h);
                        rec(children, h);
                        3u8.hash(h);
                    }
                }
            }
        }
        let mut h = DefaultHasher::new();
        rec(&self.menus, &mut h);
        h.finish()
    }

    /// Fingerprint of what can be updated in place: labels, enabled, check marks, reasons.
    pub fn state(&self) -> u64 {
        let mut h = DefaultHasher::new();
        for i in self.items() {
            (&i.label, i.enabled, i.checked, &i.disabled_reason).hash(&mut h);
        }
        h.finish()
    }
}

/// Draw the menu bar; returns the command chosen this frame.
pub fn menu_bar(ui: &mut Ui, spec: &MenuSpec, platform: Platform) -> Option<String> {
    let mut chosen = None;
    egui::MenuBar::new().ui(ui, |ui| {
        for (i, node) in spec.menus.iter().enumerate() {
            match node {
                MenuNode::Submenu { label, children } => {
                    let r = ui.menu_button(label.as_str(), |ui| menu_items(ui, children, platform, &mut chosen, 0));
                    automation::register_fmt(ui, r.response.rect, format_args!("menu.{i}"), label, Role::Menu, true, Details::default());
                }
                MenuNode::Item(item) => {
                    if item_row(ui, item, platform, true).clicked() && item.enabled {
                        chosen = Some(item.command.clone());
                    }
                }
                MenuNode::Separator => {
                    ui.separator();
                }
            }
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
                if item_row(ui, item, platform, false).clicked() && item.enabled {
                    *chosen = Some(item.command.clone());
                    ui.close();
                }
            }
            MenuNode::Separator => {
                ui.separator();
            }
            MenuNode::Submenu { label, children } => {
                ui.menu_button(label.as_str(), |ui| menu_items(ui, children, platform, chosen, depth + 1));
            }
        }
    }
}

/// One menu row: check gutter, label, shortcut on the right; disabled rows explain why on hover.
fn item_row(ui: &mut Ui, item: &MenuItem, platform: Platform, top_level: bool) -> egui::Response {
    let t = tokens::current(ui.ctx());
    let font = Tokens::font(t.type_scale.body);
    let color = if item.enabled { t.text.primary } else { t.text.disabled };
    let label = cache::galley(ui, ("ashlar_ui::menu", &item.id), cache::hash_of(&(&item.label, color, font.size.to_bits())), || {
        LayoutJob::simple_singleline(item.label.clone(), font.clone(), color)
    });
    let shortcut = item.shortcut.as_deref().map(|s| {
        cache::galley(ui, ("ashlar_ui::menu-key", &item.id), cache::hash_of(&(s, platform == Platform::Mac, t.text.secondary)), || {
            let font = Tokens::font(t.type_scale.body);
            LayoutJob::simple_singleline(super::keys::shortcut_label(ui.ctx(), s, platform, &font).unwrap_or_default(), font, t.text.secondary)
        })
    });
    let gutter = if top_level { 0.0 } else { t.control_height * 0.8 };
    let key_w = shortcut.as_ref().map_or(0.0, |g| g.size().x + t.space.xl * 2.0);
    let width = (gutter + label.size().x + key_w + t.space.md * 2.0).max(if top_level { 0.0 } else { ui.available_width() });
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
    let cmd = &item.command;
    automation::register_fmt(
        ui,
        rect,
        format_args!("menu.item.{cmd}"),
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
    fn spec_follows_menu_paths_and_hashes_split_structure_from_state() {
        let mut spec = MenuSpec::from_commands(&commands());
        let tops: Vec<_> = spec.menus.iter().map(|m| if let MenuNode::Submenu { label, .. } = m { label.as_str() } else { "" }).collect();
        assert_eq!(tops, ["File", "Edit", "View"]);
        assert_eq!(spec.items().len(), 5);
        assert_eq!(spec.find_command("file.open").unwrap().shortcut.as_deref(), Some("Mod+O"));
        assert_eq!(spec.find_command("file.export.png").unwrap().id, "m.0.1.0");
        assert_eq!(spec.find_command("edit.cut").unwrap().disabled_reason.as_deref(), Some("Nothing selected"));
        let (structure, state) = (spec.structure(), spec.state());
        spec.set_checked("view.grid", Some(true));
        assert_ne!(spec.structure(), structure, "an item became checkable");
        let structure = spec.structure();
        spec.set_checked("view.grid", Some(false));
        assert_eq!(spec.structure(), structure);
        assert_ne!(spec.state(), state);
        assert!(spec.separator_before("file.quit"));
        assert!(!spec.separator_before("missing"));
        assert_eq!(spec.find_command("file.quit").unwrap().id, "m.0.3");
        let json = serde_json::to_string(&spec).unwrap();
        assert_eq!(serde_json::from_str::<MenuSpec>(&json).unwrap(), spec);
        assert!(json.contains(r#""type":"separator""#));
    }

    #[test]
    fn deep_paths_are_capped() {
        let deep: Vec<String> = (0..50).map(|i| format!("L{i}")).collect();
        let mut c = info("deep", "Deep", &[], None, true);
        c.menu = deep;
        let spec = MenuSpec::from_commands(&[c]);
        fn depth(nodes: &[MenuNode]) -> usize {
            nodes.iter().map(|n| if let MenuNode::Submenu { children, .. } = n { 1 + depth(children) } else { 0 }).max().unwrap_or(0)
        }
        assert_eq!(depth(&spec.menus), MAX_DEPTH + 1);
    }

    #[test]
    fn menu_bar_opens_menus_and_returns_the_chosen_command() {
        let spec = MenuSpec::from_commands(&commands());
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
