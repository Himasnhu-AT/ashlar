# FAQ and troubleshooting

## The build fails on Linux

Install the system libraries (Debian and Ubuntu names):

```sh
sudo apt-get install libgtk-3-dev libxdo-dev libxkbcommon-dev libdbus-1-dev
```

Native file dialogs on Linux go through the XDG desktop portal, so the desktop needs a portal
service (`xdg-desktop-portal` and a backend for your desktop) at run time.

## Which Rust version do I need?

1.95 or newer, edition 2024. CI tests the latest stable and checks 1.95.

## Which egui version?

ashlar 0.2 is built on egui and eframe 0.36 with the wgpu renderer. Use the `egui` that ashlar
re-exports (`ashlar::egui`, also in the prelude) rather than adding your own `egui` dependency, so
the versions cannot drift apart. If you do depend on egui directly, pin the same minor version.

## Is the API stable?

No. ashlar is at 0.2; the API will change between minor versions until 1.0. Each release lists
its changes in the [changelog](../CHANGELOG.md). Pin the minor version (`ashlar-rs = "0.2"`).

## The window does not update when my data changes

An ashlar app draws a frame only when something asks for one: input, a document change, a
finished job, a running animation. If your data changes some other way (a thread of your own, a
socket, a timer), ask for a frame:

```rust,ignore
use ashlar::ui::wake::{self, Reason};

const FEED: Reason = Reason("feed"); // your own reason, shown in app.stats

let waker = wake::waker(cx.ctx(), FEED); // Send + Sync: hand it to the thread
std::thread::spawn(move || {
    // … new data arrived …
    waker.wake();
});
wake::wake_after(cx.ctx(), Reason::TIMER, std::time::Duration::from_secs(1)); // a timed frame
```

Edits through commands, `cx.edit` and jobs ask for frames themselves. `app.stats` over the
control port shows which reasons asked for frames, which also helps find an app that never goes
idle.

## The app is slow with a long list

Use `virtual_list::VirtualList` or `VirtualTable` instead of a loop over `ui.label`: they lay out
only the visible rows. Key derived data (filtered rows, layouts) by `cx.revision()` so it is
rebuilt only when the document changes. Wrap a plain list in `list::checked(ui, |ui| …)` and debug builds warn when it builds too many widgets in one frame.

## What works on the web?

The headless core (commands, documents, files, jobs, logging) builds for `wasm32-unknown-unknown`
and is checked in CI. The windowed runtime runs in a browser canvas but is experimental. There are
no threads (jobs run inline; use `spawn_stepped` for long work), no disk (use in-memory or
browser-storage backends), file dialogs can only open, and there is no tray, global shortcut,
notification, clipboard, single instance or control port. See [Web](web.md).

## Notifications do nothing on macOS

macOS delivers notifications only to an app bundle with a bundle identifier. A binary started with
`cargo run` gets `NativeError::Unsupported`. Test them from a bundled build.

## The command does not appear in the menu

Give it a menu path with `.menu(&["Edit"])` (or the third argument of `cmd!`). Commands made with
`query!` are never in menus. A command whose id is already registered is ignored with a warning in
the log.

## How do I see what went wrong?

- The log: stderr, the rotating file in the app's `logs` folder, or `app.logs` over the control
  port. Raise the level with `<APP>_LOG=debug` (for an app named `notes`, `NOTES_LOG=debug`).
- Crash logs are in the same folder.
- `--safe-mode` starts without restoring view settings.
- `ashlar-debug open ./target/debug/my-app` then `ashlar-debug snapshot -i` shows what the UI
  contains; see [Testing and debugging UIs](../docs/debugging-uis.md).

## How big is a release binary?

About 13.6 MB stripped for the smallest app on macOS arm64, 9.0 MB with a size-optimised profile.
See [Installation](installation.md#binary-size).
