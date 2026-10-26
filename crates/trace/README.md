# ashlar-trace

The telemetry SDK: crash, performance and usage telemetry for desktop apps, sent to a server you
host yourself ([`ashlar-trace-server`](https://github.com/Himasnhu-AT/ashlar/tree/main/crates/trace-server)).

The host decides what is tracked: it declares its events with `events!`, picks what to listen to on
the `Builder`, and feeds the SDK from its own loop. Nothing here depends on a UI toolkit or an async
runtime, and no call on a `Tracer` blocks on the network.

See the [integration guide](https://github.com/Himasnhu-AT/ashlar/blob/main/docs/trace/integration.md) and the
[privacy notes](https://github.com/Himasnhu-AT/ashlar/blob/main/docs/trace/privacy.md), which list every field that is collected.

Part of [ashlar](https://github.com/Himasnhu-AT/ashlar), a lightweight application framework for Rust powered by egui.
Licensed under the [BSD 3-Clause License](https://github.com/Himasnhu-AT/ashlar/blob/main/LICENSE).
