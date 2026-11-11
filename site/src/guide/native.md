# Native OS features

`ashlar::native` (the `ashlar-native` crate) wraps OS features behind one API. Where a feature is
not available on a platform, the call returns `NativeError::Unsupported`; nothing panics. Most
apps use these through the `App` builder rather than calling the crate directly.

| Feature | macOS | Windows | Linux | Web |
|---|---|---|---|---|
| File dialogs | yes | yes | yes (portal) | open only |
| Native menu bar | yes | yes | in-window menu | in-window menu |
| Tray icon | yes | yes | yes (D-Bus) | no |
| Global shortcuts | yes | yes | X11 only | no |
| Clipboard (text, image) | yes | yes | yes | no |
| Notifications | bundled apps | yes | yes (D-Bus) | no |
| Single instance | yes | yes | yes | no |

The full matrix is in the
[ashlar-native README](https://github.com/Himasnhu-AT/ashlar/blob/main/crates/native/README.md#support-matrix).

## Menu bar

The app's commands, grouped by their `.menu(&[…])` path, form the menu bar. By default it is drawn
inside the window. With the `native-menu` feature it moves to the macOS menu bar and the Windows
window menu:

```sh
cargo add ashlar-rs --features native-menu
```

## Tray icon

With the `tray` feature, `App::tray` shows a tray or status-bar icon. Each menu item's id is a
command id; choosing it runs the command. Clicking the icon shows and focuses the window.

```rust,ignore
use ashlar::command::menu::{Item, MenuNode, MenuSpec, Submenu};
use ashlar::native::tray::{Icon, TraySpec};

let icon = Icon { rgba: icon_rgba, width: 32, height: 32 };
let menu = MenuSpec::new(vec![Submenu::new("Tray", vec![MenuNode::Item(Item::new("note.add", "New Note"))])]);
App::<Notes>::new("notes", "0.1.0").tray(TraySpec::new("notes", "Notes", icon, menu).template(true))
```

`template(true)` makes macOS tint the icon for the menu bar. Where the desktop has no tray, the
icon is skipped with a log line.

## Global shortcuts

With the `hotkey` feature, `App::global_shortcut(command, chord)` runs a command when the chord is
pressed anywhere, even while another app has the focus. The window comes to the front first.

```rust,ignore
App::<Notes>::new("notes", "0.1.0").global_shortcut("note.add", "Mod+Shift+Space")
```

A chord without Ctrl, Alt or ⌘, or one another app already holds, is skipped with a log line.

## Drag and drop

When the app sets a `Format` or a `file_extension`, files dropped on the window open: the first
file with a known extension goes to `file.open` (asking to save a dirty document first). Other
files reach the view:

```rust,ignore
for path in cx.dropped_files() {
    cx.toasts.info(format!("Dropped {}", path.display()));
}
```

`App::accept_drops(false)` leaves drops entirely to the view.

## Single instance

```rust,ignore
App::<Notes>::new("notes", "0.1.0").single_instance(true)
```

A second launch hands its arguments (the files to open) to the running app and exits. The running
app opens them with `file.open` and comes to the front. Desktop only.

## Notifications

Notifications need the `notify` feature of `ashlar-native`:

```sh
cargo add ashlar-native --features notify
```

```rust,ignore
use ashlar_native::notify::{self, Notification};

let _ = notify::notify(&Notification::new("Export finished", "12 files written").with_id("export"));
```

A notification with the same id replaces the previous one. `notify::on_click(waker)` reports
clicks. On macOS this works only in an app bundle with a bundle identifier; elsewhere it returns
`Unsupported`.

## Other features

`ashlar::native` also has the clipboard with rich types (`clipboard`), keep-awake (`power`), files
opened from the file manager (`open_files`), system preferences such as accent colour, reduced
motion and contrast (`system`), and, behind their own features, dock and taskbar progress
(`progress`), the OS keychain, and dragging files out of the window (`drag-out`, macOS). See
[docs.rs/ashlar-native](https://docs.rs/ashlar-native).
