//! Update checks against fixture answers: no network, a closure stands in for HTTP.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ashlar_update::{Finished, Outcome, Release, Response, Saved, UpdateConfig, UpdateError, Updater, check};

const BASE: &str = "https://api.example.com";
const APP: &str = "my-app";

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// A fetcher that answers `status` with fixture `name` and records the URLs asked for.
fn answering(status: u16, name: &'static str, urls: Arc<Mutex<Vec<String>>>) -> impl FnMut(&str) -> Result<Response, String> + Send {
    move |url: &str| {
        urls.lock().unwrap().push(url.to_string());
        Ok(Response { status, body: fixture(name) })
    }
}

fn once(status: u16, name: &'static str, current: &str) -> Result<Outcome, UpdateError> {
    let mut fetch = answering(status, name, Arc::default());
    check(&mut fetch, BASE, APP, current)
}

#[test]
fn a_newer_release_is_available_with_its_fields() {
    let urls = Arc::new(Mutex::new(Vec::new()));
    let mut fetch = answering(200, "latest.json", Arc::clone(&urls));
    let Outcome::Available(release) = check(&mut fetch, "https://api.example.com/", APP, "1.3.2").unwrap() else { panic!("expected an update") };
    assert_eq!(*urls.lock().unwrap(), [ashlar_update::latest_url_for("https://api.example.com", "my-app", ashlar_update::PLATFORM)]);
    assert_eq!(release.version, "1.4.0");
    assert_eq!(release.size, 123_456_789);
    assert!(release.notes.contains("Faster export"));
    assert_eq!(
        release.download_url("https://example.com/download").as_deref(),
        Some("https://mirror.example.com/my-app-1.4.0.dmg"),
        "the mirror first"
    );
    assert_eq!(release.backup_url().as_deref(), Some("https://downloads.example.com/my-app-1.4.0-macos-universal.dmg"));
}

#[test]
fn same_or_newer_running_version_is_up_to_date() {
    for current in ["1.4.0", "1.4.1", "2.0.0", "1.4.0+local"] {
        let outcome = once(200, "latest.json", current).unwrap();
        assert!(matches!(outcome, Outcome::UpToDate { latest: Some(ref r) } if r.version == "1.4.0"), "{current}: {outcome:?}");
    }
    // a pre-release of the same version is older than the release
    assert!(matches!(once(200, "latest.json", "1.4.0-rc.3").unwrap(), Outcome::Available(_)));
    // a development build that isn't semver is never nagged
    assert!(matches!(once(200, "latest.json", "dev").unwrap(), Outcome::UpToDate { .. }));
}

#[test]
fn unknown_fields_and_missing_fields_are_tolerated() {
    let Outcome::Available(r) = once(200, "extra-fields.json", "1.9.0").unwrap() else { panic!() };
    assert_eq!(r.version, "2.0.0-beta.2");
    assert_eq!(r.mirror, None);
    let Outcome::Available(r) = once(200, "minimal.json", "1.0.0").unwrap() else { panic!() };
    assert_eq!(r.url, "");
    assert_eq!(r.download_url("https://example.com/download").as_deref(), Some("https://example.com/download"));
}

#[test]
fn unsafe_links_fall_through_to_the_fallback() {
    let Outcome::Available(r) = once(200, "unsafe-links.json", "1.0.0").unwrap() else { panic!() };
    assert_eq!(r.download_url("https://example.com/download").as_deref(), Some("https://example.com/download"));
}

#[test]
fn not_found_means_no_release_yet() {
    assert_eq!(once(404, "not-found.json", "1.0.0").unwrap(), Outcome::UpToDate { latest: None });
}

#[test]
fn bad_answers_are_errors_not_updates() {
    assert!(matches!(once(200, "bad-version.json", "1.0.0"), Err(UpdateError::Body(_))));
    assert!(matches!(once(200, "gateway.html", "1.0.0"), Err(UpdateError::Body(_))));
    assert!(matches!(once(200, "other-app.json", "1.0.0"), Err(UpdateError::Body(_))));
    assert_eq!(once(502, "gateway.html", "1.0.0"), Err(UpdateError::Status(502)));
    assert_eq!(once(503, "not-found.json", "1.0.0"), Err(UpdateError::Status(503)));
}

#[test]
fn offline_is_a_network_error() {
    let mut fetch = |_: &str| -> Result<Response, String> { Err("dns error: no such host".into()) };
    assert!(matches!(check(&mut fetch, BASE, APP, "1.0.0"), Err(UpdateError::Network(_))));
}

