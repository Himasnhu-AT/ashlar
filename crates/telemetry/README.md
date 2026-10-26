# ashlar-telemetry

Always-on crash, performance and usage telemetry to a self-hosted ashlar-trace server, shared by every
app built on ashlar.

- **On whenever an endpoint is configured.** Every category is allowed and there is no notice
  (tracer's `force_consent`); `DO_NOT_TRACK` is ignored. `TRACER_DISABLED` (a switch for developer
  machines and CI) turns it off. No endpoint, a bad one (`https://` only, `http://` to loopback),
  no write key, no data folder or the web: a disabled client, no requests, nothing on disk.
- **Its own transport:** ureq + rustls + the ring provider + the OS certificate verifier
  (no `ring`, no bundled roots), no redirects.
- **Never sent:** command parameters, file names, paths, typed text. Commands are reported by
  their registered id; log lines are redacted before they are kept as crash breadcrumbs.

| Hook | Where |
|---|---|
| Panics | the SDK's hook, chained to the app's own (start the client first) |
| Log breadcrumbs | `Telemetry::wrap_logger` + `ashlar_log::logging::install_wrapped` |
| Commands (id, ok, ms; rapid repeats folded) | `Journal::set_observer(telemetry.command_observer())` |
| Saves | `TelemetryConfig::save_commands`, or `Telemetry::saved` |
| Frames, first frame, focus, input, hang watchdog | `ashlar_ui::telemetry::FrameProbe` (feature `telemetry`), or `turn` (or `begin_turn` / `end_turn`) / `frame` / `input` |
| Native dialogs | `Telemetry::pause_hangs_in_dialogs` (`ashlar_native::modal`), or `paused(|| …)` around any other blocking call |
| Quit | `Telemetry::quit` (at most 1.5 s) |
| Privacy page | `Telemetry::status`; `ashlar_ui::telemetry::privacy_section` with the app's own words |

```rust
let cfg = TelemetryConfig::new("my-app", env!("CARGO_PKG_VERSION"))
    .endpoint(ENDPOINT, WRITE_KEY)
    .env_overrides("MY_APP_TELEMETRY_URL", "MY_APP_TELEMETRY_KEY")
    .save_commands(&["file.save"]);
let telemetry = Telemetry::start(&cfg, Some(&data_dir));
ashlar_log::logging::install_wrapped(Box::new(logger), level, |l| telemetry.wrap_logger(l));
telemetry.pause_hangs_in_dialogs();
session.journal.set_observer(telemetry.command_observer());
```

Tests: feature `testing` gives `testing::Recorder` (a transport that keeps every envelope, decoded)
and `testing::client` (a client on it with millisecond intervals). Cost: about 65 ns per frame on,
5 ns off (`frame_hooks_cost` in `tests/hooks.rs`).
