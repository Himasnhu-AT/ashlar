# Installation

```sh
cargo add ashlar-rs
```

The package is published as `ashlar-rs`; the library is imported as `ashlar`:

```rust,ignore
use ashlar::prelude::*;
```

Documents are serialized with serde, so most apps also add it:

```sh
cargo add serde --features derive
```

## Feature flags

| Feature | Default | What it adds |
| --- | --- | --- |
| `accesskit` | on | Screen readers and other assistive technology see the window (AccessKit) |
| `headless` | on | `--headless` on the app's command line: no window, `.ui` scripts, `--snap`, `--tree` |
| `native-menu` | off | The app's commands in the macOS and Windows menu bar |
| `tray` | off | A tray or status-bar icon with a menu (`App::tray`) |
| `hotkey` | off | System-wide shortcuts (`App::global_shortcut`) |
| `telemetry` | off | Crash, performance and usage reports to a server you host |

```sh
cargo add ashlar-rs --features native-menu,telemetry
```

`default-features = false` drops `accesskit` and `headless` for a smaller binary (about 1 MB).

## What the umbrella crate contains

`ashlar` re-exports the layers as modules: `ashlar::app`, `ashlar::command`, `ashlar::store`,
`ashlar::jobs`, `ashlar::fs`, `ashlar::native`, `ashlar::ui`, `ashlar::control`, `ashlar::cli`,
`ashlar::mcp`, plus `ashlar::egui` and `ashlar::serde_json`. `ashlar::prelude` has what most apps
need: `App`, `Cx`, `Session`, `Document`, `Driver`, `Effect`, `CommandSpec`, `CommandError`,
`Param`, `Params`, `cmd!`, `query!`, `always`, `Jobs`, `apply`, `Format`, `Revision`,
`ToastLevel`, `egui`, `Value` and `json!`.

Some crates are not re-exported. Add them yourself when you need them:

| Crate | When |
| --- | --- |
| `ashlar-motion` | Spring and tween curves for `Cx::motion` and `ashlar::ui::motion` |
| `ashlar-diff` | Building the `FileDiff`s that `ashlar::ui::diff_view` shows |
| `ashlar-debug` | `.ui` script reports and golden snapshots in tests (usually a dev-dependency) |
| `ashlar-native` with `notify` | System notifications |

Each `ashlar-*` crate also works on its own. A headless tool that only needs documents and undo can
depend on `ashlar-store` alone.

## Linux build dependencies

Debian and Ubuntu package names:

```sh
sudo apt-get install libgtk-3-dev libxdo-dev libxkbcommon-dev libdbus-1-dev
```

## Binary size

The smallest app (`examples/hello`) is 13.6 MB stripped in a release build on macOS arm64. A
profile with fat LTO, one codegen unit, `opt-level = "s"` and stripping brings it to 9.0 MB:

```toml
[profile.release-small]
inherits = "release"
lto = "fat"
codegen-units = 1
opt-level = "s"
strip = true
```

```sh
cargo build --profile release-small
```
