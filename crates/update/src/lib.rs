//! Update checks for desktop apps.
//!
//! An app asks a releases API for its latest build (`GET {base}/v1/releases/{app}/latest`, a JSON
//! [`Release`]), compares its version with the running one by semantic-versioning order, and tells
//! the person when a newer one exists. Nothing is downloaded or installed here: the app opens the
//! release's link ([`Release::download_url`]) in the browser.
//!
//! | Piece | What it does |
//! |---|---|
//! | [`Version`] | `MAJOR.MINOR.PATCH[-pre][+build]`, ordered as semver.org orders it |
//! | [`Release`], [`Outcome`] | the API's answer (tolerant of added or missing fields) and "newer or not" |
//! | [`check`] | one blocking check through a [`Fetch`] (a command-line `update` verb) |
//! | [`HttpsFetch`] | ureq + rustls (ring provider, OS verifier), feature `https` |
//! | [`Updater`] | the app's background checker: first check [`FIRST_DELAY`] after launch, then every [`INTERVAL`] while "check automatically" is on, [`RETRY`] after a failure, at once on [`Updater::check_now`]; last result and "later" kept in a state file |
//!
//! Failures (offline, server down, a bad answer) are never shown on their own: an automatic check
//! that fails is logged and tried again later; only a check the person asked for reports one
//! ([`Finished::Failed`]). The checker never blocks the UI thread: it runs on its own thread and
//! hands back a [`Status`] snapshot and a waker call.

mod fetch;
mod release;
mod saved;
mod updater;
mod version;

#[cfg(all(feature = "https", not(target_arch = "wasm32")))]
pub use fetch::HttpsFetch;
pub use fetch::{Fetch, Response, app_id_ok, base_allowed, check, latest_url};
pub use release::{MAX_BODY, MAX_NOTES, MAX_URL, Outcome, Release, is_safe_url};
pub use saved::Saved;
pub use updater::{FIRST_DELAY, Finished, INTERVAL, RETRY, Status, UpdateConfig, Updater, next_due};
pub use version::Version;

/// Why a check failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// No base URL, or one that isn't `https://`; or a bad app id.
    #[error("update checks aren't set up: {0}")]
    Config(String),
    /// Offline, DNS, TLS, timeout.
    #[error("couldn't reach the update server: {0}")]
    Network(String),
    /// The server answered with an error status.
    #[error("the update server answered HTTP {0}")]
    Status(u16),
    /// The answer isn't a release.
    #[error("the update server's answer isn't a release: {0}")]
    Body(String),
}
