# Integrating the SDK into a host app

This guide walks through adding `tracer-sdk` to an application. Every snippet is taken from, or was checked against, one of the two demo hosts in this repository:

- [`examples/demo-cli`](../examples/demo-cli): a scriptable command-line process. It is the one the end-to-end tests drive, so everything it does is tested against a real server.
- [`examples/demo-gui`](../examples/demo-gui): a small `eframe`/`egui` window that wires the SDK the way a real GUI host would. It compiles and passes clippy (`cargo xtask demo`); it has not been run, because it needs a display.

What the SDK may collect is listed in [privacy.md](privacy.md). How the pieces fit together is in [architecture.md](architecture.md).

## 1. Add the dependency

```toml
[dependencies]
tracer-sdk = "0.1"
```

Default features are `http` (a blocking HTTPS client on the SDK's own thread) and `sysinfo` (process CPU and memory, device details). Without `http`, nothing leaves the machine unless you supply a `Transport` through `Builder::transport`. Without `sysinfo`, the CPU, memory and device fields are simply absent.

The SDK needs Rust 1.90, has no async runtime, forbids `unsafe`, and never writes to stdout.

## 2. Declare the catalog

Events are declared once, as two enums. A name that is not lower-case `snake_case` (dots allowed, at most 64 bytes), or that is used twice, fails the build.

```rust
tracer_sdk::events! {
    pub enum Category {
        File = "file",
        Edit = "edit",
        View = "view",
    }
    pub enum DemoEvent {
        FileOpen = "file.open" in File,
        FileSave = "file.save" in File,
        EditUndo = "edit.undo" in Edit,
        ViewZoom = "view.zoom" in View,
    }
}
```

Only what you declare and listen to is ever recorded. `Tracer::track` takes the enum, so a misspelled event does not compile.

## 3. Build the tracer

Do it once, early in `main`, and keep the returned `Tracer` alive for the whole process. It is cheap to clone and safe to share across threads. Dropping the last clone ends the session and sends what is waiting.

```rust
let mut builder = Tracer::builder(&key, "demo", env!("CARGO_PKG_VERSION"))
    .data_dir(&data_dir)
    .listen_all::<DemoEvent>()
    .logger(LoggerMode::Install)
    .process(ProcessKind::Gui);
if let Some(endpoint) = &endpoint {
    builder = builder.endpoint(endpoint);
}
let tracer = builder.init();
```

What each setting means:

- **Key, name, version.** `Tracer::builder(write_key, app_name, app_version)`. The write key comes from `tracer-server app create` (see [self-hosting.md](self-hosting.md)). It is not a secret: it ships inside the app and only allows ingest.
- **`data_dir`.** The host's own data directory. The SDK keeps its files in a `tracer` folder inside it (see section 3.2 of the architecture). Each host picks its own location, so this is required. If it is empty or the folder cannot be created, `init` returns a disabled tracer rather than failing.
- **`endpoint`.** The server's base URL, such as `https://telemetry.example.com`, with no path. Without one the SDK records into its spool and sends nothing.
- **`build`.** An optional build identifier such as a commit hash, shown next to the version.
- **`process`.** `Gui` (the default), `Cli`, `Service` or `Child`. The dashboard counts "daily opens" from `Gui` sessions only, so give tools and helper processes their own kind.
- **`listen`.** `listen(category)`, `listen_events(&[...])` and `listen_all::<E>()` choose what is recorded in this run. An event that is not listened to is dropped before it reaches the queue. Each of them also tells the server what the app declares (so it can show events that were never fired and set apart names that were never declared), so they can come in any order and nothing else is needed. `catalog::<E>()` exists for the rare build that declares events without listening to any.
- **`send_before_notice`.** By default nothing is sent until you call `notice_shown()` (section 9). Set this to `true` only if the user sees your notice somewhere else, such as in an installer.
- **`gpu`.** Graphics adapter details are only known to the host. If you have them, pass `Gpu { name, vendor, backend, driver }` to `Builder::gpu`. They go into the device details on every envelope.
- **`signals` / `without`.** All built-in signals are on by default; `without(Signal::Hang)` switches one off.
- **`configure`.** Adjusts limits and intervals (`flush_interval`, `heartbeat_interval`, `perf_window`, `hang_threshold`, `spool_max_bytes` and so on). The demo-cli uses it to shrink every interval to milliseconds for tests:

```rust
builder = builder.configure(|config| {
    config.tick_interval = Duration::from_millis(20);
    config.heartbeat_interval = Duration::from_millis(100);
    config.hang_threshold = Duration::from_millis(400);
});
```

### GPU details

In the GUI demo the tracer is built inside eframe's app-creator callback, because that is where the toolkit's creation context exists. With the `wgpu` renderer the adapter is in `cc.wgpu_render_state` (`adapter.get_info()` gives the name, vendor and backend). With the `glow` renderer used by the demo, the renderer string can only be read through an `unsafe` GL call, which the demo's workspace forbids, so `demo-gui` passes no GPU. The SDK does not need one.

## 4. Feed it from the main loop

The performance and hang signals need a few calls from the loop that draws the UI. All of them are an atomic operation or a short lock; none blocks on disk or network.

| Call | Where | Why |
|---|---|---|
| `turn()`, or `begin_turn()` / `end_turn()` | Around each piece of work the main loop does | Hang watchdog: a turn that takes longer than `hang_threshold` is a hang |
| `frame(duration)` | Around one UI pass | Frame-time histogram, slow and frozen frames |
| `first_frame()` | After the first frame is on screen | Startup time since `init` (recorded once) |
| `set_focused(bool)` | When the window gains or loses focus, or every turn | Focused versus unfocused time |
| `note_input()` | When the user does something | Active versus idle time (idle after 120 s of no input by default) |

### Turns and the hang watchdog

A hang is the main loop being stuck in one piece of work. So tell the SDK when a piece of work starts and ends:

```rust
let _turn = tracer.turn(); // ends when the guard is dropped
handle_the_event();
```

A turn that stays open longer than `hang_threshold` (2 s by default) is reported once, with how long it has been blocked. The time between turns, while the loop waits for the next event, is never a hang, however long it lasts. That makes this work for hosts that repaint only on input, and for hosts whose event loop belongs to a GUI framework: you do not need to see the wait, only your own callbacks. No timed repaint or wake-up is needed.

`begin_turn()` and `end_turn()` are the same thing without the guard. Each is one atomic update. A second `begin_turn` while a turn is open keeps the original start, and `end_turn` without an open turn does nothing, so nesting and early returns are harmless.

### An immediate-mode GUI

This is the wiring from `demo-gui`. eframe owns the event loop and runs it only on input. It calls `logic` once per loop turn (including turns where nothing is drawn) and `ui` for each frame, and each is marked as a turn:

```rust
impl eframe::App for DemoApp {
    fn logic(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        let _turn = self.tracer.turn();
        let (focused, active) = ctx.input(|input| (input.focused, !input.events.is_empty()));
        self.tracer.set_focused(focused);
        if active {
            self.tracer.note_input();
        }
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let _turn = self.tracer.turn();
        let started = Instant::now();
        self.contents(ui);
        self.tracer.frame(started.elapsed());
        if !self.first_frame_done {
            self.first_frame_done = true;
            self.tracer.first_frame();
        }
    }
}
```

`frame` measures the cost of producing a frame (here: building the UI), not the time between frames. That is what matters for hosts that repaint only on input.

### A loop that runs continuously

A game-style loop that never waits can call `heartbeat()` once per iteration instead: a last beat older than the threshold is a hang. Do not use it for a loop that waits for input, because every wait longer than the threshold would look like a hang. Once a host has begun a turn, heartbeats no longer count, so the two are not mixed.

Hosts that call neither get no hang reports at all, never false ones. `.without(Signal::Hang)` switches the watchdog off.

### Pausing hang detection

When the main loop is legitimately blocked, for example by a native modal dialog, pause detection around it. Pauses nest, and resuming restarts the measurement, so the paused time is not a hang:

```rust
self.tracer.pause_hang_detection();
std::thread::sleep(Duration::from_secs(3));
self.tracer.resume_hang_detection();
```

The GUI demo has two buttons that block the UI thread for 3 s: one leaves detection on (and produces a `hang` issue with `blocked_ms` of about the threshold or more), one pauses it (and produces nothing).

## 5. Track usage

`track` records a declared event. `track_with` adds up to 16 properties, each a bool, integer, float or `&'static str` of at most 64 bytes. There is deliberately no way to pass a runtime string, so file names and typed text cannot get in by accident.

```rust
tracer.track(DemoEvent::FileOpen);
tracer.track_with(
    DemoEvent::FileSave,
    &[("count", Prop::Int(3)), ("format", Prop::Str("native")), ("ratio", Prop::Float(0.5)), ("recent", Prop::Bool(true))],
);
```

### A central command dispatcher

Hosts that run every user action through one function keyed by a command id can record the id there, in one place:

```rust
tracer.track_id(Category::Edit, "edit.palette_command");
```

`track_id` records an id that is not in the catalog, marked `dynamic`, under a category you listen to. Rules:

- Pass ids only, and only fixed program strings (`&'static str`). Never pass parameters, arguments, paths or anything the user typed.
- The id must follow the same naming rule as event names. An invalid one is dropped and counted.
- Dynamic ids are exempt from the server's catalog check, so a command registered at run time does not have to be in the enum.

Declare an event in the catalog when you want it to appear in the "declared but never fired" list; use `track_id` for the long tail.

## 6. Logs, handled errors and child processes

### Log lines as breadcrumbs

The SDK keeps the last 100 log lines at `Info` and above, scrubbed, and attaches them to a crash. They leave the device only inside a crash report.

- **The host has no logger.** `.logger(LoggerMode::Install)` installs the SDK's logger globally. This is what `demo-cli` does by default.
- **The host already installs its own logger.** Wrap it and install the wrapper in its place. Nothing is lost: every record still reaches the wrapped logger. This is `demo-cli --wrap-logger`, tested end to end:

```rust
let tracer = builder.init(); // LoggerMode::None, the default
let wrapped = tracer.logger(Some(Box::new(MyLogger)));
if log::set_boxed_logger(wrapped).is_ok() {
    log::set_max_level(log::LevelFilter::Info);
}
```

Only one global logger can exist, so the host must not also install its own. Set the maximum level yourself: `log` ignores every record until you do. With a disabled tracer, `Tracer::logger(Some(inner))` simply returns `inner`.

### Handled errors

For an error you caught and survived. `context` is a fixed label such as a command id, never the message's variable parts:

```rust
let failure = std::io::Error::other("could not write the preview");
tracer.report_handled(&failure, Some("preview.write"));
```

It becomes an issue of kind `handled`, counts towards the session's `errors`, and carries the breadcrumbs. The message is scrubbed (home directory, account name) and truncated, but it is still text your code wrote: do not put user content in error messages.

### Child processes

If the host spawns helpers that can abort, report how they ended. The SDK records it only when the status is not success:

```rust
let status = command.status()?;
tracer.report_child_exit("demo-child", status);
```

`program` is a fixed label. The report carries the exit code or the signal (on Unix). A child that links the SDK itself, with `ProcessKind::Child` and the same data directory, also gets its own session. If it dies without ending it, the next process that starts reports an `unclean_exit` for it.

## 7. Counters and gauges on a realtime thread

Register once, on a normal thread (registration locks and allocates). Record from the realtime thread: each call is one atomic operation, with no lock, allocation or system call.

```rust
let saves = tracer.counter("saves");   // a Counter, Arc-backed and cheap to clone
saves.add(7);
let layers = tracer.gauge("layers");   // a Gauge
layers.set(4.0);
```

Counters are reported per performance window as the increase since the previous window (zero increases are left out). A gauge is reported when it was set during the window. There is a cap on how many one process can register; handles over the cap work but are never reported. On a disabled tracer, both return detached handles that do nothing.

## 8. Short-lived tools

A command-line tool that runs for a moment never reaches the periodic send. Use `ProcessKind::Cli` and finish with `flush_blocking`:

```rust
if !tracer.flush_blocking(Duration::from_secs(5)) {
    // Not everything could be sent in time. It stays in the spool; the next run sends it.
}
drop(tracer); // ends the session and tries one last send
```

`flush_blocking` returns true when this process has nothing left that it could send. It never waits longer than the timeout, so a tool that is offline exits promptly. Data it could not send is kept (within the spool caps) and goes out the next time any process of the same host starts.

A stdio protocol server can use the SDK freely: nothing it does touches stdout.

## 9. Consent, the notice, and "forget me"

Recording is on by default (opt-out), with three independent categories: `Crash`, `Performance` and `Usage`. **Nothing is sent until the user has seen your first-run notice.** The SDK records from the first moment and keeps the data on disk, but it makes no request until you call `notice_shown()` once. That call is remembered, so later runs send from the start. The host owns the user interface:

```rust
if self.tracer.notice_pending() {
    // Show your first-run notice, with the three switches if you like. When the user closes it:
    self.tracer.notice_shown();
}

// Settings page: one switch per category.
let mut allowed = self.tracer.consent(ConsentCategory::Usage);
if ui.checkbox(&mut allowed, "Usage").changed() {
    self.tracer.set_consent(ConsentCategory::Usage, allowed);
}

// "Forget me".
self.tracer.forget();
```

- If you never call `notice_shown()`, no data ever arrives. The SDK says so once through the `log` facade (a warning with target `tracer_sdk`), and `flush_blocking` returns false while data is waiting for the notice. A tool without a user interface of its own that shares the data directory with your app needs nothing: it sends once the app has shown the notice.
- A category the user switches off before closing the notice is deleted from disk and never sent.
- If your notice is shown elsewhere, for example by an installer, build with `.send_before_notice(true)`.
- `set_consent(category, false)` takes effect at once and deletes that category's unsent items from the spool. The choice is saved and survives restarts. Other running processes of the same host pick it up within a tick.
- `forget()` deletes everything stored locally, sends a request asking the server to delete the old install's data (retried on the next launch if the server is unreachable), and issues a new install id.
- `DO_NOT_TRACK` set to anything but empty, `0` or `false`, or `TRACER_DISABLED` set the same way, turns everything off for that process: nothing is written to disk, no folder is created, no request is made, and the consent file is not changed. Document this for your users.

## 10. Inspect mode and the local report

`.inspect(true)` mirrors every item that is accepted for sending into `outbox.jsonl` in the SDK's folder (capped in size), so a user can read exactly what leaves the machine. A "show what is sent" menu item can simply open that file.

`write_report(path)` writes a plain-text report the user can attach to a bug report: app and version, device, session ids and totals, the last performance window and the recent log lines. It works without a server and without sending anything:

```rust
match self.tracer.write_report(&path) {
    Ok(()) => format!("wrote {}", path.display()),
    Err(error) => format!("could not write the report: {error}"),
}
```

## 11. Tests and builds without telemetry

`Tracer::disabled()` is a tracer whose every method does nothing and touches no files or threads. Pass it into code under test, or use it in builds that ship without telemetry:

```rust
let tracer = Tracer::disabled();
assert!(!tracer.is_enabled());
tracer.track(DemoEvent::FileOpen);          // nothing happens
assert!(tracer.flush_blocking(Duration::from_millis(1))); // nothing to send
assert_eq!(tracer.install_id(), "");
```

`demo-cli` runs its whole `normal` scenario against a disabled tracer in a unit test.

To test against a real server, run `tracer-server` in-process on `127.0.0.1:0` with a temporary data directory, as `examples/demo-cli/tests/e2e.rs` does. Set `.configure(|c| ...)` intervals to milliseconds so tests need no long sleeps.

## 12. The web build

On `wasm32` the SDK compiles but does nothing: `Builder::init` always returns a disabled tracer, so the same code path works unchanged and costs nothing. There is no data directory, no thread and no request on the web target. (`cargo xtask wasm` keeps `tracer-proto` and `tracer-sdk` building for it.)

## 13. Cost and limits

Measured with `cargo xtask stress` (release build, a 15-core laptop, a real `Tracer` on a temporary folder and a transport that accepts at once). Run it on your own hardware; the tables it prints are the source of these numbers.

**Per call on a host thread, one thread** (median, 99th percentile):

| Call | Median | p99 | Allocations |
|---|---|---|---|
| `track`, listened to | 35 ns | 42 ns | 0 |
| `track_with`, 4 properties | 44 ns | 60 ns | 1 |
| `track_id` | 38 ns | 42 ns | 0 |
| `track`, not listened to | 10 ns | 15 ns | 0 |
| `frame` | 5 ns | 10 ns | 0 |
| `turn()` or `begin_turn` + `end_turn` | 25 ns | 38 ns | 0 |
| `heartbeat`, `note_input` | 20 ns | 48 ns | 0 |
| `set_focused`, `Counter::inc`, `Gauge::set` | 1–2 ns | 2 ns | 0 |
| a log line kept as a breadcrumb | 88 ns | 187 ns | 4 |
| a log line below the kept level | 3 ns | 7 ns | 0 |
| `report_handled` | 8 µs | 26 µs | many; it is an error path |
| anything on a disabled tracer | 1–2 ns | 2 ns | 0 |

The allocation counts are from reading the code, not from a counting allocator (that needs `unsafe`, which this workspace forbids).

**What a host thread can wait for.** Nothing that touches the disk or the network. `track` is an atomic check, a set lookup and a `try_send` into a queue that was allocated at startup; when the queue is full the event is dropped and counted with one atomic add. `frame`, the turn calls, `note_input`, counters and gauges are atomic operations only. Over ten million calls of each, with the worker flushing every 20 ms, the longest single call was a few milliseconds, and so was the longest gap in an empty loop that only reads the clock: on a desktop operating system the scheduler can take a thread away at any point, and no library can promise otherwise. What the SDK promises is that it adds no wait of its own.

Calls that do touch the disk, briefly, on the calling thread: `init`, `set_consent`, `notice_shown`, `forget`, and the panic hook. `flush_blocking` and dropping the last `Tracer` wait for the worker, up to the timeout you give and `shutdown_timeout`.

**With many threads at once** the calls stay lock-free but share memory, so they slow each other down: with 8 threads calling the same thing as fast as they can, `track` is about 55 ns, `frame` about 200 ns, a kept log line about 2 µs. Counters and gauges on a realtime thread are one atomic operation on memory nothing else writes to, if you give that thread its own counter.

**Startup and shutdown.** `init` returns in under 1 ms, whether the folder is empty or holds 1,000 session folders left by earlier runs or a full 8 MiB spool; dealing with those is the worker's first job (about 0.4 s of its time for the 1,000 folders). Dropping the last `Tracer` takes about 1 ms when the server answers at once or refuses at once, as long as one request when the server is slow, and never longer than `shutdown_timeout` (3 s by default).

**In the background.** An idle app costs about 0.02% of one core for the worker and the watchdog together. At 120 frames a second with 50 events a second the whole process, pacing loop included, used 0.3% of one core. The worker writes about 500,000 items a second to the spool. With the server unreachable for five minutes at that load, resident memory stayed flat and the spool stayed at its cap.

**Limits**, all enforced, all counted in the `drops` item when they throw something away:

| Limit | Default | When it is reached |
|---|---|---|
| Queue between host threads and the worker | 1,024 items (`queue_capacity`) | The new item is dropped: `queue_full` |
| Spool on disk, all sessions together | 8 MiB (`spool_max_bytes`) | The oldest segments go: `spool_evicted` |
| Age of spooled data | 30 days (`spool_max_age`) | Older segments go: `spool_evicted` |
| One request | 1 MiB before compression | A larger backlog is sent as several requests |
| One item | 256 KiB | Dropped: `item_too_large`; an unclean-exit report that big goes without its log lines |
| Properties on one event | 16 | The rest are left out: `prop_dropped` |
| Event, category and property names | 64 bytes, lower-case `snake_case`, dots allowed | Build error for declared names; `invalid_id` or `prop_dropped` at run time |
| String property values | 64 bytes | Left out: `prop_dropped` |
| Panic and error messages | 2 KiB | Cut, on a character boundary |
| Backtraces | 32 KiB | Cut |
| Log lines kept as breadcrumbs | 100 lines of 512 bytes (`breadcrumb_capacity`) | Oldest lines go; long lines are cut |
| Counters and gauges | 256 names in all | Further names get a handle that records nothing |
| Forget requests still owed | 16 | The oldest is given up |
| Inspect mirror `outbox.jsonl` | 1 MiB | The older half is discarded |

**What a power loss can cost.** Spooled items are not flushed to the disk one by one, so the last seconds before a power loss can be lost; a crash of the app alone loses nothing that was already written. A consent change or a "forget me" is made durable by the worker within a tick (one second by default).

## 14. A short checklist

1. Add the dependency; declare the catalog with `events!`.
2. Build the tracer once at startup, with data dir, endpoint, key, version, process kind.
3. Mark each turn of the main loop with `turn()`, call `frame` around each UI pass, `first_frame` once, and `set_focused` / `note_input` from your input state.
4. Call `track` / `track_id` where user actions are dispatched.
5. Install the SDK's logger or wrap yours.
6. Report handled errors and abnormal child exits.
7. Show the first-run notice and call `notice_shown()` when the user closes it (nothing is sent before that); add the three switches and a "forget me" action.
8. In short-lived tools, end with `flush_blocking`.
9. Update the host's own privacy text from [privacy.md](privacy.md).
