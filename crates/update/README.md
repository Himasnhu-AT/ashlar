# ashlar-update

Update checks for desktop apps: ask a releases API for the app's latest build, compare it with the
running version, and tell the person when a newer one exists. Nothing is downloaded or installed:
the app opens the release's link in the browser.

The API: `GET {base}/v1/releases/{app}/latest` answers one release
(`{app, version, published_at, url, sha256, size, notes, mirror}`; only `version` is required) or
404 when the app has none yet.

| Item | What it does |
|---|---|
| `Version` | `MAJOR.MINOR.PATCH[-pre][+build]` (a leading `v` allowed), semver.org order |
| `Release::parse`, `Outcome::of` | the answer, tolerant of added or missing fields; body capped at 256 KiB, notes at 64 KiB; "newer or not" |
| `Release::download_url(fallback)` | the mirror, else the release link, else the fallback page; `https://` only |
| `Release::backup_url()` | the release link when `download_url` picked the mirror (to show as a second source) |
| `check(fetch, base, app, current)` | one blocking check (a command-line `update` verb). Refuses a non-https base (loopback `http://` allowed) and a bad app id before any request |
| `HttpsFetch` | feature `https` (default): ureq + rustls with the ring provider and the OS verifier, no redirects, 20 s timeout |
| `Updater` | the app's background checker (below) |

## Updater

```rust
let config = UpdateConfig::new("my-app", env!("CARGO_PKG_VERSION"), base_url)
    .state_file(data_dir.join("updates.json"))
    .fallback_download("https://example.com/download");
let updater = Updater::start(config, Box::new(HttpsFetch::new("my-app/1.2.3")), settings.check_automatically);
updater.set_waker(move || ctx.request_repaint());
// each frame
if let Some(release) = updater.status().available() { /* show the notice */ }
if let Some(done) = updater.take_finished() { /* answer Check for Updates… */ }
```

- One worker thread. First automatic check `FIRST_DELAY` (15 s) after launch, then every `INTERVAL`
  (12 h) while the app runs, `RETRY` (1 h) after a failure. `set_automatic(false)` stops automatic
  checks; `check_now()` checks at once either way.
- Failures are silent: logged, kept in `Status::error`, retried later. Only a check the person asked
  for reports one (`Finished::Failed`).
- `dismiss(version)` ("later") hides that version, also after a restart; a newer one shows again,
  and `check_now()` undoes it.
- The state file holds the last result and the dismissal, so the notice is up at launch before the
  network answers. A missing or corrupt file is an empty state. The UI thread never touches it.
- A running version that isn't semver (a development build) is never offered an update.

Tests use fixture answers in `tests/fixtures/` through a closure fetcher: no network.
