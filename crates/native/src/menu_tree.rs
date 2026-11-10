//! A [`MenuSpec`] as a popup menu: the tray's menu and context menus. [`plan`] is the pure
//! mapping (unit-tested); [`build`] makes the backend's menu from it.
//!
//! A popup has no menu bar, so: a spec with one top-level menu shows that menu's children; a spec
//! with several shows each as a submenu. Item ids get `prefix` so the process-wide menu events
//! ([`crate::menu_events`]) reach the popup's owner. Shortcuts are left out (a popup doesn't run
//! them), separators are tidied (none first, last or twice in a row) and OS roles a popup can't
//! show are dropped.

use ashlar_command::menu::{MAX_DEPTH, MAX_NODES, MenuNode, MenuSpec, Role};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Entry {
    Item { id: String, label: String, enabled: bool, checked: Option<bool> },
    Separator,
    Role { role: Role, label: Option<String> },
    Submenu { label: String, enabled: bool, children: Vec<Entry> },
}

/// Roles that mean something in a popup menu on this platform.
fn popup_role(role: Role) -> bool {
    match role {
        Role::About | Role::Quit | Role::Undo | Role::Redo | Role::Cut | Role::Copy | Role::Paste | Role::SelectAll => true,
        Role::Hide | Role::HideOthers | Role::ShowAll => cfg!(target_os = "macos"),
        Role::Services | Role::CloseWindow | Role::Minimize | Role::Zoom | Role::Fullscreen | Role::BringAllToFront => false,
    }
}

fn tidy(mut out: Vec<Entry>) -> Vec<Entry> {
    out.dedup_by(|a, b| *a == Entry::Separator && *b == Entry::Separator);
    while out.first() == Some(&Entry::Separator) {
        out.remove(0);
    }
    while out.last() == Some(&Entry::Separator) {
        out.pop();
    }
    out
}

fn nodes(list: &[MenuNode], prefix: &str, depth: usize, budget: &mut usize) -> Vec<Entry> {
    let mut out = vec![];
    if depth > MAX_DEPTH {
        return out;
    }
    for n in list {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        match n {
            MenuNode::Item(it) => {
                out.push(Entry::Item { id: format!("{prefix}{}", it.id), label: it.label.clone(), enabled: it.enabled, checked: it.checked })
            }
            MenuNode::Separator => out.push(Entry::Separator),
            MenuNode::Role { role, label } if popup_role(*role) => out.push(Entry::Role { role: *role, label: label.clone() }),
            MenuNode::Role { .. } => {}
            MenuNode::Submenu(s) => {
                let children = nodes(&s.children, prefix, depth + 1, budget);
                out.push(Entry::Submenu { label: s.label.clone(), enabled: s.enabled, children });
            }
        }
    }
    tidy(out)
}

