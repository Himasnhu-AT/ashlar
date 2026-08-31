//! The menu bar as plain, serializable data: one model for every renderer.
//!
//! An app builds a [`MenuSpec`] from its commands ([`MenuSpec::from_commands`]), adds what commands
//! do not say (separators, check marks, OS roles), and hands it to a renderer: `ashlar-native`
//! turns it into the OS menu bar (macOS, Windows), `ashlar-ui` draws it in the window (web,
//! Linux, or by choice). Activating an item delivers its `id`, the command id for items built from
//! commands, which the app runs through its one `execute` chokepoint.
//!
//! Renderers that keep live OS menus rebuild them only when [`MenuSpec::structure_hash`] changes
//! (items added or removed, menu key equivalents changed) and otherwise patch labels, enabled and
//! checked state in place when [`MenuSpec::state_hash`] changes.
//!
//! The model has no OS or UI types and is bounded: [`MAX_DEPTH`], [`MAX_NODES`], [`MAX_TEXT`]
//! ([`MenuSpec::validate`]); walks stop at the depth limit instead of recursing without end.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::CommandInfo;
use crate::shortcut::{self, Chord, Key};

/// Deepest submenu nesting accepted (the top-level menu counts as 1).
pub const MAX_DEPTH: usize = 8;
/// Most nodes in one menu bar.
pub const MAX_NODES: usize = 10_000;
/// Longest label or id.
pub const MAX_TEXT: usize = 256;

/// Items the OS performs itself (no command dispatch). Edit roles (`Undo` … `SelectAll`) go to
/// the OS's focused native control (a web view, a native text field); apps that draw their own
/// text fields use plain items for those instead. Renderers leave out roles their platform lacks
/// (an in-window menu bar draws none).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// The standard About panel (name and version from the bundle).
    About,
    /// macOS Services submenu.
    Services,
    Hide,
    HideOthers,
    ShowAll,
    /// Quit at once. Apps with unsaved documents use a plain `app.quit` item instead and ask first.
    Quit,
    CloseWindow,
    Minimize,
    Zoom,
    Fullscreen,
    BringAllToFront,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// What a submenu is to the OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubmenuRole {
    /// macOS lists open windows here.
    Window,
    /// macOS adds its help search field here.
    Help,
}

/// A clickable item. Activating it delivers `id`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// Unique in the bar; the command id for items built from commands (`file.save`).
    pub id: String,
    pub label: String,
    /// Portable shortcut (`Mod+Shift+S`, alternatives separated by ` / `). Only menu-safe ones
    /// become native key equivalents ([`Chord::is_menu_safe`]); the rest stay the app's window
    /// shortcuts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Why the item is disabled (shown on hover where the renderer can).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// `Some` makes it a check item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Submenu {
    pub label: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<SubmenuRole>,
    pub children: Vec<MenuNode>,
}

/// One node of the menu tree.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MenuNode {
    Item(Item),
    Separator,
    Submenu(Submenu),
    /// An OS-provided item; `label` overrides the OS's text (translations).
    Role {
        role: Role,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

/// The whole menu bar: top-level menus in order. On macOS the first is the application menu (its
/// title is replaced by the app name by the OS).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MenuSpec {
    pub menus: Vec<Submenu>,
}

impl Chord {
    /// Whether the chord may become a native menu key equivalent. Only ones with ⌘ / Ctrl (or a
    /// function key) do: a bare `V` or an Alt/Shift-only key would be swallowed by the menu while
    /// the user types into a text field, so those stay the app's own window shortcuts.
    pub fn is_menu_safe(&self) -> bool {
        self.mods.command || self.mods.ctrl || matches!(self.key, Key::F(_))
    }
}

/// The menu key equivalent of portable shortcut text: `None` when it does not parse or is not
/// menu-safe ([`Chord::is_menu_safe`]). Of alternatives (`Mod+Y / Mod+Shift+Z`) the first counts.
pub fn menu_chord(text: &str) -> Option<Chord> {
    let first = text.split(" / ").next()?;
    Chord::parse(first).ok().filter(Chord::is_menu_safe)
}

impl Item {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), accelerator: None, enabled: true, disabled_reason: None, checked: None }
    }

    pub fn accelerator(mut self, text: impl Into<String>) -> Self {
        self.accelerator = Some(text.into());
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// The native key equivalent, when the accelerator has one ([`menu_chord`]).
    pub fn menu_chord(&self) -> Option<Chord> {
        self.accelerator.as_deref().and_then(menu_chord)
    }
}

impl From<Item> for MenuNode {
    fn from(i: Item) -> Self {
        MenuNode::Item(i)
    }
}

impl From<Submenu> for MenuNode {
    fn from(s: Submenu) -> Self {
        MenuNode::Submenu(s)
    }
}

impl MenuNode {
    pub fn role(role: Role) -> Self {
        MenuNode::Role { role, label: None }
    }
}

impl Submenu {
    pub fn new(label: impl Into<String>, children: Vec<MenuNode>) -> Self {
        Self { label: label.into(), enabled: true, role: None, children }
    }

