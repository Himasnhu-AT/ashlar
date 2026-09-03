# ashlar-app

The runtime of an ashlar app. An app declares its document type and format, its commands and a
view; `App::run()` opens the window and adds everything else.

```rust
App::<Notes>::new("notes", env!("CARGO_PKG_VERSION"))
    .format(Format::new("acme.notes", "notes 1.0").migrations(vec![v1_to_v2]))
    .file_extension("notes")
    .commands(commands())                     // CommandSpec<Session<Notes>>
    .view(|ui, cx: &mut Cx<'_, Notes>| { /* cx.doc(), cx.dispatch(id, params), cx.edit(..) */ })
    .run()?;                                  // the window
notes_app().cli().run();                      // the same commands as `notes-cli` (+ MCP)
let mut s = notes_app().headless();           // tests: a Dispatch without a window
let mut d = Driver::new(notes_app().runtime()); // tests: the window on a virtual clock
```

## What an app gets

| | |
|---|---|
| Undo | Every top-level command is exactly one undo step named after it: its edits go to a working copy committed with `Store::commit`. A failed or panicking command changes nothing (document, history, redo line, view state, requested effects). Nested commands join the step. Edits made directly by the view or a job's apply are steps of their own (`edit_merged` folds a gesture). |
| Built-in commands | `file.new/open/save/save_as/revert`, `app.undo/redo`, `app.quit`, `app.palette` (Mod+Shift+P), `app.keymap`, `view.theme`, `app.document`, with menu paths and shortcuts. File commands take `path` and `discard` so the CLI and agents can use them; in the window they ask (dialogs, "save changes?"). |
| Frame scheduler | Repaints only for input, a revision change during a frame, job wakes, control requests, toasts, injected agent input, and pending work that asks for its own frame when due (the settings and autosave wake hooks, the autosave writer when a write finished): no polling. Every request goes through `ashlar_app::wake` (`wake`, `wake_after`, `waker` for threads; apps add their own `Reason`s), so `app.stats` → `wakes` counts frames and requests per reason. An idle app, clean or with unsaved changes, draws nothing (`examples/notes/tests/wake.rs`). Frame times (p50/p95/max, first frame) in `Runtime::stats` and `app.stats`. |
| Crash safety | Panic hook (chains to earlier hooks) writes `crash-<time>-<pid>.log` (message, location, backtrace, recent log lines) to the logs folder and keeps the last panic. The view runs under `catch_unwind`: a panic suspends it and shows an error window (Save As / Try Again); the session survives. Commands and jobs already catch panics. A launch marker detects a launch that died before its first frame: the next launch starts in safe mode (no restored view settings) and offers to reset them. wasm: console panic hook. |
| Logging | `log` → stderr + `<logs>/<name>.log` (rotated at 4 MiB, 3 kept, written by its own thread) + an in-memory ring of 2000 lines (`app.logs`, crash logs). Level from `<APP>_LOG` or `RUST_LOG`. |
| Autosave | `ashlar_store::Autosave` with a background writer: written when dirty, at most every interval (default 60 s), woken by a timer. Unsaved work from a session that died is offered at startup (Recover / Later / Discard); recovering is one undoable step and leaves the document unsaved. Dirty mark in the title; closing with unsaved changes asks. |
| Menus and keys | In-window menu bar from the commands' menu paths (`ashlar_ui::commands`), or the OS menu bar with feature `native-menu` (macOS/Windows, `ashlar_native::menu`). Keymap from the registry plus user overrides saved in settings (`keymap`), `keys::dispatch` each frame, palette and keymap editor windows. Theme and view state persist in settings. |
| Agent control | `--control <port>` or `<APP>_CONTROL_PORT` (or `App::control_port`) serves the keyed control port. Standard `app.*`/`engine.*` plus `ui.inspect`, `ui.click`, `ui.move`, `ui.drag`, `ui.scroll`, `ui.key`, `ui.type` (injected egui input, answered after the frame that consumed it), `ui.screenshot` (PNG base64, optionally cropped to an element), `ui.menu.list/invoke`, `ui.set` (view state, `theme`), `app.quit`, `app.stats`, `app.logs`. |
| CLI / MCP | `App::cli()` / `cli_main(app)`: `ashlar-cli` over a fresh headless session of the same app. |
| Telemetry | Feature `telemetry` (tracer-sdk, off by default): `command.run` with id, ok and duration at the chokepoint, frame times, first frame, main-loop turns (hang watchdog), crash reports after the runtime's own hook, and a consent dialog: nothing is sent before the user closes it. |

## Testing apps

`Driver` runs a `Runtime` on a headless egui context the way eframe does, on a virtual clock: a
frame runs only when something asked for one. `driver.call("ui.click", json!({"id": "notes.add"}))`
goes through the same handler as the control port. `driver.advance(5.0)` returns the frames run,
so idleness is a test. Screenshots there are blank images (no GPU); the plumbing is real.

## Performance

`cargo test -p ashlar-example-notes --release --test perf -- --ignored --nocapture --test-threads=1`
(Apple Silicon, headless; UI pass time, no tessellation or GPU):

| | |
|---|---|
| idle repaints over 5 s | 0 |
| first frame (runtime creation → end of first frame) | ~30–40 ms |
| frame p95 scrolling 100k notes | ~0.05 ms |
| a command on a 100k-note document (`Arc` per note) | ~1.5 ms |
