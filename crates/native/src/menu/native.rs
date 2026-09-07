//! The `muda` backend of [`super::MenuSpec`]: macOS (the app's menu bar) and Windows (a window's
//! menu bar).

use std::cell::Cell;
use std::sync::mpsc::{Receiver, Sender, channel};

use muda::accelerator::Accelerator as MudaAccelerator;
use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};

use super::{Item, MenuNode, MenuSpec, Role, SubmenuRole, Update};
use crate::{NativeError, Result, Waker};

/// A native item we can patch later, by its path in the spec.
enum Handle {
    Item(MenuItem),
    Check(CheckMenuItem),
    Submenu(muda::Submenu),
}

/// The live native menu bar. Not `Send`: create, update and drop it on the main thread.
pub struct NativeMenuBar {
    menu: Menu,
    structure: u64,
    /// The state last pushed to the OS; reset by an activation, because the OS flips a check
    /// item's mark itself when it is chosen and the app may not follow (the command failed).
    state: Cell<u64>,
    handles: Vec<(Vec<usize>, Handle)>,
    events: Receiver<String>,
}

fn muda_accel(item: &Item) -> Option<MudaAccelerator> {
    item.menu_chord().as_ref().and_then(super::to_muda)
}

fn os(e: impl std::fmt::Display) -> NativeError {
    NativeError::Os(crate::os_text(e))
}

#[cfg(target_os = "macos")]
fn require_main_thread() -> Result<()> {
    objc2::MainThreadMarker::new().map(|_| ()).ok_or_else(|| NativeError::Os("the menu bar must be created and changed on the main thread".into()))
}

#[cfg(not(target_os = "macos"))]
fn require_main_thread() -> Result<()> {
    Ok(())
}

fn predefined(role: Role, label: Option<&str>) -> Option<PredefinedMenuItem> {
    let l = label;
    Some(match role {
        Role::About => PredefinedMenuItem::about(l, None),
        Role::Quit => PredefinedMenuItem::quit(l),
        Role::CloseWindow => PredefinedMenuItem::close_window(l),
        Role::Minimize => PredefinedMenuItem::minimize(l),
        Role::Undo => PredefinedMenuItem::undo(l),
        Role::Redo => PredefinedMenuItem::redo(l),
        Role::Cut => PredefinedMenuItem::cut(l),
        Role::Copy => PredefinedMenuItem::copy(l),
        Role::Paste => PredefinedMenuItem::paste(l),
        Role::SelectAll => PredefinedMenuItem::select_all(l),
        #[cfg(target_os = "macos")]
        Role::Services => PredefinedMenuItem::services(l),
        #[cfg(target_os = "macos")]
        Role::Hide => PredefinedMenuItem::hide(l),
        #[cfg(target_os = "macos")]
        Role::HideOthers => PredefinedMenuItem::hide_others(l),
        #[cfg(target_os = "macos")]
        Role::ShowAll => PredefinedMenuItem::show_all(l),
        #[cfg(target_os = "macos")]
        Role::Zoom => PredefinedMenuItem::zoom(l),
        #[cfg(target_os = "macos")]
        Role::Fullscreen => PredefinedMenuItem::fullscreen(l),
        #[cfg(target_os = "macos")]
        Role::BringAllToFront => PredefinedMenuItem::bring_all_to_front(l),
        // Not a thing on this platform: left out.
        #[cfg(not(target_os = "macos"))]
        Role::Services | Role::Hide | Role::HideOthers | Role::ShowAll | Role::Zoom | Role::Fullscreen | Role::BringAllToFront => return None,
    })
}

