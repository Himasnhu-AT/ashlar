//! A tray / status-bar icon with a menu: the macOS status bar, the Windows notification area and,
//! on Linux, a StatusNotifierItem over D-Bus (no GTK). The menu is a [`MenuSpec`], mapped as a
//! popup (one top-level menu shows its children, several show as submenus). Clicks on the icon
//! and chosen items arrive through [`Tray::events`], after `waker` is called. Dropping the
//! [`Tray`] removes the icon.
//!
//! Create, change and drop it on the main thread once the event loop runs (macOS refuses
//! otherwise, with an error rather than a crash).

use ashlar_command::menu::MenuSpec;

use crate::{NativeError, Result, Waker};

/// Longest tooltip kept, in characters.
pub const MAX_TOOLTIP: usize = 256;
/// Largest icon side accepted, in pixels.
pub const MAX_ICON_SIDE: u32 = 1024;
/// Longest tray id, in characters.
pub const MAX_ID: usize = 64;

/// A straight-alpha RGBA icon, row by row from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Icon {
    /// The size and buffer length agree and the sides are 1..=[`MAX_ICON_SIDE`].
    pub fn validate(&self) -> Result<()> {
        let side_ok = |s: u32| (1..=MAX_ICON_SIDE).contains(&s);
        if !side_ok(self.width) || !side_ok(self.height) {
            return Err(NativeError::Invalid(format!("a tray icon is 1–{MAX_ICON_SIDE} pixels a side")));
        }
        let want = (self.width as usize).saturating_mul(self.height as usize).saturating_mul(4);
        if self.rgba.len() != want {
            return Err(NativeError::Invalid(format!("a {}×{} icon needs {want} bytes, not {}", self.width, self.height, self.rgba.len())));
        }
        Ok(())
    }
}

/// What to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraySpec {
    /// Names the icon to the OS (Linux) and keeps menus of several trays apart. 1–[`MAX_ID`]
    /// characters, no control characters.
    pub id: String,
    pub tooltip: String,
    pub icon: Icon,
    /// macOS: draw the icon as a template (its alpha only, tinted for the menu bar).
    pub template: bool,
    pub menu: MenuSpec,
}

impl TraySpec {
    pub fn new(id: impl Into<String>, tooltip: impl Into<String>, icon: Icon, menu: MenuSpec) -> Self {
        Self { id: id.into(), tooltip: tooltip.into(), icon, template: false, menu }
    }

    pub fn template(mut self, on: bool) -> Self {
        self.template = on;
        self
    }

    pub fn validate(&self) -> Result<()> {
        let n = self.id.chars().count();
        if n == 0 || n > MAX_ID || self.id.chars().any(char::is_control) {
            return Err(NativeError::Invalid(format!("a tray id is 1–{MAX_ID} characters without control characters")));
        }
        self.icon.validate()?;
        self.menu.validate().map_err(|p| NativeError::Invalid(p.join("; ")))
    }
}

/// Something the user did with the tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEvent {
    /// A left click (release) or a double click on the icon.
    Activate,
    /// A menu item was chosen: its id in the spec.
    Menu(String),
}

