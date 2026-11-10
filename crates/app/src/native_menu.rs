//! The app's commands in the OS menu bar (feature `native-menu`, macOS and Windows): the same
//! command list the in-window bar is built from, mirrored into `ashlar_native::menu`.
//!
//! Text-editing shortcuts (⌘Z, ⌘⇧Z, ⌘Y, ⌘A/C/X/V) get no native key equivalent: the OS menu would
//! take the key before the window sees it, so ⌘Z in a text field would undo the document. The
//! items stay in the menu (a click runs the command); their keys reach the window, where the
//! keymap leaves them to a focused text field ([`ashlar_ui::commands::keys::works_while_typing`]).
//! The menus therefore don't show those shortcuts; the command palette and keymap editor do.

use ashlar_command::CommandInfo;
#[cfg(target_os = "macos")]
use ashlar_native::menu::SubmenuRole;
use ashlar_native::menu::{MenuNode, MenuSpec, NativeMenuBar, Role, Submenu};
use ashlar_ui::commands::keys::text_editing_chord;

pub(crate) struct NativeMenu {
    bar: NativeMenuBar,
}

/// Take the item `id` out of the spec (any depth), if it is there.
#[cfg(any(target_os = "macos", test))]
fn take_item(nodes: &mut Vec<MenuNode>, id: &str) -> Option<MenuNode> {
    if let Some(i) = nodes.iter().position(|n| matches!(n, MenuNode::Item(it) if it.id == id)) {
        return Some(nodes.remove(i));
    }
    nodes.iter_mut().find_map(|n| match n {
        MenuNode::Submenu(s) => take_item(&mut s.children, id),
        _ => None,
    })
}

/// Drop the native key equivalent of items whose shortcut a text field owns.
fn strip_text_chords(nodes: &mut [MenuNode]) {
    for n in nodes {
        match n {
            MenuNode::Item(it) if it.menu_chord().is_some_and(|c| text_editing_chord(&c)) => it.accelerator = None,
            MenuNode::Submenu(s) => strip_text_chords(&mut s.children),
            _ => {}
        }
    }
}

