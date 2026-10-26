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
| `wasm` | `cargo check --target wasm32-unknown-unknown` on the rank 0–1 crates |

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

Tools needed locally: stable Rust with `rustfmt`, `clippy` and the `wasm32-unknown-unknown`
target; cargo-deny is optional locally and installed on CI.

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