/// The menu-event prefix of tray `id`'s items.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn prefix(id: &str) -> String {
    format!("\u{1}tray/{id}/")
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn clip_tooltip(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(MAX_TOOLTIP).collect()
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::collections::HashMap;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Mutex, Once, PoisonError};

    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    use super::*;
    use crate::menu_events::{Subscription, subscribe};

    /// Icon clicks, by tray id. tray-icon has one process-wide click handler, like the menus.
    static CLICKS: Mutex<Option<HashMap<String, (Sender<()>, Waker)>>> = Mutex::new(None);
    static INSTALL: Once = Once::new();

    fn on_click(e: TrayIconEvent) {
        let id = match &e {
            TrayIconEvent::Click { id, button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
            | TrayIconEvent::DoubleClick { id, .. } => id.0.clone(),
            _ => return,
        };
        let sink = CLICKS.lock().unwrap_or_else(PoisonError::into_inner).as_ref().and_then(|m| m.get(&id).cloned());
        if let Some((tx, wake)) = sink
            && tx.send(()).is_ok()
        {
            wake();
        }
    }

    pub struct Tray {
        icon: tray_icon::TrayIcon,
        id: String,
        #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
        template: bool,
        menu_rx: Receiver<String>,
        click_rx: Receiver<()>,
        _route: Subscription,
    }

    fn os(e: impl std::fmt::Display) -> NativeError {
        NativeError::Os(crate::os_text(e))
    }

    fn icon(i: &Icon) -> Result<tray_icon::Icon> {
        tray_icon::Icon::from_rgba(i.rgba.clone(), i.width, i.height).map_err(os)
    }

    impl Tray {
        pub fn new(spec: TraySpec, waker: Waker) -> Result<Self> {
            spec.validate()?;
            #[cfg(target_os = "macos")]
            if objc2::MainThreadMarker::new().is_none() {
                return Err(NativeError::Os("the tray must be created on the main thread".into()));
            }
            INSTALL.call_once(|| {
                TrayIconEvent::set_event_handler(Some(|e: TrayIconEvent| {
                    // runs inside the OS's callback: never unwind into it
                    let _ = std::panic::catch_unwind(|| on_click(e));
                }));
            });
            let (menu_tx, menu_rx) = channel();
            let (click_tx, click_rx) = channel();
            {
                let mut clicks = CLICKS.lock().unwrap_or_else(PoisonError::into_inner);
                let map = clicks.get_or_insert_with(HashMap::new);
                if map.contains_key(&spec.id) {
                    return Err(NativeError::Invalid(format!("a tray with id `{}` already exists", spec.id)));
                }
                map.insert(spec.id.clone(), (click_tx, waker.clone()));
            }
            let route = subscribe(prefix(&spec.id), menu_tx, waker);
            let built = crate::guarded("the tray icon", || {
                let menu = crate::menu_tree::build(&crate::menu_tree::plan(&spec.menu, &prefix(&spec.id)))?;
                let b = TrayIconBuilder::new().with_id(spec.id.clone()).with_tooltip(clip_tooltip(&spec.tooltip)).with_menu(Box::new(menu));
                #[cfg(target_os = "macos")]
                let b = if spec.template { b.with_icon_templated(icon(&spec.icon)?) } else { b.with_icon(icon(&spec.icon)?) };
                #[cfg(not(target_os = "macos"))]
                let b = b.with_icon(icon(&spec.icon)?);
                b.build().map_err(os)
            });
            match built {
                Ok(icon) => Ok(Self { icon, id: spec.id, template: spec.template, menu_rx, click_rx, _route: route }),
                Err(e) => {
                    forget_clicks(&spec.id);
                    Err(e)
                }
            }
        }

        pub fn events(&self) -> Vec<TrayEvent> {
            let mut out: Vec<TrayEvent> = self.click_rx.try_iter().map(|()| TrayEvent::Activate).collect();
            out.extend(self.menu_rx.try_iter().map(TrayEvent::Menu));
            out
        }

        pub fn set_tooltip(&self, text: &str) -> Result<()> {
            crate::guarded("the tray tooltip", || self.icon.set_tooltip(Some(clip_tooltip(text))).map_err(os))
        }

        pub fn set_icon(&self, i: &Icon) -> Result<()> {
            i.validate()?;
            crate::guarded("the tray icon", || {
                let i = Some(icon(i)?);
                #[cfg(target_os = "macos")]
                let set = if self.template { self.icon.set_icon_templated(i) } else { self.icon.set_icon(i) };
                #[cfg(not(target_os = "macos"))]
                let set = self.icon.set_icon(i);
                set.map_err(os)
            })
        }

        pub fn set_menu(&self, spec: &MenuSpec) -> Result<()> {
            spec.validate().map_err(|p| NativeError::Invalid(p.join("; ")))?;
            crate::guarded("the tray menu", || {
                let menu = crate::menu_tree::build(&crate::menu_tree::plan(spec, &prefix(&self.id)))?;
                self.icon.set_menu(Some(Box::new(menu)));
                Ok(())
            })
        }
    }

    fn forget_clicks(id: &str) {
        if let Some(m) = CLICKS.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            m.remove(id);
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            forget_clicks(&self.id);
            // the icon itself goes away when `self.icon` drops
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::*;

    pub struct Tray(());

    impl Tray {
        pub fn new(spec: TraySpec, _waker: Waker) -> Result<Self> {
            spec.validate()?;
            Err(NativeError::Unsupported)
        }
        pub fn events(&self) -> Vec<TrayEvent> {
            vec![]
        }
        pub fn set_tooltip(&self, _text: &str) -> Result<()> {
            Err(NativeError::Unsupported)
        }
        pub fn set_icon(&self, _i: &Icon) -> Result<()> {
            Err(NativeError::Unsupported)
        }
        pub fn set_menu(&self, _spec: &MenuSpec) -> Result<()> {
            Err(NativeError::Unsupported)
        }
    }
}

/// A live tray icon. Not `Send`: create, change and drop it on the main thread.
pub use imp::Tray;

#[cfg(test)]
mod tests {
    use ashlar_command::menu::{Item, MenuNode, Submenu};

    use super::*;

    fn icon(w: u32, h: u32) -> Icon {
        Icon { rgba: vec![0; (w * h * 4) as usize], width: w, height: h }
    }

    fn spec() -> TraySpec {
        TraySpec::new("main", "My app", icon(16, 16), MenuSpec::new(vec![Submenu::new("Tray", vec![MenuNode::Item(Item::new("show", "Show"))])]))
    }

    #[test]
    fn specs_are_checked_before_anything_reaches_the_os() {
        assert!(spec().validate().is_ok());
        let mut s = spec();
        s.id = String::new();
        assert!(matches!(s.validate(), Err(NativeError::Invalid(_))));
        s.id = "a\nb".into();
        assert!(s.validate().is_err());
        let mut s = spec();
        s.icon.rgba.pop();
        assert!(s.validate().is_err());
        s.icon = icon(0, 16);
        assert!(s.validate().is_err());
        s.icon = Icon { rgba: vec![], width: MAX_ICON_SIDE + 1, height: 1 };
        assert!(s.validate().is_err());
        let mut s = spec();
        s.menu.menus[0].children.push(MenuNode::Item(Item::new("show", "Again")));
        assert!(s.validate().is_err(), "duplicate ids are refused");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn tray_items_carry_the_tray_prefix_so_events_route_back() {
        let p = crate::menu_tree::plan(&spec().menu, &prefix("main"));
        assert!(matches!(&p[..], [crate::menu_tree::Entry::Item { id, .. }] if id == "\u{1}tray/main/show"));
        assert_eq!(crate::menu_events::owner(["", &prefix("main"), &prefix("other")], "\u{1}tray/main/show"), Some(1));
    }

    #[test]
    fn tooltips_lose_control_characters_and_length() {
        assert_eq!(clip_tooltip("a\nb"), "ab");
        assert_eq!(clip_tooltip(&"x".repeat(MAX_TOOLTIP + 9)).chars().count(), MAX_TOOLTIP);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn creating_a_tray_never_panics() {
        // macOS needs the main thread (tests/main_thread.rs); elsewhere this may succeed or fail
        // with an error, depending on the desktop.
        let _ = Tray::new(spec(), crate::no_waker());
    }
}
