# Contributing to ashlar

Thanks for your interest in ashlar. Bug reports, fixes, docs, examples and new building blocks are
all welcome.

## Before you start

- **Bugs:** search the [issues](https://github.com/Himasnhu-AT/ashlar/issues) first, then open
  one with the bug report template.
- **Features and new crates:** open an issue to discuss the idea before you write a lot of code.
  ashlar holds only general building blocks that serve more than one kind of app. Widgets for one
  domain (a timeline, a CAD canvas) belong in the app that needs them.
- **Security issues:** don't open a public issue. See [SECURITY.md](SECURITY.md).

## Development setup

1. Install stable Rust (1.95 or newer) with `rustfmt`, `clippy` and the wasm target:
   ```sh
   rustup component add rustfmt clippy
   rustup target add wasm32-unknown-unknown
   ```
2. On Linux, install the system libraries:
   ```sh
   sudo apt-get install libgtk-3-dev libxdo-dev libxkbcommon-dev libdbus-1-dev
   ```
3. Optional: `cargo install cargo-deny --locked` for the dependency checks.
4. Run the gate:
   ```sh
   cargo xtask ci
   ```

## The rules

[AGENTS.md](AGENTS.md) holds the project's rules. They apply to people and AI agents alike, and
CI enforces most of them. In short:

- **Never crash.** No `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!` or `unimplemented!`
  outside tests, and no `unsafe`. Treat every input-derived number as hostile.
- **Respect the layers.** A crate depends only on crates of a lower rank
  ([docs/architecture.md](docs/architecture.md)). The headless core never depends on egui.
- **Performance is a feature.** Idle apps do no work. Hot paths have benchmarks.
- **No app-specific code.** Describe apps by kind ("a video editor"), not by name.

The full list of gates, and how to run each one alone, is in
[docs/contributing.md](docs/contributing.md).

## Pull requests

1. Fork the repository and create a branch from `main`.
2. Keep each pull request to one change, with tests for new behaviour and bug fixes.
3. Run `cargo xtask ci` and make sure it passes.
4. Update the docs and the crate's README when you change public behaviour, and add a line to
   [CHANGELOG.md](CHANGELOG.md) under "Unreleased".
5. Write commit subjects in the imperative mood, under about 72 characters. Use the body to explain
   why.
6. Open the pull request and fill in the template.

## License

By contributing, you agree that your contributions are licensed under the
[BSD 3-Clause License](LICENSE), the same license as the project.
