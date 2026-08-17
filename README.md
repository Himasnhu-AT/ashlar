# Tracer

Crash, performance and usage telemetry for Rust desktop apps, with a backend you host yourself.

- **`tracer-sdk`** — a small client you embed. Safe Rust only, no async runtime, no UI toolkit. It never blocks your frame loop and never prints.
- **`tracer-server`** — one binary and one data folder: ingest API, SQLite storage, and a dashboard for crashes, performance and usage.
- **`tracer-proto`** — the wire types both share.

> Status: early development. See [ROADMAP.md](ROADMAP.md).

## What it collects

| Signal | What you get |
|---|---|
| Crashes | Panics with backtrace, handled errors, unclean exits found on next launch, abnormal child-process exits, each with recent log lines and device details. |
| Hangs | A report when the main loop stops responding. |
| Performance | Frame-time histogram, slow and frozen frames, startup time, process CPU and memory, plus your own counters and gauges. |
| Usage | Sessions, daily opens, focused / active / idle time, and the events you declare. |

The host app decides what is tracked. Events are declared as enums and checked at build time; anything not declared and listened to is never recorded. End users can switch each category off, and `DO_NOT_TRACK=1` is honoured. [docs/privacy.md](docs/privacy.md) lists every field.

## Sketch

```rust
tracer_sdk::events! {
    pub enum Category { File = "file", Edit = "edit" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
        Undo = "edit.undo" in Edit,
    }
}

let tracer = tracer_sdk::Tracer::builder("app-write-key", "my-app", env!("CARGO_PKG_VERSION"))
    .data_dir(my_data_dir)
    .endpoint("https://telemetry.example.com")
    .catalog::<AppEvent>()
    .listen(Category::File)
    .init();

tracer.track(AppEvent::FileOpen);
tracer.frame(frame_duration);   // once per frame
tracer.heartbeat();             // once per main-loop turn
```

## Documentation

- [Integrating the SDK into a host app](docs/integration.md)
- [Self-hosting the server](docs/self-hosting.md)
- [Architecture](docs/architecture.md)
- [Wire format](docs/wire-format.md)
- [Privacy and collected fields](docs/privacy.md)

## Development

```
cargo xtask ci
```

## Licence

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
