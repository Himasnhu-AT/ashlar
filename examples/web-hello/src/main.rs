//! The smallest ashlar app for the web. `cargo xtask wasm` (part of the gate) checks that it
//! builds for `wasm32-unknown-unknown`.
//!
//! To run it in a browser: `trunk serve examples/web-hello/index.html` (trunk runs `wasm-bindgen`
//! and serves the page). The app runs in `<canvas id="web-hello">`; if it cannot start, a visible
//! error appears next to the canvas. On the desktop the same code opens a window.

use ashlar::prelude::*;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Doc {
    clicks: u32,
}

fn main() -> Result<(), ashlar::app::Error> {
    App::<Doc>::new("web-hello", env!("CARGO_PKG_VERSION"))
        .title("Hello, web")
        .canvas_id("web-hello")
        .view(|ui, cx: &mut Cx<'_, Doc>| {
            let t = cx.tokens();
            ui.heading("Hello from ashlar on the web");
            ui.add_space(t.space.md);
            if ui.button("Click").clicked() {
                cx.edit("Click", |d| d.clicks = d.clicks.saturating_add(1));
            }
            ui.label(format!("Clicked {} times (undo with Ctrl/Cmd+Z)", cx.doc().clicks));
        })
        .run()
}
