//! Agent control of the window. `ui.*` input, the element tree, waits and screenshots are the
//! debugger's ([`ashlar_ui::devtools`]); the runtime answers the rest (`ui.inspect`, menus,
//! `ui.set`, `app.*`) with access to the whole app.

use ashlar_control::{Request, UI_METHODS, UiHandler};
use ashlar_ui::devtools;

/// Extra methods the windowed runtime answers itself.
pub const RUNTIME_METHODS: &[&str] = &["app.stats", "app.logs"];

/// Collects requests while `ashlar_control::handle` runs; the runtime answers them right after
/// with access to the whole app.
#[derive(Default)]
pub(crate) struct UiQueue {
    pub pending: Vec<Request>,
}

impl UiHandler for UiQueue {
    fn methods(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in UI_METHODS.iter().chain(devtools::METHODS).chain(RUNTIME_METHODS) {
            if !out.iter().any(|o| o == m) {
                out.push((*m).to_string());
            }
        }
        out
    }
    fn handle(&mut self, req: Request) {
        self.pending.push(req);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn methods_are_listed_once() {
        let m = UiQueue::default().methods();
        for name in ["ui.inspect", "ui.snapshot", "ui.click", "ui.wait", "devtools.toggle", "app.stats"] {
            assert_eq!(m.iter().filter(|x| *x == name).count(), 1, "{name} in {m:?}");
        }
    }
}
