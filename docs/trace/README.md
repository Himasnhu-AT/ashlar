# ashlar-trace: telemetry

Crash, performance and usage telemetry for Rust desktop apps, with a backend you host yourself.

- **`ashlar-trace`** — a small client you embed. Safe Rust only, no async runtime, no UI toolkit. It never blocks your frame loop and never prints.
- **`ashlar-trace-server`** — one binary and one data folder: ingest API, SQLite storage, and a dashboard for crashes, performance and usage.
- **`ashlar-trace-proto`** — the wire types both share.

> Status: release candidate, load-tested; not yet published. See [roadmap.md](roadmap.md). Apps built on ashlar use it through `ashlar-telemetry`, which wires it to the command chokepoint, logging, the frame loop and native dialogs.

## What it collects

| Signal | What you get |
|---|---|
| Crashes | Panics with backtrace, handled errors, unclean exits found on next launch, abnormal child-process exits, each with recent log lines and device details. |
| Hangs | A report when the main loop stops responding. |
| Performance | Frame-time histogram, slow and frozen frames, startup time, process CPU and memory, plus your own counters and gauges. |
| Usage | Sessions, daily opens, focused / active / idle time, and the events you declare. |

The host app decides what is tracked. Events are declared as enums and checked at build time; anything not declared and listened to is never recorded. End users can switch each category off, and `DO_NOT_TRACK=1` is honoured. [docs/privacy.md](privacy.md) lists every field.

## Sketch

```rust
ashlar_trace::events! {
    pub enum Category { File = "file", Edit = "edit" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
        Undo = "edit.undo" in Edit,
    }
}

let tracer = ashlar_trace::Tracer::builder("app-write-key", "my-app", env!("CARGO_PKG_VERSION"))
    .data_dir(my_data_dir)
    .endpoint("https://telemetry.example.com")
    .listen(Category::File)
    .init();

// Nothing is sent until the user has seen your first-run notice.
tracer.notice_shown();

// Once per turn of the main loop:
let _turn = tracer.turn();      // a turn that takes too long is reported as a hang
tracer.track(AppEvent::FileOpen);
tracer.frame(frame_duration);   // once per frame
```

## Documentation

- [Integrating the SDK into a host app](integration.md)
- [Self-hosting the server](self-hosting.md)
- [Architecture](architecture.md)
- [Wire format](wire-format.md)
- [Privacy and collected fields](privacy.md)

## Development

The crates are part of the ashlar workspace and its gate (`cargo xtask ci`). Heavier runs:
`cargo xtask trace-stress` (the SDK's benchmarks and stress tests, release build),
`cargo xtask trace-load` (a short load profile against a spawned release server) and
`cargo xtask trace-demo` (checks the windowed demo, which is its own workspace).
