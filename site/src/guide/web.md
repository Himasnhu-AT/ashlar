# Web

ashlar's headless core (commands, documents, files, jobs, logging) builds for
`wasm32-unknown-unknown`, and CI checks it. Running the windowed app in a browser is experimental:
it works, and `examples/web-hello` builds in the gate, but not every feature is there.

## The example

```rust,ignore
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
```

The same code opens a window on the desktop. On the web the app runs in the `<canvas>` named by
`App::canvas_id` (default: the app's name, then `ashlar`). If it cannot start (no such canvas, no
WebGL), a visible error appears next to the canvas as well as in the console.

## Dependencies

Turn off the default features: `headless` has no meaning in a browser. Keep `accesskit` for screen
readers.

```toml
[dependencies]
ashlar = { package = "ashlar-rs", version = "0.2", default-features = false, features = ["accesskit"] }
serde = { version = "1", features = ["derive"] }
```

## Building and serving

The example uses [trunk](https://trunkrs.dev), which runs `wasm-bindgen` and serves the page:

```sh
rustup target add wasm32-unknown-unknown
trunk serve examples/web-hello/index.html
```

The page needs a canvas with the right id and a trunk link to the binary:

```html
<link data-trunk rel="rust" data-bin="ashlar-example-web-hello">
<canvas id="web-hello"></canvas>
```

## What differs on the web

- Jobs run inline, with no threads. Use `spawn_stepped` for long work so it advances a few steps
  per frame.
- The disk-backed settings and files compile but report errors; use the in-memory backends or a
  browser-storage backend (`ashlar::fs::ConfigStore`).
- File dialogs can only open files.
- The tray, global shortcuts, notifications, the clipboard, single instance, the control port and
  record and replay are not available.
