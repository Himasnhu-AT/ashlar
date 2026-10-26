# ashlar-trace-proto

Wire types shared by the `ashlar-trace` SDK and `ashlar-trace-server`: a versioned JSON envelope
([wire format](https://github.com/Himasnhu-AT/ashlar/blob/main/docs/trace/wire-format.md)).

- Fields are only ever added, never renamed or repurposed. New fields are optional.
- Readers ignore fields they don't know, and map item types they don't know to `ItemBody::Unknown`.
- A breaking change bumps `SCHEMA_VERSION`.

This crate does no I/O and builds for `wasm32-unknown-unknown`.

Part of [ashlar](https://github.com/Himasnhu-AT/ashlar), a lightweight application framework for Rust powered by egui.
Licensed under the [BSD 3-Clause License](https://github.com/Himasnhu-AT/ashlar/blob/main/LICENSE).
