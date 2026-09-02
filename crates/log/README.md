# ashlar-log

Logging and crash safety for ashlar apps and command-line tools. Headless, depends only on `log`
and std, builds for `wasm32-unknown-unknown`. `ashlar-app` re-exports it as `ashlar_app::{logging, crash}`.

| Item | What it does |
|---|---|
| `Filter::parse(spec)` | `env_logger` directives: `warn,my_crate=debug,my_crate::io=trace,wgpu=off`. Longest matching target prefix wins; bare `target` = everything from it; typos are skipped; input is capped. `max_level()` for `log::set_max_level`. |
| `filter_from_env(&["MYAPP_LOG", "RUST_LOG"], default)` | The first of those variables that is set and not blank, else `default`. |
| `logging::Logger` | `new(name, dir, level, stderr)`, `.with_filter(f)`, `.with_output(f)`: stderr (optional, a closed pipe is ignored), `<dir>/<name>.log` written by its own thread and rotated at 4 MiB (3 old files kept), and an in-memory ring of the newest 2000 lines. Never writes stdout. |
| `logging::{install, flush, recent, dropped, level_of}` | Install once per process; `flush()` waits for the file thread (call it before a CLI exits); `recent(limit, level)` reads the ring; `dropped()` counts lines the file thread could not keep up with. |
| `crash::install_with(CrashConfig)` | Panic hook, installed once and chained to the previous hook. `CrashConfig { app, version, logs_dir, file_name, append, capture_backtrace }`; `CrashConfig::new` is `crash-<unix>-<pid>.log`, one file per panic, with a backtrace. `crash::install(dir, app, version)` is that default. |
| `crash::{last_panic, take_last_panic, PanicRecord}` | The most recent panic, for an error window. |
| `crash::LaunchGuard` | A marker file that tells the next launch this one died before it was up (safe mode). |

Feature `serde`: `Serialize` for `LogLine` and `PanicRecord`.

```rust,ignore
// a CLI: warnings to stderr unless MYTOOL_LOG / RUST_LOG say otherwise; stdout stays clean
let logger = ashlar_log::Logger::new("mytool", None, log::LevelFilter::Trace, true)
    .with_filter(ashlar_log::filter_from_env(&["MYTOOL_LOG", "RUST_LOG"], "warn"));
let max = logger.max_level();
ashlar_log::logging::install(Box::new(logger), max);
ashlar_log::crash::install_with(ashlar_log::crash::CrashConfig {
    file_name: Box::new(|r| format!("crash-{}.log", r.unix / 86_400)), // one file a day
    append: true,
    capture_backtrace: false,
    ..ashlar_log::crash::CrashConfig::new("mytool", env!("CARGO_PKG_VERSION"), Some(logs_dir))
});
```
