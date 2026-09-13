# ashlar-native

OS features every desktop app needs, behind one small, safe API. No UI toolkit dependency (no
egui / eframe / winit): events arrive on channels, and callbacks take a `Waker` so an idle app
wakes only when something happened. Every feature is a cargo feature and degrades cleanly: where
it is not implemented, a call returns `NativeError::Unsupported`; nothing panics.

Errors are one enum, `NativeError`: `Unsupported`, `Denied(msg)`, `Cancelled` (a dismissed
dialog), `Invalid(msg)` (bad input from the caller), `Os(msg)` (one line, safe to show).

| Module (feature) | API |
|---|---|
| `dialogs` | `FileDialog::new().title(..).directory(..).file_name(..).filter("Images", &["png"])`, then `open_file / open_files / pick_folder / save_file` (blocking) or `*_async`; `open_bytes_async(max)` reads the picked file (works on the web). `message(MessageLevel::Error, title, text)`: a modal message box (start-up failures, command-line output without a console). |
| `menu` | `MenuSpec` (serde): `Submenu`s of `Item { id, label, accelerator: "Mod+Shift+S", enabled, checked }`, separators, `Role`s (About, Services, Hide, Quit, Window items, Edit items). `structure_hash` / `state_hash`, `validate`, `from_commands(&[CommandInfo], order)`, `from_items([(path, Item)], order)` (check marks, translated titles). `NativeMenuBar::install(&spec, waker)`, `update(&spec) -> Unchanged / Patched / Rebuilt`, `drain() -> Vec<id>` (an activation makes the next `update` push the state again, so a check mark the OS flipped on click shows the app's state), `attach_window(&window)` (Windows). `Accelerator::parse`, `is_menu_safe`, `to_muda`. |
| `clipboard` | `text / set_text`, `image / set_image` (RGBA8), `clear`; custom types `set_custom(&[(type, bytes)])`, `custom(type)`, `types()`. |
| `power` | `KeepAwake::new(Awake::Busy / System / Display, reason)`: an RAII guard. |
| `open_files` (`open-files`) | `OpenFiles::new(waker)`: `push_args(argv, cwd)`, `listen_os()` (macOS Apple events), `sender()` for other sources, `drain() -> Vec<Opened::File / Url>`. `from_args`, `classify`, `file_url_to_path`. |
| `single_instance` (`single-instance`) | `SingleInstance::new(app_id).claim(&args, on_forward) -> Launch::Primary(guard) / Forwarded`. |
| `notify` | `notify(&Notification::new(title, body))`. |
| `progress` | `set(window, Progress::None / Indeterminate / Normal(f) / Paused(f) / Error(f))`, `set_badge(Some("3"))`. |
| `secrets` | `Keychain::new(service)`: `get / set / delete / available`. |
| `system` | `preferences() -> { appearance, accent, reduced_motion, increased_contrast }`, `open_url` (http, https, mailto only), `open_path`, `reveal`. |
| `cli_tool` (`cli-tool`) | `CliTool::for_current_app(name)` (the tool at `<exe dir>/../Helpers/<name>`, i.e. `Contents/Helpers/<name>` in a macOS bundle) or `CliTool::new(name, helper)`; `status()` → `Missing / Ours / Broken (the app moved) / OtherCopy / ForeignLink / File / Folder`; `install(replace)` links `/usr/local/bin/<name>` → the helper, asks for admin rights through the macOS password prompt when the folder isn't writable, falls back to `~/.local/bin` with a `PATH` hint when declined, repairs a stale link, and never replaces a file or link that isn't ours unless `replace` (never a folder); `uninstall()` removes only our links; `repair()`. `ASHLAR_CLI_BIN_DIR` / `ASHLAR_CLI_FALLBACK_DIR` redirect both folders and turn the prompt off (tests). `commands::<S: CliToolHost>(menu)`: `app.installCommandLineTool` (param `replace`), `app.uninstallCommandLineTool`, `app.commandLineToolStatus`. |

Default features: `dialogs menu clipboard power open-files single-instance system cli-tool`. `full` adds
`notify progress secrets`.

## Support matrix

✓ implemented and tested on this platform · ◐ implemented, compile-checked only (never run) ·
– `Unsupported`

| Feature | macOS | Windows | Linux | Web |
|---|---|---|---|---|
| dialogs | ✓ (unit tests; pickers by hand) | ◐ rfd | ◐ rfd, XDG portal | ◐ `open_bytes_async` only |
| menu spec | ✓ | ✓ (pure Rust) | ✓ | ✓ |
| native menu bar | ✓ install / patch / rebuild on the main thread | ◐ per window via `attach_window` | – | – |
| clipboard text / image | ◐ arboard (round trip is an ignored test: it replaces the clipboard) | ◐ | ◐ X11 / Wayland via arboard | – |
| clipboard custom types | ◐ NSPasteboard (same ignored test) | – | – | – |
| power | ✓ `pmset` shows the assertion while the guard lives | ◐ SetThreadExecutionState | ◐ logind `Inhibit` | – |
| open_files: argv | ✓ | ✓ | ✓ | – |
| open_files: OS events | ◐ Apple event parsing is tested; delivery needs a bundled app with document types | – (argv) | – (argv) | – |
| single_instance | ✓ Unix socket, across processes | ◐ loopback + token | ◐ Unix socket in `$XDG_RUNTIME_DIR` | – |
| notify | ◐ bundled apps only; `Unsupported` unbundled (tested) | – | ◐ D-Bus | – |
| progress / badge | ✓ runs on the main thread (Dock tile) | ◐ ITaskbarList3 (badge –) | – | – |
| secrets | ◐ Keychain (round trip is an ignored test) | ◐ Credential Manager | ◐ Secret Service | – |
| system preferences | ✓ | ◐ registry + SPI | ◐ portal settings | ◐ media queries |
| open_url / reveal | ◐ NSWorkspace | ◐ ShellExecuteW / explorer | ◐ xdg-open / FileManager1 | ◐ `window.open` (url only) |
| cli_tool | ✓ links and conflicts in temp folders, admin path with a stand-in runner; the real password prompt by hand | – | ◐ links, no prompt (falls back) | – |

Checked on macOS (aarch64): `cargo test -p ashlar-native --features full` (unit tests plus
`tests/main_thread.rs`, which runs the main-thread APIs on the real main thread) and
`cargo clippy -D warnings` for `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu` and
`wasm32-unknown-unknown`, default and `full` features. Nothing has run on Windows or Linux yet.

By hand: `cargo run -p ashlar-native --features full --example native_demo -- <command>`
(`prefs`, `open`, `save`, `clip-custom`, `awake 10`, `single`, `menu`, …).

## Threads

- macOS menus, Dock progress and the Apple event handlers must be set up on the main thread;
  off it they return an error. Blocking dialogs on macOS also belong on the main thread (use the
  async ones inside an event loop).
- `KeepAwake` and `NativeMenuBar` are not `Send`: the Windows power request belongs to the
  thread that made it, and menu objects to the main thread.

## Notes for integration

- `menu::MenuSpec` is independent of egui. `ashlar-ui` has its own menu model for the in-window
  bar; the two should be unified (one spec drawn in the window on the web and Linux, and handed
  to `NativeMenuBar` on macOS / Windows). `MenuSpec::from_commands` already mirrors
  `ashlar_command::CommandInfo` menu paths and shortcuts. `ashlar-command` has its own shortcut
  parser (`Mod+Shift+S`, alternatives with ` / `); `menu::Accelerator` should defer to it once
  both are merged.
- File associations are declared at packaging time (Info.plist `CFBundleDocumentTypes`,
  registry ProgIDs, `.desktop` `MimeType=`); `open_files` only receives what the OS sends.
- `unsafe` is confined to the modules listed in `docs/adr/0001-native-ffi.md`.