/// The popup entries for `spec`, item ids prefixed with `prefix`.
pub(crate) fn plan(spec: &MenuSpec, prefix: &str) -> Vec<Entry> {
    let mut budget = MAX_NODES;
    match spec.menus.as_slice() {
        [only] => nodes(&only.children, prefix, 1, &mut budget),
        menus => menus
            .iter()
            .map(|m| Entry::Submenu { label: m.label.clone(), enabled: m.enabled, children: nodes(&m.children, prefix, 2, &mut budget) })
            .collect(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod build {
    use muda::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

    use super::Entry;
    use crate::{NativeError, Result};

    fn os(e: impl std::fmt::Display) -> NativeError {
        NativeError::Os(crate::os_text(e))
    }

    fn predefined(role: ashlar_command::menu::Role, label: Option<&str>) -> Option<PredefinedMenuItem> {
        use ashlar_command::menu::Role;
        Some(match role {
            Role::About => PredefinedMenuItem::about(label, None),
            Role::Quit => PredefinedMenuItem::quit(label),
            Role::Undo => PredefinedMenuItem::undo(label),
            Role::Redo => PredefinedMenuItem::redo(label),
            Role::Cut => PredefinedMenuItem::cut(label),
            Role::Copy => PredefinedMenuItem::copy(label),
            Role::Paste => PredefinedMenuItem::paste(label),
            Role::SelectAll => PredefinedMenuItem::select_all(label),
            #[cfg(target_os = "macos")]
            Role::Hide => PredefinedMenuItem::hide(label),
            #[cfg(target_os = "macos")]
            Role::HideOthers => PredefinedMenuItem::hide_others(label),
            #[cfg(target_os = "macos")]
            Role::ShowAll => PredefinedMenuItem::show_all(label),
            _ => return None,
        })
    }

    fn fill(entries: &[Entry], append: &mut dyn FnMut(&dyn IsMenuItem) -> Result<()>) -> Result<()> {
        for e in entries {
            match e {
                Entry::Item { id, label, enabled, checked: Some(on) } => append(&CheckMenuItem::with_id(id.clone(), label, *enabled, *on, None))?,
                Entry::Item { id, label, enabled, checked: None } => append(&MenuItem::with_id(id.clone(), label, *enabled, None))?,
                Entry::Separator => append(&PredefinedMenuItem::separator())?,
                Entry::Role { role, label } => {
                    if let Some(p) = predefined(*role, label.as_deref()) {
                        append(&p)?;
                    }
                }
                Entry::Submenu { label, enabled, children } => {
                    let sub = Submenu::new(label, *enabled);
                    fill(children, &mut |i| sub.append(i).map_err(os))?;
                    append(&sub)?;
                }
            }
        }
        Ok(())
    }

    /// The backend menu for `entries`.
    pub(crate) fn build(entries: &[Entry]) -> Result<Menu> {
        crate::guarded("the popup menu", || {
            let menu = Menu::new();
            fill(entries, &mut |i| menu.append(i).map_err(os))?;
            Ok(menu)
        })
    }
}
#[cfg(not(target_arch = "wasm32"))]
#[allow(unused_imports)] // used by the tray and context menus, not by the menu bar alone
pub(crate) use build::build;

#[cfg(test)]
mod tests {
    use ashlar_command::menu::{Item, Submenu};

    use super::*;

    fn item(id: &str) -> MenuNode {
        MenuNode::Item(Item::new(id, id.to_uppercase()))
    }

    #[test]
    fn one_menu_shows_its_children_with_prefixed_ids() {
        let spec = MenuSpec::new(vec![Submenu::new(
            "Tray",
            vec![item("show"), MenuNode::Separator, MenuNode::Item(Item::new("mute", "Mute").checked(true)), MenuNode::role(Role::Quit)],
        )]);
        let p = plan(&spec, "\u{1}t/");
        assert_eq!(p.len(), 4);
        assert_eq!(p[0], Entry::Item { id: "\u{1}t/show".into(), label: "SHOW".into(), enabled: true, checked: None });
        assert_eq!(p[1], Entry::Separator);
        assert!(matches!(&p[2], Entry::Item { checked: Some(true), .. }));
        assert_eq!(p[3], Entry::Role { role: Role::Quit, label: None });
    }

    #[test]
    fn several_menus_become_submenus() {
        let spec = MenuSpec::new(vec![Submenu::new("A", vec![item("a")]), Submenu::new("B", vec![item("b")])]);
        let p = plan(&spec, "");
        assert!(matches!(&p[..], [Entry::Submenu { label: a, .. }, Entry::Submenu { label: b, .. }] if a == "A" && b == "B"));
    }

    #[test]
    fn separators_are_tidied_and_unsuitable_roles_dropped() {
        let spec = MenuSpec::new(vec![Submenu::new(
            "T",
            vec![
                MenuNode::Separator,
                item("a"),
                MenuNode::Separator,
                MenuNode::role(Role::Zoom),
                MenuNode::Separator,
                item("b"),
                MenuNode::Separator,
            ],
        )]);
        let p = plan(&spec, "");
        assert_eq!(p.len(), 3, "{p:?}");
        assert_eq!(p[1], Entry::Separator);
    }

    #[test]
    fn deep_or_huge_specs_are_cut_off() {
        let mut node = item("leaf");
        for _ in 0..(MAX_DEPTH + 5) {
            node = MenuNode::Submenu(Submenu::new("s", vec![node]));
        }
        let p = plan(&MenuSpec::new(vec![Submenu::new("T", vec![node])]), "");
        fn depth(e: &[Entry]) -> usize {
            e.iter().map(|e| if let Entry::Submenu { children, .. } = e { 1 + depth(children) } else { 0 }).max().unwrap_or(0)
        }
        assert!(depth(&p) <= MAX_DEPTH);
        let many = MenuSpec::new(vec![Submenu::new("T", (0..MAX_NODES + 10).map(|i| item(&i.to_string())).collect())]);
        assert_eq!(plan(&many, "").len(), MAX_NODES);
    }
}
