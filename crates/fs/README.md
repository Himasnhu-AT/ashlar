# ashlar-fs

Files and settings for ashlar apps. Headless, no UI dependencies, builds for `wasm32-unknown-unknown`.

| Item | What it does |
|---|---|
| `AppDirs::new(qualifier, org, app)` | Config, data, cache and logs directories where each OS expects them (via the `directories` crate). `ASHLAR_DIRS_ROOT=/some/dir` puts all four under one folder (tests, portable installs). `AppDirs::under(root)` does the same in code. |
| `atomic_write(path, bytes)` | Durable replace: hidden temp file in the same directory, write, `fsync`, rename over the target, `fsync` the directory (Unix). On failure the temp file is removed and the old file is untouched. Follows symlinks, keeps permissions. `atomic_write_with(path, \|file\| ...)` streams instead of buffering. |
| `ConfigStore` | Named text blobs (`settings.json`, `keymap.json`). `DirConfig` (a folder, atomic writes) and `MemoryConfig`. Names that climb out of the store (`..`, absolute) are refused. |
| `FileOps` | Path-based read / atomic write / remove / list, for autosave and recovery. `StdFiles` and `MemFiles`. A browser (OPFS) backend implements the same trait. |
| `Settings` | Keyed JSON: `get::<T>`, `get_or`, `set`, `remove`, `keys`. File shape `{"schema_version": N, "values": {...}}` with a migration chain. Saves after a quiet period (`tick(now)`) or at once (`flush()`). A corrupt file is moved to `<name>.corrupt`; a file from a newer schema is read but never overwritten. |
| `RecentFiles` | MRU list: cap, dedupe (lexically normalized paths), `prune(exists)`, load/store through `Settings`. |

## Clock

`Settings::tick` takes a monotonic `Duration` from the caller instead of reading `Instant`, so it behaves the same on the web (where `Instant::now()` panics) and is deterministic in tests.

## Durability notes

- The parent directory must exist (`StdFiles::write` creates it).
- Windows has no directory `fsync`; the rename goes through `MoveFileExW`, which NTFS journals.
- Leftover temp files from a crash mid-save are named `.<name>.<pid>-<n>.tmp`; `atomic::is_temp_name` recognises them and `StdFiles::list` hides them.