/// Append `nodes` under `parent`, recording patchable handles by path.
fn build_children(parent: &muda::Submenu, nodes: &[MenuNode], path: &mut Vec<usize>, handles: &mut Vec<(Vec<usize>, Handle)>) -> Result<()> {
    if path.len() > super::MAX_DEPTH {
        return Ok(());
    }
    for (i, n) in nodes.iter().enumerate() {
        path.push(i);
        match n {
            MenuNode::Item(it) => {
                let accel = muda_accel(it);
                if let Some(checked) = it.checked {
                    let h = CheckMenuItem::with_id(it.id.clone(), &it.label, it.enabled, checked, accel);
                    parent.append(&h).map_err(os)?;
                    handles.push((path.clone(), Handle::Check(h)));
                } else {
                    let h = MenuItem::with_id(it.id.clone(), &it.label, it.enabled, accel);
                    parent.append(&h).map_err(os)?;
                    handles.push((path.clone(), Handle::Item(h)));
                }
            }
            MenuNode::Separator => parent.append(&PredefinedMenuItem::separator()).map_err(os)?,
            MenuNode::Role { role, label } => {
                if let Some(p) = predefined(*role, label.as_deref()) {
                    parent.append(&p).map_err(os)?;
                }
            }
            MenuNode::Submenu(s) => {
                let sub = muda::Submenu::new(&s.label, s.enabled);
                build_children(&sub, &s.children, path, handles)?;
                parent.append(&sub).map_err(os)?;
                set_role(&sub, s.role);
                handles.push((path.clone(), Handle::Submenu(sub)));
            }
        }
        path.pop();
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_role(sub: &muda::Submenu, role: Option<SubmenuRole>) {
    match role {
        Some(SubmenuRole::Window) => sub.set_as_windows_menu_for_nsapp(),
        Some(SubmenuRole::Help) => sub.set_as_help_menu_for_nsapp(),
        None => {}
    }
}

#[cfg(not(target_os = "macos"))]
fn set_role(_: &muda::Submenu, _: Option<SubmenuRole>) {}

impl NativeMenuBar {
    /// Build the menu bar from `spec` and show it (macOS: as the app's menu bar; Windows: call
    /// [`Self::attach_window`] for each window that shows it). Activations are queued for
    /// [`Self::try_recv`] and `waker` is called. Main thread only. Replaces the menu-event
    /// handler of any earlier bar.
    pub fn install(spec: &MenuSpec, waker: Waker) -> Result<Self> {
        require_main_thread()?;
        crate::guarded("the menu bar", || {
            let (tx, rx): (Sender<String>, Receiver<String>) = channel();
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                if tx.send(e.id().0.clone()).is_ok() {
                    waker();
                }
            }));
            let mut bar = Self { menu: Menu::new(), structure: 0, state: Cell::new(0), handles: vec![], events: rx };
            bar.rebuild(spec)?;
            #[cfg(target_os = "macos")]
            bar.menu.init_for_nsapp();
            Ok(bar)
        })
    }

    /// Show the bar in a window (Windows). A no-op on macOS, whose menu bar is app-wide.
    pub fn attach_window(&self, window: &impl raw_window_handle::HasWindowHandle) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            use raw_window_handle::RawWindowHandle;
            let handle = window.window_handle().map_err(os)?;
            let RawWindowHandle::Win32(h) = handle.as_raw() else { return Err(NativeError::Unsupported) };
            let hwnd = h.hwnd.get();
            #[allow(unsafe_code)]
            // SAFETY: `hwnd` comes from a live window handle borrowed for this call, so it names a
            // valid window owned by this thread's event loop. muda subclasses that window to show the
            // menu and drops the subclass when the window is destroyed.
            let r = unsafe { self.menu.init_for_hwnd(hwnd) };
            r.map_err(os)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = window;
            Ok(())
        }
    }

    /// Bring the native bar up to date with `spec`: rebuild on a structure change, patch labels /
    /// enabled / checked on a state change, otherwise do nothing. Call every frame; it is a
    /// couple of hashes when nothing changed.
    pub fn update(&mut self, spec: &MenuSpec) -> Result<Update> {
        let (structure, state) = (spec.structure_hash(), spec.state_hash());
        if structure != self.structure {
            require_main_thread()?;
            crate::guarded("the menu bar", || self.rebuild(spec))?;
            return Ok(Update::Rebuilt);
        }
        if state == self.state.get() {
            return Ok(Update::Unchanged);
        }
        require_main_thread()?;
        crate::guarded("the menu bar", || {
            self.patch(spec);
            Ok(())
        })?;
        self.state.set(state);
        Ok(Update::Patched)
    }

    /// The next activated item id, if any.
    pub fn try_recv(&self) -> Option<String> {
        let id = self.events.try_recv().ok();
        if id.is_some() {
            self.state.set(0);
        }
        id
    }

    /// Every activated item id since the last call, oldest first. After an activation the next
    /// [`Self::update`] pushes the state again, so a check mark the OS flipped shows the app's
    /// state, not the click.
    pub fn drain(&self) -> Vec<String> {
        let ids: Vec<String> = self.events.try_iter().take(1024).collect();
        if !ids.is_empty() {
            self.state.set(0);
        }
        ids
    }

    fn rebuild(&mut self, spec: &MenuSpec) -> Result<()> {
        while self.menu.remove_at(0).is_some() {}
        self.handles.clear();
        for (i, m) in spec.menus.iter().enumerate() {
            let sub = muda::Submenu::new(&m.label, m.enabled);
            let mut path = vec![i];
            build_children(&sub, &m.children, &mut path, &mut self.handles)?;
            self.menu.append(&sub).map_err(os)?;
            set_role(&sub, m.role);
            self.handles.push((vec![i], Handle::Submenu(sub)));
        }
        self.structure = spec.structure_hash();
        self.state.set(spec.state_hash());
        Ok(())
    }

    fn patch(&self, spec: &MenuSpec) {
        for (path, handle) in &self.handles {
            let node = if let [top] = path.as_slice() { spec.menus.get(*top).map(|m| (&m.label, m.enabled, None)) } else { None };
            let node = node.or_else(|| match spec.node(path)? {
                MenuNode::Item(i) => Some((&i.label, i.enabled, i.checked)),
                MenuNode::Submenu(s) => Some((&s.label, s.enabled, None)),
                _ => None,
            });
            let Some((label, enabled, checked)) = node else { continue };
            match handle {
                Handle::Item(h) => {
                    if h.text() != *label {
                        h.set_text(label);
                    }
                    h.set_enabled(enabled);
                }
                Handle::Check(h) => {
                    if h.text() != *label {
                        h.set_text(label);
                    }
                    h.set_enabled(enabled);
                    h.set_checked(checked.unwrap_or(false));
                }
                Handle::Submenu(h) => {
                    if h.text() != *label {
                        h.set_text(label);
                    }
                    h.set_enabled(enabled);
                }
            }
        }
    }
}
