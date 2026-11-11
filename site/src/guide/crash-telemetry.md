# Crash safety and telemetry

## What the runtime catches

- **A panic in the view** is caught. The window stays open and shows the error in place of the
  view; the document and undo history are untouched.
- **A panic in a command** fails that command with `CommandError::Panicked`. The document is left as
  it was before the command, and the user sees an error toast.
- **A panic in a job** fails the job. Nothing else is affected.
- **A panic in a scene or in `setup`** is logged and skipped.

Every panic is written to a crash log in the app's `logs` folder by a panic hook that chains to any
hook installed before it.

## When the process dies anyway

A crash outside these guards, a kill or a power cut ends the process. Two things limit the damage:

- **Recovery.** Autosave keeps snapshots of unsaved work. On the next launch a dialog offers to
  recover them. See [Documents and undo](documents.md#autosave-and-recovery).
- **Safe mode.** If the previous launch died before its first frame, the app starts without
  restoring view settings (theme, window state) and says so. `--safe-mode` does the same by hand.

## Logs

The runtime logs to stderr, a rotating file in the `logs` folder, and an in-memory ring that
`app.logs` (control port) and the MCP tool `app_logs` read. Filter with `<APP>_LOG` or `RUST_LOG`
(`NOTES_LOG=warn,notes=debug` for an app named `notes`). Nothing is written to stdout, so a CLI or
an MCP server can log freely.

## In your own code

The library code follows the rules it enforces on itself, and apps can adopt them:

- Deny `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!` and `unimplemented!` outside tests:

  ```rust,ignore
  #![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
  ```

- Treat numbers from input as hostile: `get()` instead of `[i]`, checked or saturating math.
- Return `CommandError::failed` or `bad_params` from commands instead of panicking.

## Telemetry

With the `telemetry` feature, the app can send crash, performance and usage reports to a server you
host (`ashlar-trace-server`):

```sh
cargo add ashlar-rs --features telemetry
```

```rust,ignore
App::<Doc>::new("notes", env!("CARGO_PKG_VERSION"))
    .telemetry("app-write-key", Some("https://telemetry.example.com"))
```

When an endpoint is set, telemetry is on. The runtime reports:

- panics, with recent log lines;
- each command run (id, success, duration), from the command chokepoint;
- frame times and startup time;
- hangs, when a main-loop turn takes too long;
- saves.

In this integration every category is on and there is no first-run notice; `DO_NOT_TRACK` is
ignored. `TRACER_DISABLED` in the environment (meant for developer machines and CI) turns it off.
The endpoint must be `https://` (or `http://` to loopback). Command parameters, file names, paths
and typed text are never sent. The write key only allows sending data; it is not a secret. Without
the feature, every telemetry call compiles to nothing.

Because users cannot switch it off from the app, tell them in your privacy policy what is sent.
An app that needs consent and per-category switches can use the `ashlar-trace` SDK directly.

Before you ship telemetry, read what is collected:

- [Privacy and collected fields](../docs/trace/privacy.md)
- [Self-hosting the server](../docs/trace/self-hosting.md)
- [Integrating the SDK](../docs/trace/integration.md), for apps that use `ashlar-trace` directly
