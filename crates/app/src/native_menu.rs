//! The app's commands in the OS menu bar (feature `native-menu`, macOS and Windows): the same
//! command list the in-window bar is built from, mirrored into `ashlar_native::menu`.

use ashlar_command::CommandInfo;
use ashlar_native::menu::{MenuNode, MenuSpec, NativeMenuBar, Role, Submenu};

pub(crate) struct NativeMenu {
    bar: NativeMenuBar,
}

fn spec(infos: &[CommandInfo]) -> MenuSpec {
    let mut spec = MenuSpec::from_commands(infos, crate::runtime::MENU_ORDER);
    #[cfg(target_os = "macos")]
    {
        // the first menu is the application menu on macOS
        let app = Submenu::new(
            "App",
            vec![
                MenuNode::role(Role::About),
                MenuNode::Separator,
                MenuNode::role(Role::Services),
                MenuNode::Separator,
                MenuNode::role(Role::Hide),
                MenuNode::role(Role::HideOthers),
                MenuNode::role(Role::ShowAll),
            ],
        );
        spec.menus.insert(0, app);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = Submenu::new("", vec![MenuNode::role(Role::About)]);
    spec
}

impl NativeMenu {
    /// Install the bar (main thread). `None` when the platform refuses; the in-window bar stays.
    pub fn install(infos: &[CommandInfo], ctx: &egui::Context) -> Option<NativeMenu> {
        let wake = ctx.clone();
        match NativeMenuBar::install(&spec(infos), std::sync::Arc::new(move || wake.request_repaint())) {
            Ok(bar) => Some(NativeMenu { bar }),
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
