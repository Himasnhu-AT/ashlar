# Files and settings

## App folders

Each app gets four folders, in the places the OS expects: `config`, `data`, `cache` and `logs`.
They are named from `App::organization(qualifier, org)` and the app name (the default organisation
is `org` / `ashlar`).

```rust,ignore
App::<Doc>::new("notes", "0.1.0").organization("com", "Acme")
```

To put everything somewhere else (tests, a portable install), pass the folders yourself or set
`ASHLAR_DIRS_ROOT`:

```rust,ignore
use ashlar::fs::AppDirs;

App::<Doc>::new("notes", "0.1.0").dirs(AppDirs::under("/tmp/notes-test"))
```

What goes where:

| Folder | Contents |
| --- | --- |
| `config` | Settings, the control key file |
| `data` | `Recovery` (autosave snapshots) |
| `logs` | The rotating log file and crash logs |
| `cache` | Free for the app |

## Settings

`cx.settings` is keyed JSON with typed getters. Changes are saved after a short delay, with an
atomic write, and flushed when the app ends.

```rust,ignore
let recent_limit: u32 = cx.settings.get_or("recent.limit", 10);
if ui.checkbox(&mut sync, "Sync").changed() {
    let _ = cx.settings.set("sync", &sync);
}
```

`get`, `get_or`, `get_value`, `set`, `set_value`, `remove` and `keys` cover most uses. The runtime
keeps its own entries there too, such as the window placement under `"window"`.

## Recent files

`RecentFiles` is a most-recently-used list that lives in the settings:

```rust,ignore
use ashlar::fs::RecentFiles;

let mut recent = RecentFiles::load(cx.settings, "recent", 10);
if let Some(path) = cx.session().path() {
    recent.push(path);
}
recent.store(cx.settings, "recent");
for p in recent.paths() {
    ui.label(p.display().to_string());
}
```

`prune(|p| p.exists())` drops files that are gone.

## Atomic writes

`ashlar::fs::atomic_write(path, bytes)` replaces a file so that a crash or a power cut leaves the old
content or the new, never a torn mix. Document saves, autosave and settings all use it.

## File dialogs

The built-in `file.open` and `file.save_as` show the native dialogs. For other files use
`ashlar::native::dialogs::FileDialog`:

```rust,ignore
use ashlar::native::dialogs::FileDialog;

match FileDialog::new().title("Import image").filter("Images", &["png", "jpg"]).open_file() {
    Ok(path) => cx.dispatch("image.import", json!({"path": path})),
    Err(e) => log::info!("no file: {e}"), // NativeError::Cancelled when the user cancels
}
```

`open_files`, `pick_folder` and `save_file` work the same way. On Linux they use the XDG desktop
portal; on the web only opening is available.

The `ashlar-fs` crate is headless and builds for wasm32. See
[docs.rs/ashlar-fs](https://docs.rs/ashlar-fs).
