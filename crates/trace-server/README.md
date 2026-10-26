# ashlar-trace-server

The self-hosted backend for `ashlar-trace`: ingest API, SQLite storage, rollups, a dashboard and
release reports in one binary.

```sh
cargo install ashlar-trace-server
```

Configuration comes from `TRACER_*` environment variables. See
[self-hosting](https://github.com/Himasnhu-AT/ashlar/blob/main/docs/trace/self-hosting.md) for setup, including the Docker image in
[`deploy/trace`](https://github.com/Himasnhu-AT/ashlar/tree/main/deploy/trace).

Part of [ashlar](https://github.com/Himasnhu-AT/ashlar), a lightweight application framework for Rust powered by egui.
Licensed under the [BSD 3-Clause License](https://github.com/Himasnhu-AT/ashlar/blob/main/LICENSE).
