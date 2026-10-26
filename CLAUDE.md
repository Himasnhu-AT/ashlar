Read AGENTS.md first; its rules override everything.

## Releases and crates.io

Publishing is irreversible: a version on crates.io can never be replaced. The process is in
docs/releasing.md.

- Never publish, create a `v*` tag or push one unless the user asks for a release in this
  conversation, naming the version. "Commit" or "push" alone is not a request to release.
- The only publish path is pushing a `vX.Y.Z` tag, which runs `.github/workflows/release.yml`. It
  publishes only after its guards pass: the tag matches the workspace version, the tagged commit
  is on `main`, the full CI gate passes on that commit, CHANGELOG.md has a dated section for the
  version, and the version is not on crates.io yet. Running the workflow by hand is a dry run.
- Never run `cargo publish` without `--dry-run` from a machine unless the user explicitly says to
  publish from this machine. The crates.io token lives only in the repository secret
  `CARGO_REGISTRY_TOKEN`; never ask for it, print it or store it in the repository.
- Before tagging: everything for the release is merged into `main`, CI on that commit is green,
  `cargo xtask ci` and `cargo publish --workspace --locked --dry-run` pass, the workspace version and
  `[workspace.dependencies]` versions are bumped together, and CHANGELOG.md moves "Unreleased" to
  `## [x.y.z] - YYYY-MM-DD`.
- Keep CHANGELOG.md current: every user-visible change adds a line under "Unreleased" in the same
  commit.
