# ashlar documentation site

The source of https://ashlar.himanshuat.com, built with [mdBook](https://rust-lang.github.io/mdBook/)
0.5.4.

- `src/introduction.md` and `src/guide/`: the user guide (building apps with ashlar), written here.
- `src/docs/`, `src/CONTRIBUTING.md`, `src/AGENTS.md`, `src/SECURITY.md`, `src/CHANGELOG.md`: the
  developer guide and changelog. Each is one `{{#include …}}` line that pulls in the file of the same
  name from the repository (`docs/`, the root), so the site always shows the current text. Edit
  those files, not the wrappers. The wrappers mirror the folder layout of `docs/`, so relative links
  between the docs keep working.
- `src/img/`: screenshots rendered headless from the gallery example
  (`cargo run -p ashlar-example-gallery -- --snapshot <dir>`, the `*-light.png` files).
- `llms.sh`: run after `mdbook build`; writes `book/llms.txt`, `book/llms-full.txt` and a copy of
  every page's markdown next to its HTML (`book/guide/commands.md`).

## Build locally

Install mdBook 0.5.4 (`cargo install mdbook --version 0.5.4 --locked`, or a prebuilt binary from
https://github.com/rust-lang/mdBook/releases), then from the repository root:

```sh
mdbook serve site          # http://localhost:3000, rebuilds on change
mdbook build site && (cd site && sh llms.sh)   # what Vercel runs; output in site/book/
```

`mdbook build` should print no warnings.

## Deploy on Vercel

1. In Vercel, **Add New → Project** and import the GitHub repository `Himasnhu-AT/ashlar`.
2. Set **Root Directory** to `site` and **Framework Preset** to **Other**. Leave the build and
   output settings alone: `site/vercel.json` sets them. Its build command downloads the pinned
   mdBook release for `x86_64-unknown-linux-gnu`, runs `./mdbook build` and `sh llms.sh`, and
   serves `book/` with clean URLs.
3. Keep **Settings → Build and Deployment → Include files outside the Root Directory in the Build
   Step** enabled (the default). The include lines reach `../../docs` and `../../CHANGELOG.md`,
   outside `site/`; with that setting off the build fails.
4. **Settings → Domains**: add `ashlar.himanshuat.com`.
5. At the DNS provider for `himanshuat.com`, create a **CNAME** record: name `ashlar`, value
   `cname.vercel-dns.com`. Vercel issues the certificate once the record resolves.

Every push to `main` then redeploys the site; pull requests get preview URLs.

`vercel.json` also redirects links the included docs make to repository files that are not pages
(`/LICENSE`, `/examples/…`, `/crates/…`) to GitHub, and serves `.md` and `.txt` files as text.

To update mdBook, change every `0.5.4` in `vercel.json`'s `buildCommand` and in this
file.
