//! A hook around every call that blocks the calling thread in the OS until the user answers (the
//! native file dialogs and message boxes). A frame-loop hang watchdog pauses itself here: a dialog
//! left open for a minute is not a hung app. Always compiled, so code without the `dialogs`
//! feature can install the hook too.

use std::sync::{Arc, PoisonError, RwLock};

/// Called with `true` just before a blocking native call and `false` just after it (also when the
/// call failed or panicked). Must be cheap and must not block.
pub type ModalHook = Arc<dyn Fn(bool) + Send + Sync>;

static HOOK: RwLock<Option<ModalHook>> = RwLock::new(None);

/// Install the process-wide hook (`None` removes it). Replaces any earlier one.
pub fn set_hook(hook: Option<ModalHook>) {
    *HOOK.write().unwrap_or_else(PoisonError::into_inner) = hook;
}

/// Remove `hook` if it is still the installed one (another may have replaced it since).
pub fn clear_if(hook: &ModalHook) {
    let mut slot = HOOK.write().unwrap_or_else(PoisonError::into_inner);
    if slot.as_ref().is_some_and(|h| Arc::ptr_eq(h, hook)) {
        *slot = None;
    }
}

/// Run `f`, a blocking native call, between the hook's `true` and `false`.
pub fn around<T>(f: impl FnOnce() -> T) -> T {
    let hook = HOOK.read().unwrap_or_else(PoisonError::into_inner).clone();
    let _end = Ended(hook.clone());
    if let Some(h) = &hook {
        h(true);
    }
    f()
}

/// Calls the hook's `false` when dropped, so a panic inside the call still ends the pause.
struct Ended(Option<ModalHook>);

impl Drop for Ended {
    fn drop(&mut self) {
        if let Some(h) = &self.0 {
            h(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[test]
    fn the_hook_brackets_the_call_even_when_it_panics() {
        let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
        let sink = seen.clone();
        set_hook(Some(Arc::new(move |on| sink.lock().unwrap().push(on))));
        assert_eq!(around(|| 7), 7);
        let r = std::panic::catch_unwind(|| around(|| -> u8 { panic!("inside the dialog") }));
        assert!(r.is_err());
        set_hook(None);
        assert_eq!(around(|| 1), 1);
        assert_eq!(*seen.lock().unwrap(), [true, false, true, false]);
        // clear_if removes only the hook it is given
        let a: ModalHook = Arc::new(|_| {});
        let b: ModalHook = Arc::new(|_| {});
        set_hook(Some(a.clone()));
        clear_if(&b);
        assert!(HOOK.read().unwrap().is_some());
        clear_if(&a);
        assert!(HOOK.read().unwrap().is_none());
    }
}
