# Contributing

Read [AGENTS.md](../AGENTS.md) first; its rules apply to people and agents alike. The crate ranks
and design principles are in [architecture.md](architecture.md).

## Gates

`cargo xtask ci` must pass before every commit. It runs:

| Step | What it checks |
|---|---|
| `fmt` | `cargo fmt --all --check` |
| `names` | no app name in any file of the repository (false positives: `xtask/names-allow.txt`) |
| `layers` | each crate depends only on lower ranks; ranks 0–4 never on a UI crate |
| `deny-panics` | every crate inherits the workspace lints; no never-crash lint is allowed away outside tests (the OS FFI crate follows docs/adr/0001-native-ffi.md instead) |
| deps | `cargo deny check bans sources` (skipped with a note when cargo-deny isn't installed) |
| `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `test` | `cargo test --workspace` |
| `wasm` | `cargo check --target wasm32-unknown-unknown` on the rank 0–1 crates, `ashlar-ui`, `ashlar-app` and `examples/web-hello` |

The quick checks run first and are all reported; then clippy, wasm and the tests run side by side
(clippy and wasm in their own target dirs under `target/`, their output shown only on failure). If
another checkout is compiling, `ci` waits for it so two gates don't fight over the cores;
`--no-wait` skips that. Every step also runs on its own: `cargo xtask names`, `cargo xtask wasm`…

Other tasks:

- `cargo xtask msrv`: `cargo check` on the workspace `rust-version` (1.95);
  `rustup toolchain install 1.95 --profile minimal` first.
- `cargo xtask advisories`: cargo-deny on advisories, bans and sources (`cargo install cargo-deny --locked`).
- `cargo xtask bench [--compare FILE]`: the performance gate, see architecture.md.

CI (`.github/workflows/ci.yml`) runs `cargo xtask ci` on Linux, macOS and Windows, `cargo xtask
msrv`, and the advisory check. Actions are pinned by commit SHA; update them together.

Releases (crates.io and GitHub) are in [releasing.md](releasing.md).

Tools needed locally: rustup. `rust-toolchain.toml` pins the Rust version CI uses, with `rustfmt`,
`clippy` and the `wasm32-unknown-unknown` target, and rustup installs it on first use. Bump the pin
on purpose in its own commit, after `cargo xtask ci` passes on the new version. cargo-deny is
optional locally and installed on CI.

## Linux and Windows before pushing

`cargo xtask ci` on a Mac checks macOS only. CI also runs it on Linux and Windows, and runs on
every pull request, so work on a branch and merge only when all three gates are green. To catch
problems earlier:

- Windows compile and lint errors: `cargo xwin clippy --workspace --all-targets --target
  x86_64-pc-windows-msvc -- -D warnings` (`cargo install cargo-xwin --locked`). It does not run
  the tests.
- Linux, the whole gate, in Docker:
  `docker run --rm -v "$PWD":/w -w /w rust:1.99.1 sh -c 'apt-get update && apt-get install -y
  libgtk-3-dev libxdo-dev libxkbcommon-dev libdbus-1-dev && rustup component add rustfmt clippy
  && rustup target add wasm32-unknown-unknown && CARGO_TARGET_DIR=target/linux cargo xtask ci --no-wait'`.
- Tests that depend on the OS (timers, memory, key names, line endings) are the usual Linux and
  Windows failures: check their assumptions on all three before relying on a number.

## Commits

- One task per commit, with its id: `AS-P1.2: history merge keys`.
- Only green states: run `cargo xtask ci` (or, while working on one crate in a parallel worktree,
  `cargo test -p <crate>`, `cargo clippy -p <crate> --all-targets -- -D warnings` and
  `cargo xtask layers`; the full gate runs at integration).
- Imperative subject under ~72 characters; the body says why.
- AI-assisted commits end with the co-author line, e.g.
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Parallel work

One git worktree per contributor under `.claude/worktrees/<name>` (ignored by git), branch
`wt/<name>`, with its own `CARGO_TARGET_DIR=$PWD/target/agent-<name>`; remove both once merged. Never commit on `main`; the integrator merges.