    pub fn with_role(mut self, role: SubmenuRole) -> Self {
        self.role = Some(role);
        self
    }
}

/// Where a node sits: indices from the top-level menu down.
pub type Path = Vec<usize>;

impl MenuSpec {
    pub fn new(menus: Vec<Submenu>) -> Self {
        Self { menus }
    }

    /// A menu bar mirrored from a command list: each command with a non-empty `menu` path goes
    /// into the submenu that path names (`["File", "Export"]` = File ▸ Export), in list order,
    /// with its label, shortcut (normalized when it parses), enabled state and disabled reason.
    /// Top-level menus follow `order`; menus not in it come after, in first-seen order. Paths
    /// deeper than [`MAX_DEPTH`] are cut there. Separators, check marks and roles are the app's to
    /// add ([`Self::separator_before`], [`Self::set_checked`]).
    pub fn from_commands(commands: &[CommandInfo], order: &[&str]) -> Self {
        let mut spec = MenuSpec::default();
        for c in commands {
            let Some((top, rest)) = c.menu.split_first() else { continue };
            let rest = rest.get(..MAX_DEPTH.saturating_sub(1)).unwrap_or(rest);
            let idx = match spec.menus.iter().position(|m| &m.label == top) {
                Some(i) => i,
                None => {
                    spec.menus.push(Submenu::new(top.clone(), vec![]));
                    spec.menus.len() - 1
                }
            };
            let Some(menu) = spec.menus.get_mut(idx) else { continue };
            let Some(children) = submenu_children(&mut menu.children, rest) else { continue };
            let mut item = Item::new(c.id.clone(), c.label.clone()).enabled(c.enabled);
            item.accelerator = c.shortcut.as_deref().map(|s| shortcut::normalize(s).unwrap_or_else(|| s.to_owned()));
            item.disabled_reason = if c.enabled { None } else { c.disabled_reason.clone() };
            children.push(item.into());
        }
        let rank = |label: &str| order.iter().position(|o| *o == label).unwrap_or(order.len());
        spec.menus.sort_by_key(|m| rank(&m.label));
        spec
    }

    /// Every item, depth first.
    pub fn items(&self) -> Vec<&Item> {
        let mut out = vec![];
        self.walk(&mut |_, n| {
            if let MenuNode::Item(i) = n {
                out.push(i);
            }
        });
        out
    }

    /// The item with `id` (for items built from commands, the command id).
    pub fn find(&self, id: &str) -> Option<&Item> {
        self.items().into_iter().find(|i| i.id == id)
    }

