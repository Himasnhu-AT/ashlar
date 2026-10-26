# Releasing

Every crate in `crates/` is published to crates.io with the same version (the workspace
`version` in the root `Cargo.toml`). The umbrella crate is published as **`ashlar-rs`**, because
the `ashlar` name on crates.io belongs to an unrelated project; its library name is still `ashlar`,
so apps write `use ashlar::prelude::*`. Examples and `xtask` are `publish = false`.

## One-time setup

1. On crates.io, create an API token with the `publish-new` and `publish-update` scopes. Pick no
   expiry (or a long one), so it is set once.
2. Store it as the repository secret `CARGO_REGISTRY_TOKEN`:
   ```sh
   gh secret set CARGO_REGISTRY_TOKEN -R Himasnhu-AT/ashlar
   ```
   (paste the token when asked). The token never needs to be on a developer machine.
3. Optional, later: switch to crates.io [trusted publishing](https://crates.io/docs/trusted-publishing)
   and delete the token.

## What triggers a release

Only pushing a tag of the form `vMAJOR.MINOR.PATCH` (for example `v0.2.0`). Nothing else
publishes: pushes to `main`, pull requests and Dependabot never do, and running the `release`
workflow by hand (Actions → release → Run workflow) is always a dry run.

Before anything is uploaded, the workflow refuses to go on unless:

| Guard | Why |
|---|---|
| the tag equals `v` + the workspace version | no mismatched or mistyped tags |
| the tagged commit is on `main` | nothing from an unmerged branch |
| the full CI gate passes on that exact commit (all three OSes, MSRV) | no broken or half-done state |
| `CHANGELOG.md` has a `## [x.y.z] - YYYY-MM-DD` section | every release is documented |
| that version of `ashlar-rs` is not on crates.io yet | no accidental re-release |

Then it runs `cargo publish --workspace --locked` (every crate in dependency order) and creates
the GitHub release with that version's changelog section as the text.

## Cutting a release

1. Make sure `main` is green on CI and everything for the release is merged.
2. Bump `version` in `[workspace.package]` **and** every `version = "…"` of the internal crates in
   `[workspace.dependencies]` (they must match). Run `cargo check` to update `Cargo.lock`.
3. In `CHANGELOG.md`, rename "Unreleased" to `## [x.y.z] - YYYY-MM-DD`, add a fresh empty
   "Unreleased" section, and update the compare links at the bottom.
4. Check that everything packages: `cargo publish --workspace --locked --dry-run`.
5. Commit to `main` and push. Wait for CI to pass.
6. Tag and push the tag:
   ```sh
   git tag -a v0.2.0 -m "ashlar 0.2.0"
   git push origin v0.2.0
   ```
7. Watch the `release` workflow. If a guard fails, nothing was published: fix the cause, delete
   the tag (`git push origin :refs/tags/v0.2.0`, `git tag -d v0.2.0`) and tag again.

If publishing stops halfway (a network error, a rate limit), crates.io already holds the crates
that made it and refuses to take them again. Publish the rest by hand with
`cargo publish -p <crate> --locked`, lowest rank first (the ranks are in
[architecture.md](architecture.md)), then create the GitHub release with `gh release create`.

## Versioning

ashlar follows [Semantic Versioning](https://semver.org). Before 1.0, a minor version (0.x) may
break the API; patch versions never do. The items marked `CONTRACT` in `ashlar-command` change only
additively.
