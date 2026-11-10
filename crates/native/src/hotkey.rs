//! System-wide shortcuts: a chord that reaches the app while another app has the focus ("show the
//! quick-capture window"). Chords are [`ashlar_command`] chords (`Mod+Shift+Space`), the same text
//! the keymap and menus use; a chord without Ctrl, Alt or ⌘ (`F5`, `Shift+A`) is refused, since
//! taking a plain key from every other app is never what the user wants.
//!
//! macOS and Windows (create it on the thread that runs the event loop, the main thread for an
//! ashlar app); Linux on X11 only. Wayland has no global shortcuts for clients: there,
//! [`GlobalShortcuts::new`] may succeed but no chord ever fires.

use ashlar_command::shortcut::Chord;

use crate::{NativeError, Result};

/// Most shortcuts one app may hold.
pub const MAX_SHORTCUTS: usize = 64;
/// Longest shortcut id, in characters.
pub const MAX_ID: usize = 256;

/// Whether `chord` can be a global shortcut: it needs Ctrl, Alt or ⌘ and a key with a physical
/// code. Returns the key's `keyboard-types` code name.
pub fn check(chord: &Chord) -> Result<String> {
    let m = chord.mods;
    if !(m.command || m.ctrl || m.alt) {
        return Err(NativeError::Invalid(format!("`{}` has no Ctrl, Alt or ⌘: a global shortcut needs one", chord.portable())));
    }
    crate::key_code::code_name(chord).ok_or_else(|| NativeError::Invalid(format!("`{}` names a key a global shortcut can't use", chord.portable())))
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::collections::HashMap;
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::sync::{Mutex, Once, PoisonError};

    use super::*;
    use crate::Waker;

    /// The backend's modifiers and key code name for `chord`, or why it can't be global.
    pub(super) fn backend(chord: &Chord) -> Result<(global_hotkey::hotkey::Modifiers, String)> {
        use global_hotkey::hotkey::Modifiers as M;
        let m = chord.mods;
        let code = check(chord)?;
        let command = if cfg!(target_os = "macos") { M::META } else { M::CONTROL };
        let mut mods = M::empty();
        for (on, flag) in [(m.command, command), (m.ctrl, M::CONTROL), (m.alt, M::ALT), (m.shift, M::SHIFT)] {
            if on {
                mods |= flag;
            }
        }
        Ok((mods, code))
    }

    pub(super) fn hotkey(chord: &Chord) -> Result<global_hotkey::hotkey::HotKey> {
        let (mods, code) = backend(chord)?;
        let code: global_hotkey::hotkey::Code =
            code.parse().map_err(|_| NativeError::Invalid(format!("`{}` names a key a global shortcut can't use", chord.portable())))?;
        Ok(global_hotkey::hotkey::HotKey::new(Some(mods), code))
    }

    /// Presses, by the backend's hotkey number: the shortcut's id and where to send it.
    type Sinks = HashMap<u32, (String, Sender<String>, Waker)>;
    static SINKS: Mutex<Option<Sinks>> = Mutex::new(None);
    static INSTALL: Once = Once::new();

    fn on_event(e: global_hotkey::GlobalHotKeyEvent) {
        if e.state() != global_hotkey::HotKeyState::Pressed {
            return;
        }
        let sink = SINKS.lock().unwrap_or_else(PoisonError::into_inner).as_ref().and_then(|m| m.get(&e.id()).cloned());
        if let Some((id, tx, wake)) = sink
            && tx.send(id).is_ok()
        {
            wake();
        }
    }

    fn os(e: impl std::fmt::Display) -> NativeError {
        NativeError::Os(crate::os_text(e))
    }

    pub struct GlobalShortcuts {
        manager: global_hotkey::GlobalHotKeyManager,
        held: Vec<(String, global_hotkey::hotkey::HotKey)>,
        tx: Sender<String>,
        rx: Receiver<String>,
        waker: Waker,
    }

    impl GlobalShortcuts {
        /// Ready to register chords; presses are queued for [`Self::drain`] and `waker` is called.
        pub fn new(waker: Waker) -> Result<Self> {
            #[cfg(target_os = "macos")]
            if objc2::MainThreadMarker::new().is_none() {
                return Err(NativeError::Os("global shortcuts must be set up on the main thread".into()));
            }
            INSTALL.call_once(|| {
                global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(|e: global_hotkey::GlobalHotKeyEvent| {
                    // runs inside the OS's callback: never unwind into it
                    let _ = std::panic::catch_unwind(|| on_event(e));
                }));
            });
            let manager = crate::guarded("global shortcuts", || global_hotkey::GlobalHotKeyManager::new().map_err(os))?;
            let (tx, rx) = channel();
            Ok(Self { manager, held: vec![], tx, rx, waker })
        }

        /// Hold `chord` system-wide; a press is reported as `id`. Registering an id again moves it
        /// to the new chord. Fails when the chord has no Ctrl/Alt/⌘, when another app (or this one,
        /// under another id) holds it, or past [`MAX_SHORTCUTS`].
        pub fn register(&mut self, id: impl Into<String>, chord: &Chord) -> Result<()> {
            let id = id.into();
            if id.is_empty() || id.chars().count() > MAX_ID {
                return Err(NativeError::Invalid(format!("a shortcut id is 1–{MAX_ID} characters")));
            }
            let key = hotkey(chord)?;
            if let Some((other, _)) = self.held.iter().find(|(i, k)| k.id() == key.id() && *i != id) {
                return Err(NativeError::Invalid(format!("`{}` is already the shortcut for `{other}`", chord.portable())));
            }
            if self.held.iter().any(|(i, k)| *i == id && k.id() == key.id()) {
                return Ok(());
            }
            if !self.held.iter().any(|(i, _)| *i == id) && self.held.len() >= MAX_SHORTCUTS {
                return Err(NativeError::Invalid(format!("at most {MAX_SHORTCUTS} global shortcuts")));
            }
            crate::guarded("a global shortcut", || self.manager.register(key).map_err(|e| NativeError::Denied(crate::os_text(e))))?;
            let _ = self.unregister(&id);
            SINKS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert_with(HashMap::new)
                .insert(key.id(), (id.clone(), self.tx.clone(), self.waker.clone()));
            self.held.push((id, key));
            Ok(())
        }

        /// Release the chord registered as `id`; `false` if there was none.
        pub fn unregister(&mut self, id: &str) -> bool {
            let Some(i) = self.held.iter().position(|(h, _)| h == id) else { return false };
            let (_, key) = self.held.remove(i);
            if let Some(m) = SINKS.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
                m.remove(&key.id());
            }
            let _ = crate::guarded("a global shortcut", || self.manager.unregister(key).map_err(os));
            true
        }

        /// The ids registered now, in order.
        pub fn ids(&self) -> Vec<&str> {
            self.held.iter().map(|(i, _)| i.as_str()).collect()
        }

        /// Shortcut ids pressed since the last call, oldest first.
        pub fn drain(&self) -> Vec<String> {
            self.rx.try_iter().collect()
        }
    }

    impl Drop for GlobalShortcuts {
        fn drop(&mut self) {
            let ids: Vec<String> = self.held.iter().map(|(i, _)| i.clone()).collect();
            for id in ids {
                self.unregister(&id);
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::*;

    pub struct GlobalShortcuts(());

    impl GlobalShortcuts {
        pub fn new(_waker: crate::Waker) -> Result<Self> {
            Err(NativeError::Unsupported)
        }
        pub fn register(&mut self, _id: impl Into<String>, chord: &Chord) -> Result<()> {
            check(chord)?;
            Err(NativeError::Unsupported)
        }
        pub fn unregister(&mut self, _id: &str) -> bool {
            false
        }
        pub fn ids(&self) -> Vec<&str> {
            vec![]
        }
        pub fn drain(&self) -> Vec<String> {
            vec![]
        }
    }
}

/// The app's global shortcuts. Not `Send`: create, change and drop it on the event-loop thread.
/// Dropping it releases every chord it holds.
pub use imp::GlobalShortcuts;

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        Chord::parse(text).unwrap_or_else(|e| panic!("{text}: {e:?}"))
    }

    #[test]
    fn unmodified_and_shift_only_chords_are_refused() {
        for text in ["F5", "A", "Shift+A", "Shift+F1", "Space"] {
            assert!(matches!(check(&chord(text)), Err(NativeError::Invalid(_))), "{text}");
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn chords_map_onto_the_platforms_modifiers() {
        use global_hotkey::hotkey::Modifiers as M;
        let command = if cfg!(target_os = "macos") { M::META } else { M::CONTROL };
        assert_eq!(super::imp::backend(&chord("Mod+Shift+K")).unwrap(), (command | M::SHIFT, "KeyK".to_string()));
        assert_eq!(super::imp::backend(&chord("Ctrl+Alt+F5")).unwrap(), (M::CONTROL | M::ALT, "F5".to_string()));
        assert_eq!(check(&chord("Alt+Space")).unwrap(), "Space");
        assert!(check(&chord("Mod+é")).is_err());
        assert!(super::imp::hotkey(&chord("Mod+Shift+Space")).is_ok());
    }

    #[cfg(not(any(target_os = "macos", target_arch = "wasm32")))]
    #[test]
    fn setting_up_never_panics() {
        // macOS needs the main thread (tests/main_thread.rs); elsewhere this may succeed or fail
        // with an error, depending on the desktop.
        if let Ok(mut g) = GlobalShortcuts::new(crate::no_waker()) {
            assert!(g.register("x", &chord("Shift+A")).is_err());
            assert!(!g.unregister("missing"));
        }
    }
}
