//! A native menu bar described by a plain, serializable [`MenuSpec`].
//!
//! The app builds the spec from its state every frame (cheap: plain data), and
//! [`NativeMenuBar::update`] turns it into the OS menu: it rebuilds only when
//! [`MenuSpec::structure_hash`] changes (items added or removed, accelerators changed) and
//! otherwise pushes label / enabled / checked changes in place, only when
//! [`MenuSpec::state_hash`] changes. Activations arrive as item ids on a channel, with a
//! [`crate::Waker`] call so an idle app wakes up.
//!
//! The spec has no OS or UI types, builds everywhere (the web uses an in-window menu bar drawn
//! from the same spec) and is tested without creating real menus. The native bar exists on macOS
//! (the app's menu bar) and Windows (a window's menu bar, see [`NativeMenuBar::attach_window`]);
//! elsewhere [`NativeMenuBar::install`] returns `Unsupported`.

mod accelerator;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod native;

use std::hash::{Hash, Hasher};

pub use accelerator::{Accelerator, Modifiers, menu_accelerator};
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub use native::NativeMenuBar;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
use crate::{NativeError, Result};

/// What [`NativeMenuBar::update`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Update {
    Unchanged,
    /// Labels / enabled / checked state pushed in place.
    Patched,
    /// The native menus were rebuilt.
    Rebuilt,
}

/// Deepest submenu nesting accepted.
pub const MAX_DEPTH: usize = 8;
/// Most nodes in one menu bar.
pub const MAX_NODES: usize = 10_000;
/// Longest label or id.
pub const MAX_TEXT: usize = 256;

/// Items the OS performs itself (no command dispatch). Edit roles (`Undo` … `SelectAll`) go to
/// the OS's focused native control (a web view, a native text field); apps that draw their own
/// text fields use plain items for those instead. Roles other platforms lack are left out there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SubmenuRole {
    /// macOS lists open windows here.
    Window,
    /// macOS adds its help search field here.
    Help,
}

/// A clickable item. Activating it delivers `id`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// Unique in the bar; usually a command id (`file.save`).
    pub id: String,
    pub label: String,
    /// Portable accelerator (`Mod+Shift+S`). Only menu-safe ones become native key equivalents
    /// ([`Accelerator::is_menu_safe`]); the rest stay the app's window shortcuts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// `Some` makes it a check item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MenuSpec {
    pub menus: Vec<Submenu>,
}

impl Item {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into(), accelerator: None, enabled: true, checked: None }
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

    /// The native key equivalent, when the accelerator has one.
    pub fn native_accelerator(&self) -> Option<Accelerator> {
        self.accelerator.as_deref().and_then(menu_accelerator)
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

    /// Fingerprint of everything that needs a native rebuild when it changes: the tree's shape,
    /// item ids, check-item-ness, roles and native accelerators. Labels, enabled and checked
    /// state are patched in place instead.
    pub fn structure_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        fn rec(nodes: &[MenuNode], depth: usize, h: &mut std::collections::hash_map::DefaultHasher) {
            if depth > MAX_DEPTH {
                return;
            }
            for n in nodes {
                match n {
                    MenuNode::Item(i) => {
                        0u8.hash(h);
                        i.id.hash(h);
                        i.checked.is_some().hash(h);
                        i.native_accelerator().map(|a| a.portable()).hash(h);
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

    /// Fingerprint of what is patched in place: labels, enabled and checked state.
    pub fn state_hash(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for m in &self.menus {
            (&m.label, m.enabled).hash(&mut h);
        }
        self.walk(&mut |_, n| match n {
            MenuNode::Item(i) => (&i.label, i.enabled, i.checked).hash(&mut h),
            MenuNode::Submenu(s) => (&s.label, s.enabled).hash(&mut h),
            _ => {}
        });
        h.finish()
    }

    /// Check the spec: unique non-empty ids, bounded size, depth and text, parseable
    /// accelerators. Returns every problem found.
    pub fn validate(&self) -> std::result::Result<(), Vec<String>> {
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
                        && let Err(e) = a.split(" / ").try_for_each(|a| Accelerator::parse(a).map(|_| ()))
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

    /// A menu bar mirrored from a command list: each command with a non-empty `menu` path goes
    /// into the submenu that path names (`["File", "Export"]` = File ▸ Export), in list order,
    /// with its label, shortcut and enabled state. Top-level menus follow `order`; menus not in
    /// it come after, in first-seen order. Separators and roles are the app's to add.
    pub fn from_commands(commands: &[ashlar_command::CommandInfo], order: &[&str]) -> Self {
        let mut spec = MenuSpec::default();
        for c in commands {
            let Some((top, rest)) = c.menu.split_first() else { continue };
            if rest.len() >= MAX_DEPTH {
                continue;
            }
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
            item.accelerator = c.shortcut.clone();
            children.push(item.into());
        }
        let rank = |label: &str| order.iter().position(|o| *o == label).unwrap_or(order.len());
        spec.menus.sort_by_key(|m| rank(&m.label));
        spec
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

/// No native menu bar on this platform: every call reports `Unsupported` (draw the spec in the
/// window instead).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub struct NativeMenuBar(());

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl NativeMenuBar {
    pub fn install(_spec: &MenuSpec, _waker: crate::Waker) -> Result<Self> {
        Err(NativeError::Unsupported)
    }
    pub fn attach_window(&self, _window: &impl raw_window_handle::HasWindowHandle) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn update(&mut self, _spec: &MenuSpec) -> Result<Update> {
        Err(NativeError::Unsupported)
    }
    pub fn try_recv(&self) -> Option<String> {
        None
    }
    pub fn drain(&self) -> Vec<String> {
        vec![]
    }
}

#[cfg(test)]
mod tests;