#[test]
fn plain_http_and_bad_app_ids_are_refused_before_any_request() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let mut fetch = move |_: &str| -> Result<Response, String> {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(Response { status: 200, body: fixture("latest.json") })
    };
    assert!(matches!(check(&mut fetch, "http://api.example.com", APP, "1.0.0"), Err(UpdateError::Config(_))));
    assert!(matches!(check(&mut fetch, "", APP, "1.0.0"), Err(UpdateError::Config(_))));
    assert!(matches!(check(&mut fetch, BASE, "../admin", "1.0.0"), Err(UpdateError::Config(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // loopback is allowed for a local test server
    assert!(check(&mut fetch, "http://127.0.0.1:8090", APP, "1.0.0").is_ok());
}

// --- the background checker ---

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn config(dir: &Path, first_delay: Duration) -> UpdateConfig {
    UpdateConfig::new(APP, "1.3.2", BASE).state_file(dir.join("updates.json")).fallback_download("https://example.com/download").timings(
        first_delay,
        Duration::from_secs(3600),
        Duration::from_secs(3600),
    )
}

fn counting(name: &'static str, calls: Arc<AtomicUsize>) -> Box<dyn ashlar_update::Fetch> {
    Box::new(move |_: &str| -> Result<Response, String> {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Response { status: 200, body: fixture(name) })
    })
}

fn state_file(dir: &Path) -> PathBuf {
    dir.join("updates.json")
}

#[test]
fn checks_automatically_after_the_first_delay_and_saves_the_result() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let wakes = Arc::new(AtomicUsize::new(0));
    let updater = Updater::start(config(dir.path(), Duration::from_millis(100)), counting("latest.json", Arc::clone(&calls)), true);
    let woken = Arc::clone(&wakes);
    updater.set_waker(move || {
        woken.fetch_add(1, Ordering::SeqCst);
    });
    wait_until("the automatic check", || updater.available().is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(wakes.load(Ordering::SeqCst) >= 1);
    // an automatic check never produces a "finished" answer
    assert_eq!(updater.take_finished(), None);
    let status = updater.status();
    assert!(status.checked_at.is_some());
    assert_eq!(status.error, None);
    wait_until("the state file", || Saved::load(&state_file(dir.path())).latest.is_some());
    assert_eq!(Saved::load(&state_file(dir.path())).latest.unwrap().version, "1.4.0");
    // the next check is 12 h away: nothing more happens
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn automatic_off_never_checks_until_asked() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let updater = Updater::start(config(dir.path(), Duration::ZERO), counting("latest.json", Arc::clone(&calls)), false);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!updater.automatic());
    updater.check_now();
    wait_until("the manual check", || updater.status().checked_at.is_some());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(updater.take_finished(), Some(Finished::Available(r)) if r.version == "1.4.0"));
    assert_eq!(updater.take_finished(), None, "taken once");
    // turning automatic on with a fresh result waits a full interval
    updater.set_automatic(true);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn a_manual_check_says_up_to_date_or_failed() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path(), Duration::ZERO);
    cfg.current_version = "1.4.0".into();
    let updater = Updater::start(cfg, counting("latest.json", Arc::default()), false);
    updater.check_now();
    wait_until("up to date", || updater.status().checked_at.is_some());
    assert_eq!(updater.take_finished(), Some(Finished::UpToDate));
    assert_eq!(updater.available(), None);

    let offline: Box<dyn ashlar_update::Fetch> = Box::new(|_: &str| -> Result<Response, String> { Err("offline".into()) });
    let updater = Updater::start(config(dir.path(), Duration::ZERO), offline, false);
    updater.check_now();
    wait_until("a failure", || updater.status().error.is_some());
    assert!(matches!(updater.take_finished(), Some(Finished::Failed(_))));
}

#[test]
fn later_hides_that_version_across_restarts_but_not_a_newer_one() {
    let dir = tempfile::tempdir().unwrap();
    {
        let updater = Updater::start(config(dir.path(), Duration::ZERO), counting("latest.json", Arc::default()), true);
        wait_until("the check", || updater.available().is_some());
        updater.dismiss("1.4.0");
        assert_eq!(updater.available(), None);
        assert!(updater.status().newer().is_some(), "still known, only hidden");
        wait_until("the dismissal saved", || Saved::load(&state_file(dir.path())).dismissed.as_deref() == Some("1.4.0"));
    }
    // a restart reads the cached result and the dismissal before any network
    let never: Box<dyn ashlar_update::Fetch> = Box::new(|_: &str| -> Result<Response, String> { Err("offline".into()) });
    let updater = Updater::start(config(dir.path(), Duration::from_secs(3600)), never, true);
    wait_until("the cache", || updater.status().latest.is_some());
    assert_eq!(updater.available(), None);
    // a newer release shows again
    let newer = Release::parse(&fixture("extra-fields.json")).unwrap();
    updater.apply(Ok(Outcome::Available(newer)), false);
    assert_eq!(updater.available().unwrap().version, "2.0.0-beta.2");
    // and "check now" brings back a dismissed one
    updater.dismiss("2.0.0-beta.2");
    assert_eq!(updater.available(), None);
    updater.check_now();
    assert_eq!(updater.available().unwrap().version, "2.0.0-beta.2");
}

#[test]
fn a_cached_release_older_than_the_running_version_is_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    let saved = Saved { checked_at: 1, latest: Some(Release::parse(&fixture("latest.json")).unwrap()), dismissed: None };
    saved.store(&state_file(dir.path()));
    let mut cfg = config(dir.path(), Duration::from_secs(3600));
    cfg.current_version = "1.5.0".into();
    let updater = Updater::start(cfg, counting("latest.json", Arc::default()), true);
    wait_until("the cache", || updater.status().latest.is_some());
    assert_eq!(updater.available(), None);
}

#[test]
fn a_panicking_fetcher_is_a_failure_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let boom: Box<dyn ashlar_update::Fetch> = Box::new(|_: &str| -> Result<Response, String> { panic!("boom") });
    let updater = Updater::start(config(dir.path(), Duration::ZERO), boom, false);
    updater.check_now();
    wait_until("the failure", || updater.status().error.is_some());
    assert!(matches!(updater.take_finished(), Some(Finished::Failed(_))));
    // the worker is still alive
    updater.check_now();
    wait_until("a second answer", || updater.take_finished().is_some());
}

#[test]
fn off_never_fetches() {
    let updater = Updater::off(UpdateConfig::new(APP, "1.0.0", ""));
    updater.check_now();
    updater.dismiss("1.0.0");
    assert_eq!(updater.available(), None);
    assert_eq!(updater.status().checked_at, None);
}