fn spec(infos: &[CommandInfo]) -> MenuSpec {
    let mut spec = MenuSpec::from_commands(infos, crate::runtime::MENU_ORDER);
    for m in &mut spec.menus {
        strip_text_chords(&mut m.children);
    }
    #[cfg(target_os = "macos")]
    {
        // Quit belongs in the application menu, not File
        let mut quit = None;
        for m in &mut spec.menus {
            quit = quit.or_else(|| take_item(&mut m.children, "app.quit"));
        }
        spec.menus.retain(|m| !m.children.is_empty());
        let mut app_items = vec![
            MenuNode::role(Role::About),
            MenuNode::Separator,
            MenuNode::role(Role::Services),
            MenuNode::Separator,
            MenuNode::role(Role::Hide),
            MenuNode::role(Role::HideOthers),
            MenuNode::role(Role::ShowAll),
        ];
        if let Some(q) = quit {
            app_items.extend([MenuNode::Separator, q]);
        }
        // the first menu is the application menu on macOS
        spec.menus.insert(0, Submenu::new("App", app_items));
        let window = Submenu::new(
            "Window",
            vec![MenuNode::role(Role::Minimize), MenuNode::role(Role::Zoom), MenuNode::Separator, MenuNode::role(Role::BringAllToFront)],
        )
        .with_role(SubmenuRole::Window);
        // before Help, which stays last
        let at = spec.menus.iter().position(|m| m.role == Some(SubmenuRole::Help)).unwrap_or(spec.menus.len());
        spec.menus.insert(at, window);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = Submenu::new("", vec![MenuNode::role(Role::About)]);
    spec
}

/// The bar counts only once it shows in the window: a failed attach leaves the in-window bar.
fn attached<T>(bar: T, attach: ashlar_native::Result<()>) -> Option<T> {
    match attach {
        Ok(()) => Some(bar),
        Err(e) => {
            log::warn!("native menu bar could not be attached to the window, using the in-window one: {e}");
            None
        }
    }
}

impl NativeMenu {
    /// Install the bar and show it in `window` (main thread). `None` when the platform refuses or
    /// the bar can't be attached; the in-window bar stays.
    pub fn install(infos: &[CommandInfo], ctx: &egui::Context, window: &eframe::Frame) -> Option<NativeMenu> {
        let wake = ashlar_ui::wake::waker(ctx, ashlar_ui::wake::Reason::MENU);
        match NativeMenuBar::install(&spec(infos), std::sync::Arc::new(move || wake.wake())) {
            Ok(bar) => {
                let r = bar.attach_window(window);
                attached(NativeMenu { bar }, r)
            }
            Err(e) => {
                log::warn!("native menu bar unavailable, using the in-window one: {e}");
                None
            }
        }
    }

    pub fn update(&mut self, infos: &[CommandInfo]) {
        if let Err(e) = self.bar.update(&spec(infos)) {
            log::warn!("native menu bar: {e}");
        }
    }

    /// Command ids chosen since the last call.
    pub fn drain(&self) -> Vec<String> {
        self.bar.drain()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ashlar_native::NativeError;

    fn info(id: &str, menu: &str, shortcut: &str) -> CommandInfo {
        CommandInfo {
            id: id.to_owned(),
            label: id.to_owned(),
            menu: vec![menu.to_owned()],
            shortcut: Some(shortcut.to_owned()),
            params_schema: serde_json::Value::Null,
            params_text: None,
            enabled: true,
            disabled_reason: None,
            mutates: false,
        }
    }

    fn infos() -> Vec<CommandInfo> {
        vec![
            info("file.save", "File", "Mod+S"),
            info("app.quit", "File", "Mod+Q"),
            info("app.undo", "Edit", "Mod+Z"),
            info("app.redo", "Edit", "Mod+Shift+Z / Mod+Y"),
        ]
    }

    #[test]
    fn inactive_when_attach_fails() {
        assert!(attached((), Err(NativeError::Unsupported)).is_none());
        assert!(attached((), Err(NativeError::Os("no window".into()))).is_none());
        assert_eq!(attached(7, Ok(())), Some(7));
    }

    #[test]
    fn text_editing_shortcuts_stay_with_the_window() {
        let s = spec(&infos());
        assert_eq!(s.find("app.undo").map(|i| i.accelerator.clone()), Some(None), "⌘Z is the text field's");
        assert_eq!(s.find("app.redo").map(|i| i.accelerator.clone()), Some(None));
        assert!(s.find("file.save").and_then(|i| i.menu_chord()).is_some(), "⌘S stays a key equivalent");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_quit_is_in_the_app_menu_and_there_is_a_window_menu() {
        let s = spec(&infos());
        let app = &s.menus[0];
        assert!(app.children.iter().any(|n| matches!(n, MenuNode::Item(i) if i.id == "app.quit")), "{app:?}");
        let file = s.menus.iter().find(|m| m.label == "File").unwrap();
        assert!(!file.children.iter().any(|n| matches!(n, MenuNode::Item(i) if i.id == "app.quit")));
        let window = s.menus.iter().find(|m| m.role == Some(SubmenuRole::Window)).unwrap();
        for role in [Role::Minimize, Role::Zoom, Role::BringAllToFront] {
            assert!(window.children.iter().any(|n| matches!(n, MenuNode::Role { role: r, .. } if *r == role)), "{role:?}");
        }
        assert!(s.validate().is_ok(), "{:?}", s.validate());
    }

    #[test]
    fn take_item_finds_nested_items() {
        let mut s = spec(&infos());
        let found = s.menus.iter_mut().find_map(|m| take_item(&mut m.children, "file.save"));
        assert!(matches!(found, Some(MenuNode::Item(i)) if i.id == "file.save"));
        assert!(s.find("file.save").is_none());
    }
}