    /// Visit every node with its path, depth first, at most [`MAX_DEPTH`] deep.
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&[usize], &'a MenuNode)) {
        fn rec<'a>(nodes: &'a [MenuNode], path: &mut Vec<usize>, f: &mut dyn FnMut(&[usize], &'a MenuNode)) {
            if path.len() > MAX_DEPTH {
                return;
            }
            for (i, n) in nodes.iter().enumerate() {
                path.push(i);
                f(path, n);
                if let MenuNode::Submenu(s) = n {
                    rec(&s.children, path, f);
                }
                path.pop();
            }
        }
        for (i, m) in self.menus.iter().enumerate() {
            rec(&m.children, &mut vec![i], f);
        }
    }

    /// The children lists of every menu and submenu, depth first, until `f` returns true.
    fn any_children_mut(&mut self, f: &mut dyn FnMut(&mut Vec<MenuNode>) -> bool) -> bool {
        fn rec(nodes: &mut Vec<MenuNode>, depth: usize, f: &mut dyn FnMut(&mut Vec<MenuNode>) -> bool) -> bool {
            if depth > MAX_DEPTH {
                return false;
            }
            if f(nodes) {
                return true;
            }
            nodes.iter_mut().any(|n| match n {
                MenuNode::Submenu(s) => rec(&mut s.children, depth + 1, f),
                _ => false,
            })
        }
        self.menus.iter_mut().any(|m| rec(&mut m.children, 1, f))
    }

    /// Insert a separator before the item `id`, unless it is first in its menu or already follows
    /// one. Returns whether the item was found.
    pub fn separator_before(&mut self, id: &str) -> bool {
        self.any_children_mut(&mut |nodes| {
            let Some(i) = nodes.iter().position(|n| matches!(n, MenuNode::Item(it) if it.id == id)) else { return false };
            if i > 0 && !matches!(nodes.get(i - 1), Some(MenuNode::Separator)) {
                nodes.insert(i, MenuNode::Separator);
            }
            true
        })
    }

    /// Set the check mark of the item `id` (`None`: a plain item again). Returns whether it was
    /// found.
    pub fn set_checked(&mut self, id: &str, checked: Option<bool>) -> bool {
        self.any_children_mut(&mut |nodes| {
            for n in nodes.iter_mut() {
                if let MenuNode::Item(it) = n
                    && it.id == id
                {
                    it.checked = checked;
                    return true;
                }
            }
            false
        })
    }

    /// Fingerprint of everything that needs a native rebuild when it changes: the tree's shape,
    /// item ids, check-item-ness, roles and menu key equivalents. Labels, enabled and checked
    /// state are patched in place instead.
    pub fn structure_hash(&self) -> u64 {
        let mut h = DefaultHasher::new();
        fn rec(nodes: &[MenuNode], depth: usize, h: &mut DefaultHasher) {
            if depth > MAX_DEPTH {
                return;
            }
            for n in nodes {
                match n {
                    MenuNode::Item(i) => {
                        0u8.hash(h);
                        i.id.hash(h);
                        i.checked.is_some().hash(h);
                        i.menu_chord().hash(h);
                    }
                    MenuNode::Separator => 1u8.hash(h),
                    MenuNode::Submenu(s) => {
                        2u8.hash(h);
                        s.role.hash(h);
                        rec(&s.children, depth + 1, h);
                        3u8.hash(h);
                    }
                    MenuNode::Role { role, label } => {
                        4u8.hash(h);
                        role.hash(h);
                        // OS items can't be relabelled in place everywhere.
                        label.hash(h);
                    }
                }
            }
        }
        for m in &self.menus {
            5u8.hash(&mut h);
            m.role.hash(&mut h);
            rec(&m.children, 1, &mut h);
        }
        h.finish()
    }

    /// Fingerprint of what is patched in place: labels, enabled and checked state, disabled
    /// reasons.
    pub fn state_hash(&self) -> u64 {
        let mut h = DefaultHasher::new();
        for m in &self.menus {
            (&m.label, m.enabled).hash(&mut h);
        }
        self.walk(&mut |_, n| match n {
            MenuNode::Item(i) => (&i.label, i.enabled, i.checked, &i.disabled_reason).hash(&mut h),
            MenuNode::Submenu(s) => (&s.label, s.enabled).hash(&mut h),
            _ => {}
        });
        h.finish()
    }

    /// Check the spec: unique non-empty ids, bounded size, depth and text, parseable
    /// accelerators. Returns every problem found.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = vec![];
        let mut ids = std::collections::HashSet::new();
        let mut nodes = 0usize;
        /// Nesting depth below `nodes`, counted no further than one past the limit.
        fn depth(nodes: &[MenuNode], d: usize) -> usize {
            if d > MAX_DEPTH {
                return d;
            }
            nodes.iter().map(|n| if let MenuNode::Submenu(s) = n { depth(&s.children, d + 1) } else { d }).max().unwrap_or(d)
        }
        for m in &self.menus {
            if depth(&m.children, 1) > MAX_DEPTH {
                problems.push(format!("menu `{}` nests deeper than {MAX_DEPTH}", clip(&m.label)));
            }
            if m.label.chars().count() > MAX_TEXT {
                problems.push("a menu title is too long".into());
            }
        }
        self.walk(&mut |_, n| {
            nodes += 1;
            match n {
                MenuNode::Item(i) => {
                    if i.id.is_empty() || i.id.chars().count() > MAX_TEXT {
                        problems.push(format!("item `{}` has an empty or too long id", clip(&i.label)));
                    } else if !ids.insert(i.id.as_str()) {
                        problems.push(format!("id `{}` is used twice", clip(&i.id)));
                    }
                    if i.label.chars().count() > MAX_TEXT {
                        problems.push(format!("item `{}` has a too long label", clip(&i.id)));
                    }
                    if let Some(a) = &i.accelerator
                        && let Err(e) = shortcut::parse_list(a)
                    {
                        problems.push(format!("item `{}`: {e}", clip(&i.id)));
                    }
                }
                MenuNode::Submenu(s) if s.label.chars().count() > MAX_TEXT => problems.push("a submenu title is too long".into()),
                _ => {}
            }
        });
        if nodes > MAX_NODES {
            problems.push(format!("{nodes} nodes; at most {MAX_NODES}"));
        }
        if problems.is_empty() { Ok(()) } else { Err(problems) }
    }

    /// The node at `path`, if any.
    pub fn node(&self, path: &[usize]) -> Option<&MenuNode> {
        let (top, rest) = path.split_first()?;
        let mut nodes = &self.menus.get(*top)?.children;
        let (last, mid) = rest.split_last()?;
        for i in mid {
            match nodes.get(*i)? {
                MenuNode::Submenu(s) => nodes = &s.children,
                _ => return None,
            }
        }
        nodes.get(*last)
    }
}

/// The children of the submenu at `path` below `children`, created where missing.
fn submenu_children<'a>(children: &'a mut Vec<MenuNode>, path: &[String]) -> Option<&'a mut Vec<MenuNode>> {
    let Some((name, rest)) = path.split_first() else { return Some(children) };
    let pos = match children.iter().position(|n| matches!(n, MenuNode::Submenu(s) if &s.label == name)) {
        Some(p) => p,
        None => {
            children.push(Submenu::new(name.clone(), vec![]).into());
            children.len() - 1
        }
    };
    match children.get_mut(pos)? {
        MenuNode::Submenu(s) => submenu_children(&mut s.children, rest),
        _ => None,
    }
}

fn clip(s: &str) -> String {
    s.chars().take(64).collect()
}

#[cfg(test)]
mod tests;
