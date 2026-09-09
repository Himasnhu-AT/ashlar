//! Edge cases and light versions of the stress tests, through the public API. These run in the gate; the
//! heavy versions are in `stress.rs` and run with `cargo xtask stress`.

// This whole file is test code, but the lint only exempts `#[test]` functions, not their helpers.
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use support::{flood_with_consent_and_forget, flood_with_exact_accounting};

#[test]
fn a_flood_from_many_threads_is_accounted_for_exactly() {
    for seed in 1..=3 {
        let tally = flood_with_exact_accounting(seed, 8, 4_000);
        println!("seed {seed}: {tally:?}");
        assert!(tally.delivered_events > 0, "{tally:?}");
    }
}

#[test]
fn a_flood_with_consent_changes_and_forgets_ends_cleanly() {
    for seed in 1..=2 {
        flood_with_consent_and_forget(seed, 8, 4_000);
    }
}

/// Not a test of its own: the part a child process plays in the test below.
#[test]
#[ignore = "run as a child process by several_processes_on_one_data_directory_are_each_reported_once"]
fn edge_child_process() {
    support::child_process();
}

#[test]
fn several_processes_on_one_data_directory_are_each_reported_once() {
    support::many_processes(1, 10, "edge_child_process");
}

// ---- helpers ----

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use support::{AppEvent, Category, Chaos, Rng, TempDir};
use tracer_sdk::proto::{CrashKind, Envelope, ItemBody, SessionStatus};
use tracer_sdk::{Builder, ConsentCategory, Signal, Tracer};

const WAIT: Duration = Duration::from_secs(20);

/// A transport that accepts everything and remembers it.
fn recorder() -> Chaos {
    let chaos = Chaos::new(0);
    chaos.healthy.store(true, Ordering::Relaxed);
    chaos
}

fn builder(dir: &Path, server: &Chaos) -> Builder {
    Tracer::builder("key", "host", "1.0.0")
        .data_dir(dir)
        .endpoint("https://telemetry.example.com")
        .transport(server.clone())
        .send_before_notice(true)
        .listen_all::<AppEvent>()
        .signals(&[Signal::Session, Signal::Usage, Signal::Crash])
        .configure(|config| {
            config.tick_interval = Duration::from_millis(5);
            config.flush_interval = Duration::from_millis(20);
            config.retry_min = Duration::from_millis(5);
            config.retry_max = Duration::from_millis(20);
        })
}

fn envelopes(server: &Chaos) -> Vec<Envelope> {
    server.seen.lock().unwrap().iter().filter(|seen| seen.status == 200).filter_map(|seen| seen.envelope.clone()).collect()
}

fn event_names(server: &Chaos) -> Vec<String> {
    let bodies = envelopes(server).into_iter().flat_map(|envelope| envelope.items).map(|item| item.body);
    bodies.filter_map(|body| if let ItemBody::Event(event) = body { Some(event.name) } else { None }).collect()
}

fn drops(server: &Chaos) -> std::collections::BTreeMap<String, u64> {
    let mut total = std::collections::BTreeMap::new();
    for item in envelopes(server).into_iter().flat_map(|envelope| envelope.items) {
        if let ItemBody::Drops(drops) = item.body {
            for (reason, count) in drops.reasons {
                *total.entry(reason).or_insert(0) += count;
            }
        }
    }
    total
}

/// Starts a tracer, checks that starting was quick, records one event and delivers it. The shape of
/// "whatever state the folder is in, the SDK comes up and works".
fn comes_up_and_works(dir: &Path, what: &str) {
    let server = recorder();
    let started = Instant::now();
    let tracer = builder(dir, &server).init();
    let took = started.elapsed();
    assert!(took < Duration::from_millis(500), "{what}: init took {took:?}");
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT), "{what}: could not flush");
    assert_eq!(event_names(&server).iter().filter(|name| *name == "file.open").count(), 1, "{what}");
    assert_eq!(tracer.install_id().len(), 32, "{what}");
    drop(tracer);
}

/// Leaves a session folder behind as a killed process would, with `events` waiting in it and a marker.
/// Returns its session id.
fn dead_session(dir: &TempDir, events: usize) -> String {
    let offline = Chaos::new(0);
    // Unhealthy with seed 0 would answer at random; a transport that is never asked is simpler.
    let tracer = Tracer::builder("key", "host", "1.0.0")
        .data_dir(dir.path())
        .transport(offline)
        .listen_all::<AppEvent>()
        .signals(&[Signal::Session, Signal::Usage, Signal::Crash])
        .configure(|config| config.tick_interval = Duration::from_millis(5))
        .init();
    for _ in 0..events {
        tracer.track(AppEvent::FileSave);
    }
    assert!(tracer.flush_blocking(WAIT));
    let session = tracer.session_id();
    let folder = dir.spool().join(&session);
    let marker = std::fs::read(folder.join("session.json")).unwrap();
    drop(tracer);
    // The clean end removed the marker and added a session end; put back what a kill would have left.
    for entry in std::fs::read_dir(&folder).unwrap().flatten() {
        let text = std::fs::read_to_string(entry.path()).unwrap_or_default();
        if text.contains(r#""type":"session_end""#) {
            let kept: String = text.lines().filter(|line| !line.contains(r#""type":"session_end""#)).map(|line| format!("{line}\n")).collect();
            std::fs::write(entry.path(), kept).unwrap();
        }
    }
    std::fs::write(folder.join("session.json"), marker).unwrap();
    session
}

fn items_of(server: &Chaos, session: &str) -> Vec<ItemBody> {
    envelopes(server)
        .into_iter()
        .filter(|envelope| envelope.session_id == session)
        .flat_map(|envelope| envelope.items)
        .map(|item| item.body)
        .collect()
}

// ---- a hostile filesystem ----

#[cfg(unix)]
mod unix_only {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Makes a folder read-only or writable. False when a read-only folder can still be written, as with
    /// rights that ignore the mode; the condition cannot be staged then and the test has nothing to say.
    fn read_only(dir: &Path, on: bool) -> bool {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(if on { 0o555 } else { 0o755 })).unwrap();
        !on || std::fs::write(dir.join("probe"), b"").is_err()
    }

    #[test]
    fn a_read_only_data_directory_disables_quietly_and_works_once_it_is_writable() {
        let dir = TempDir::new();
        let server = recorder();
        if !read_only(dir.path(), true) {
            read_only(dir.path(), false);
            return;
        }
        let started = Instant::now();
        let tracer = builder(dir.path(), &server).init();
        tracer.track(AppEvent::FileOpen);
        tracer.set_consent(ConsentCategory::Usage, false);
        tracer.notice_shown();
        tracer.forget();
        assert!(tracer.flush_blocking(WAIT));
        let _ = tracer.write_report(&dir.path().join("report.txt"));
        drop(tracer);
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        assert!(server.seen.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

        // The same path becomes usable later: a tracer started then works normally.
        read_only(dir.path(), false);
        comes_up_and_works(dir.path(), "after the directory became writable");
    }

    #[test]
    fn a_session_folder_that_turns_read_only_loses_items_by_count_and_recovers() {
        let dir = TempDir::new();
        let server = recorder();
        let tracer = builder(dir.path(), &server).init();
        tracer.track(AppEvent::FileOpen);
        assert!(tracer.flush_blocking(WAIT));
        let folder = dir.spool().join(tracer.session_id());
        if !read_only(&folder, true) {
            read_only(&folder, false);
            return;
        }
        let started = Instant::now();
        for _ in 0..20 {
            tracer.track(AppEvent::Undo);
        }
        tracer.flush_blocking(WAIT);
        assert!(started.elapsed() < Duration::from_secs(5), "a host thread stalled: {:?}", started.elapsed());
        read_only(&folder, false);
        tracer.track(AppEvent::FileSave);
        assert!(tracer.flush_blocking(WAIT));
        tracer.track(AppEvent::FileSave);
        assert!(tracer.flush_blocking(WAIT));
        drop(tracer);
        let names = event_names(&server);
        assert_eq!(names.iter().filter(|name| *name == "file.save").count(), 2, "recording resumed: {names:?}");
        let lost = 20 - names.iter().filter(|name| *name == "edit.undo").count() as u64;
        assert_eq!(drops(&server).get("io_error").copied().unwrap_or(0), lost, "every item that could not be written is counted");
        assert!(lost > 0);
    }

    #[test]
    fn links_in_the_spool_are_never_followed_out_of_it() {
        let dir = TempDir::new();
        let outside = TempDir::new();
        let session = dead_session(&dir, 3);
        // An outside folder made to look exactly like a session folder of this install.
        let decoy = outside.path().join("decoy");
        std::fs::create_dir_all(&decoy).unwrap();
        for entry in std::fs::read_dir(dir.spool().join(&session)).unwrap().flatten() {
            std::fs::copy(entry.path(), decoy.join(entry.file_name())).unwrap();
        }
        let secret = outside.path().join("secret.txt");
        std::fs::write(
            &secret,
            "u{\"id\":\"00000000000000000000000000000000\",\"ts\":1,\"type\":\"event\",\"name\":\"secret.name\",\"category\":\"secret\"}\n",
        )
        .unwrap();
        let before = support::disk_bytes(outside.path());

        // A link to that folder in the spool, and links to the outside file where the SDK reads files.
        std::os::unix::fs::symlink(&decoy, dir.spool().join("0123456789abcdef0123456789abcdef")).unwrap();
        let orphan = dir.spool().join(&session);
        std::os::unix::fs::symlink(&secret, orphan.join("00000099.jsonl")).unwrap();
        std::os::unix::fs::symlink(&secret, orphan.join("breadcrumbs.jsonl")).unwrap();
        std::os::unix::fs::symlink(&secret, dir.sdk().join("consent.json")).unwrap();

        let server = recorder();
        let tracer = builder(dir.path(), &server).init();
        assert!(tracer.flush_blocking(WAIT));
        tracer.set_consent(ConsentCategory::Performance, false);
        assert!(tracer.flush_blocking(WAIT));
        drop(tracer);

        let sent = format!("{:?}", envelopes(&server));
        assert!(!sent.contains("secret"), "content from outside the spool was sent");
        assert_eq!(items_of(&server, &session).iter().filter(|body| matches!(body, ItemBody::Event(_))).count(), 3);
        assert!(std::fs::read_to_string(&secret).unwrap().contains("secret.name"), "the outside file was changed");
        assert_eq!(support::disk_bytes(outside.path()), before, "something outside the spool was written or deleted");
        assert!(decoy.join("header.json").exists());
    }

    #[test]
    fn odd_names_in_the_spool_are_ignored_and_real_orphans_still_adopted() {
        let dir = TempDir::new();
        let session = dead_session(&dir, 2);
        let odd: [&[u8]; 6] =
            ["名前".as_bytes(), "🦀🦀".as_bytes(), "e\u{301}\u{202e}tluser".as_bytes(), b"bad-utf8-\xff\xfe", b" leading and trailing ", b"..hidden"];
        for name in odd {
            // Some filesystems refuse names that are not valid UTF-8; those cases are then skipped.
            let folder = dir.spool().join(std::ffi::OsStr::from_bytes(name));
            if std::fs::create_dir_all(&folder).is_err() {
                continue;
            }
            std::fs::write(folder.join("header.json"), b"{\"schema\":1}").unwrap();
            std::fs::write(folder.join("00000000.jsonl"), b"u{}\n").unwrap();
            let _ = std::fs::write(folder.join(std::ffi::OsStr::from_bytes(b"\xff.jsonl")), b"junk");
        }
        let _ = std::fs::write(dir.spool().join(&session).join(std::ffi::OsStr::from_bytes(b"\xf0\x9f.jsonl")), b"junk");
        std::fs::write(dir.spool().join(&session).join("名前.jsonl"), b"junk").unwrap();
        let server = recorder();
        let tracer = builder(dir.path(), &server).init();
        tracer.track(AppEvent::FileOpen);
        assert!(tracer.flush_blocking(WAIT));
        drop(tracer);
        assert_eq!(items_of(&server, &session).iter().filter(|body| matches!(body, ItemBody::Event(_))).count(), 2);
        assert_eq!(event_names(&server).iter().filter(|name| *name == "file.open").count(), 1);
    }
}

#[test]
fn files_where_folders_belong_and_folders_where_files_belong() {
    // A file where the spool folder should be: nothing can be recorded, and nothing breaks.
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.sdk()).unwrap();
    std::fs::write(dir.spool(), b"not a folder").unwrap();
    let server = recorder();
    let started = Instant::now();
    let tracer = builder(dir.path(), &server).init();
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(std::fs::read(dir.spool()).unwrap(), b"not a folder");
    // Once it is out of the way the same directory works.
    std::fs::remove_file(dir.spool()).unwrap();
    comes_up_and_works(dir.path(), "after the file was removed");

    // Folders where the state files should be: they cannot be read or replaced, and the SDK carries on
    // with what it has in memory.
    for name in ["install.json", "consent.json", "selection.json", "install.lock", "outbox.jsonl"] {
        let dir = TempDir::new();
        std::fs::create_dir_all(dir.sdk().join(name).join("inside")).unwrap();
        comes_up_and_works(dir.path(), name);
        assert!(dir.sdk().join(name).join("inside").is_dir(), "{name}: the folder was removed");
        let server = recorder();
        let tracer = builder(dir.path(), &server).inspect(true).init();
        tracer.set_consent(ConsentCategory::Usage, false);
        assert!(!tracer.consent(ConsentCategory::Usage));
        tracer.forget();
        tracer.notice_shown();
        assert!(tracer.flush_blocking(WAIT), "{name}");
    }
}

#[test]
fn the_sdk_folder_deleted_under_a_running_session_is_put_back() {
    let dir = TempDir::new();
    let server = recorder();
    let tracer = builder(dir.path(), &server).init();
    tracer.set_consent(ConsentCategory::Performance, false);
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    let install_id = tracer.install_id();

    // The worker may be writing a file at this very moment, which makes one attempt fail; the folder is
    // gone when an attempt has gone through.
    while std::fs::remove_dir_all(dir.sdk()).is_err() {
        std::thread::sleep(Duration::from_millis(1));
    }
    let started = Instant::now();
    for _ in 0..10 {
        tracer.track(AppEvent::Undo);
    }
    tracer.flush_blocking(WAIT);
    assert!(started.elapsed() < Duration::from_secs(5));
    // After the next pass the folder is back and recording goes on, under the same install id.
    tracer.flush_blocking(WAIT);
    tracer.track(AppEvent::FileSave);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(event_names(&server).iter().filter(|name| *name == "file.save").count(), 1);
    assert_eq!(tracer.install_id(), install_id);
    let folder = dir.spool().join(tracer.session_id());
    let restored = |name: &str| folder.join(name).exists();
    assert!(restored("lock") && restored("header.json"));
    let deadline = Instant::now() + WAIT;
    while !(restored("session.json") && dir.sdk().join("install.json").exists() && dir.sdk().join("consent.json").exists()) {
        assert!(Instant::now() < deadline, "the marker and the state files were not put back");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(tracer);
    // The next run sees the same install and the same choices.
    let tracer = builder(dir.path(), &recorder()).init();
    assert_eq!(tracer.install_id(), install_id);
    assert!(!tracer.consent(ConsentCategory::Performance));
}

#[test]
fn state_files_cut_short_at_every_length_or_filled_with_noise_never_stop_the_sdk() {
    // One real run, to have real files to damage.
    let dir = TempDir::new();
    {
        let tracer = builder(dir.path(), &recorder()).init();
        tracer.set_consent(ConsentCategory::Performance, false);
        tracer.notice_shown();
        tracer.forget();
        assert!(tracer.flush_blocking(WAIT));
    }
    let mut rng = Rng(42);
    for name in ["install.json", "consent.json", "selection.json"] {
        let path = dir.sdk().join(name);
        let whole = std::fs::read(&path).unwrap();
        assert!(whole.len() > 20, "{name}");
        for length in 0..whole.len() {
            std::fs::write(&path, &whole[..length]).unwrap();
            comes_up_and_works(dir.path(), &format!("{name} cut to {length} bytes"));
        }
        for round in 0..25 {
            let noise: Vec<u8> = (0..rng.below(300)).map(|_| rng.next() as u8).collect();
            std::fs::write(&path, &noise).unwrap();
            comes_up_and_works(dir.path(), &format!("{name} noise round {round}"));
        }
        // Valid JSON of the wrong shape.
        for text in ["null", "[]", "{}", "0", "\"text\"", "{\"install_id\":5,\"crash\":\"yes\",\"acknowledged\":[]}", "{\"install_id\":\"zz\"}"] {
            std::fs::write(&path, text).unwrap();
            comes_up_and_works(dir.path(), &format!("{name} holding {text}"));
        }
        std::fs::write(&path, &whole).unwrap();
    }
}

#[test]
fn a_dead_sessions_files_cut_short_or_filled_with_noise_never_stop_the_sdk() {
    let mut rng = Rng(7);
    for name in ["header.json", "session.json"] {
        let template = TempDir::new();
        let session = dead_session(&template, 2);
        let whole = std::fs::read(template.spool().join(&session).join(name)).unwrap();
        let mut variants: Vec<Vec<u8>> = (0..whole.len()).step_by(7).map(|length| whole[..length].to_vec()).collect();
        variants.extend((0..15).map(|_| (0..rng.below(400)).map(|_| rng.next() as u8).collect()));
        for (index, variant) in variants.into_iter().enumerate() {
            let dir = TempDir::new();
            let session = dead_session(&dir, 2);
            std::fs::write(dir.spool().join(&session).join(name), &variant).unwrap();
            let server = recorder();
            let tracer = builder(dir.path(), &server).init();
            tracer.track(AppEvent::FileOpen);
            assert!(tracer.flush_blocking(WAIT), "{name} variant {index}");
            drop(tracer);
            // Whatever could still be made out of the dead session is delivered at most once.
            let events = items_of(&server, &session).iter().filter(|body| matches!(body, ItemBody::Event(_))).count();
            assert!(events == 0 || events == 2, "{name} variant {index}: {events} events");
            assert!(items_of(&server, &session).iter().filter(|body| matches!(body, ItemBody::Crash(_))).count() <= 1);
        }
    }
}

#[test]
fn a_huge_sparse_segment_is_dropped_unread() {
    let dir = TempDir::new();
    let session = dead_session(&dir, 2);
    let huge = dir.spool().join(&session).join("00000050.jsonl");
    std::fs::File::create(&huge).unwrap().set_len(1 << 30).unwrap();
    let server = recorder();
    let started = Instant::now();
    let tracer = builder(dir.path(), &server).init();
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    // Reading a gigabyte, even of zeros, takes far longer than this and a gigabyte of memory.
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    assert!(!huge.exists());
    assert_eq!(
        items_of(&server, &session).iter().filter(|body| matches!(body, ItemBody::Event(_))).count(),
        2,
        "the session's real items still arrive"
    );
    let dropped = drops(&server);
    assert!(dropped.get("spool_damaged").copied().unwrap_or(0) + dropped.get("spool_evicted").copied().unwrap_or(0) >= 1, "{dropped:?}");
}

#[test]
fn ten_thousand_junk_files_in_the_spool_cost_little() {
    let dir = TempDir::new();
    let session = dead_session(&dir, 2);
    for index in 0..10_000 {
        std::fs::write(dir.spool().join(format!("junk-{index}.tmp")), b"x").unwrap();
    }
    for index in 0..2_000 {
        std::fs::write(dir.spool().join(&session).join(format!("junk-{index}.tmp")), b"x").unwrap();
    }
    let server = recorder();
    let started = Instant::now();
    let tracer = builder(dir.path(), &server).init();
    let init = started.elapsed();
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    let flushed = started.elapsed();
    drop(tracer);
    assert!(init < Duration::from_millis(250), "init took {init:?}");
    assert!(flushed < Duration::from_secs(5), "the first flush took {flushed:?}");
    assert_eq!(items_of(&server, &session).iter().filter(|body| matches!(body, ItemBody::Event(_))).count(), 2);
    assert_eq!(
        std::fs::read_dir(dir.spool()).unwrap().filter(|entry| entry.as_ref().unwrap().file_name().to_string_lossy().starts_with("junk-")).count(),
        10_000,
        "files that are not the SDK's are left alone"
    );
}

#[test]
fn a_marker_from_the_future_with_absurd_totals_is_reported_once_and_dated_now() {
    let dir = TempDir::new();
    let session = dead_session(&dir, 1);
    let path = dir.spool().join(&session).join("session.json");
    let mut marker: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    marker["started"] = serde_json::json!(u64::MAX - 5);
    marker["updated"] = serde_json::json!(u64::MAX);
    marker["errors"] = serde_json::json!(u32::MAX);
    marker["totals"] = serde_json::json!({"seq": u32::MAX, "duration_s": u64::MAX, "focused_s": u64::MAX, "active_s": u64::MAX, "idle_s": u64::MAX});
    std::fs::write(&path, serde_json::to_vec(&marker).unwrap()).unwrap();
    let server = recorder();
    let tracer = builder(dir.path(), &server).init();
    assert!(tracer.flush_blocking(WAIT));
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let reports: Vec<_> =
        envelopes(&server).into_iter().filter(|envelope| envelope.session_id == session).flat_map(|envelope| envelope.items).collect();
    let ends: Vec<_> =
        reports.iter().filter(|item| matches!(&item.body, ItemBody::SessionEnd(end) if end.status == SessionStatus::Abnormal)).collect();
    let crashes: Vec<_> =
        reports.iter().filter(|item| matches!(&item.body, ItemBody::Crash(crash) if crash.kind == CrashKind::UncleanExit)).collect();
    assert_eq!((ends.len(), crashes.len()), (1, 1), "{reports:?}");
    assert!(ends[0].ts <= now && crashes[0].ts <= now, "a report may not be dated in the future");
    assert!(matches!(&ends[0].body, ItemBody::SessionEnd(end) if end.totals.duration_s == u64::MAX && end.errors == u32::MAX));
}

// ---- lifecycle ----

/// Open file descriptors of this process, where that can be counted.
fn open_files() -> Option<usize> {
    std::fs::read_dir("/dev/fd").ok().map(Iterator::count)
}

/// Starts and ends a tracer `times` times in this process and checks that nothing accumulates.
pub fn start_and_stop(times: usize) {
    let dir = TempDir::new();
    let server = recorder();
    // A few rounds first: the first use of anything may open a file or start a thread for good.
    for _ in 0..3 {
        drop(builder(dir.path(), &server).signals(&Signal::ALL).init());
    }
    let before = open_files();
    for round in 0..times {
        let tracer = builder(dir.path(), &server).signals(&Signal::ALL).init();
        tracer.track(AppEvent::FileOpen);
        drop(tracer.turn());
        if round % 10 == 0 {
            assert!(tracer.flush_blocking(WAIT));
        }
    }
    let after = open_files();
    if let (Some(before), Some(after)) = (before, after) {
        assert!(after <= before + 2, "open files went from {before} to {after} over {times} starts");
    }
    // Every worker and watchdog thread has ended: each holds its session folder until it does.
    assert_eq!(std::fs::read_dir(dir.spool()).unwrap().count(), 0, "session folders were left behind");
    let names = event_names(&server);
    assert_eq!(names.len(), times, "one event per run, each delivered once");
}

#[test]
fn starting_and_stopping_many_times_leaks_nothing() {
    start_and_stop(60);
}

#[test]
fn many_tracers_alive_at_once_stay_apart() {
    let shared = TempDir::new();
    let dirs: Vec<TempDir> = (0..12).map(|_| TempDir::new()).collect();
    let mut tracers = Vec::new();
    for index in 0..20 {
        let server = recorder();
        let dir = dirs.get(index).map_or(shared.path(), TempDir::path);
        tracers.push((builder(dir, &server).init(), server, index));
    }
    for (tracer, _, index) in &tracers {
        for _ in 0..=*index {
            tracer.track(AppEvent::FileOpen);
        }
    }
    // The last clone of each goes away on some other thread.
    let handles: Vec<_> = tracers
        .into_iter()
        .map(|(tracer, server, index)| {
            let clone = tracer.clone();
            drop(tracer);
            std::thread::spawn(move || {
                let session = clone.session_id();
                assert!(clone.flush_blocking(WAIT));
                drop(clone);
                let items = items_of(&server, &session);
                assert_eq!(items.iter().filter(|body| matches!(body, ItemBody::Event(_))).count(), index + 1, "tracer {index}");
                assert_eq!(items.iter().filter(|body| matches!(body, ItemBody::SessionEnd(end) if end.status == SessionStatus::Exited)).count(), 1);
                assert!(envelopes(&server).iter().all(|envelope| envelope.session_id == session), "tracer {index} sent another tracer's session");
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
}

#[test]
fn a_tracer_dropped_by_a_panic_ends_its_session_as_crashed() {
    let dir = TempDir::new();
    let server = recorder();
    let tracer = builder(dir.path(), &server).init();
    let session = tracer.session_id();
    let handle = std::thread::spawn(move || {
        let _tracer = tracer;
        panic!("the host's main thread fails");
    });
    assert!(handle.join().is_err());
    let items = items_of(&server, &session);
    assert!(
        items.iter().any(|body| matches!(body, ItemBody::SessionEnd(end) if end.status == SessionStatus::Crashed && end.errors == 0)),
        "{items:?}"
    );
    assert!(items.iter().any(|body| matches!(body, ItemBody::Crash(crash) if crash.kind == CrashKind::Panic && crash.message.as_deref() == Some("the host's main thread fails"))));
    assert_eq!(std::fs::read_dir(dir.spool()).unwrap().count(), 0);
}

#[test]
fn handles_that_outlive_the_tracer_do_nothing_and_do_not_fail() {
    let dir = TempDir::new();
    let server = recorder();
    let tracer = builder(dir.path(), &server).signals(&Signal::ALL).init();
    let (counter, gauge, logger, turn) = (tracer.counter("late"), tracer.gauge("late"), tracer.logger(None), tracer.turn());
    // The guard holds a clone, so the session only ends when it goes too.
    drop(tracer);
    assert!(dir.spool().join(std::fs::read_dir(dir.spool()).unwrap().next().unwrap().unwrap().file_name()).join("session.json").exists());
    drop(turn);
    assert_eq!(std::fs::read_dir(dir.spool()).unwrap().count(), 0);
    let requests = server.seen.lock().unwrap().len();
    counter.add(5);
    gauge.set(1.0);
    logger.log(&log::Record::builder().level(log::Level::Error).target("late").args(format_args!("after the end")).build());
    logger.flush();
    assert!(
        !logger.enabled(&log::Metadata::builder().level(log::Level::Trace).build())
            || logger.enabled(&log::Metadata::builder().level(log::Level::Error).build())
    );
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(server.seen.lock().unwrap().len(), requests, "nothing is sent after the session has ended");
    assert_eq!(std::fs::read_dir(dir.spool()).unwrap().count(), 0, "nothing is written after the session has ended");
}

#[test]
fn every_call_on_a_disabled_tracer_is_a_harmless_no_op() {
    let tracer = Tracer::disabled();
    let clone = tracer.clone();
    assert!(!tracer.is_enabled());
    tracer.track(AppEvent::FileOpen);
    tracer.track_with(AppEvent::FileSave, &[("count", tracer_sdk::Prop::Int(1))]);
    tracer.track_id(Category::File, "file.x");
    tracer.set_focused(true);
    tracer.note_input();
    tracer.frame(Duration::from_millis(1));
    tracer.first_frame();
    tracer.counter("c").inc();
    tracer.gauge("g").set(1.0);
    tracer.heartbeat();
    tracer.begin_turn();
    tracer.end_turn();
    drop(tracer.turn());
    tracer.pause_hang_detection();
    tracer.resume_hang_detection();
    tracer.report_handled(&"an error", Some("context"));
    #[cfg(unix)]
    tracer.report_child_exit("tool", std::os::unix::process::ExitStatusExt::from_raw(9));
    assert!(!tracer.consent(ConsentCategory::Usage));
    tracer.set_consent(ConsentCategory::Usage, true);
    assert!(!tracer.consent(ConsentCategory::Usage));
    assert!(!tracer.notice_pending());
    tracer.notice_shown();
    tracer.forget();
    assert_eq!((tracer.install_id(), tracer.session_id()), (String::new(), String::new()));
    assert!(tracer.flush_blocking(Duration::from_secs(1)));
    let dir = TempDir::new();
    let report = dir.path().join("report.txt");
    tracer.write_report(&report).unwrap();
    assert!(!report.exists());

    // Its logger passes records through to the host's, or swallows them when there is none.
    struct Count(std::sync::Arc<std::sync::atomic::AtomicU64>);
    impl log::Log for Count {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }
        fn log(&self, _: &log::Record<'_>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn flush(&self) {
            self.0.fetch_add(100, Ordering::Relaxed);
        }
    }
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let record = log::Record::builder().level(log::Level::Error).target("host").args(format_args!("line")).build();
    let wrapped = clone.logger(Some(Box::new(Count(seen.clone()))));
    wrapped.log(&record);
    wrapped.flush();
    assert_eq!(seen.load(Ordering::Relaxed), 101);
    let silent = clone.logger(None);
    assert!(!silent.enabled(record.metadata()));
    silent.log(&record);
    silent.flush();
    let _ = PathBuf::new();
}

// ---- what the builder sets reaches the wire ----

#[test]
fn builder_settings_and_child_exits_reach_the_server() {
    let dir = TempDir::new();
    let server = recorder();
    let tracer = builder(dir.path(), &server)
        .build("abc1234")
        .process(tracer_sdk::ProcessKind::Service)
        .gpu(tracer_sdk::Gpu { name: "Example GPU".to_owned(), vendor: Some("Example".to_owned()), backend: Some("metal".to_owned()), driver: None })
        .signals(&Signal::ALL)
        .without(Signal::Hang)
        .without(Signal::Sys)
        .logger(tracer_sdk::LoggerMode::None)
        .inspect(true)
        .init();
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        tracer.report_child_exit("exporter", std::process::ExitStatus::from_raw(3 << 8));
        tracer.report_child_exit("exporter", std::process::ExitStatus::from_raw(9));
        tracer.report_child_exit("exporter", std::process::ExitStatus::from_raw(0));
    }
    let report = dir.path().join("report.txt");
    tracer.write_report(&report).unwrap();
    assert!(tracer.flush_blocking(WAIT));
    let session = tracer.session_id();
    let install = tracer.install_id();
    drop(tracer);

    let sent = envelopes(&server);
    let first = &sent[0];
    assert_eq!((first.app.build.as_deref(), first.app.process), (Some("abc1234"), tracer_sdk::ProcessKind::Service));
    let gpu = first.device.as_ref().and_then(|device| device.gpu.as_ref()).unwrap();
    assert_eq!((gpu.name.as_str(), gpu.backend.as_deref()), ("Example GPU", Some("metal")));
    let items = items_of(&server, &session);
    let start = items.iter().find_map(|body| if let ItemBody::SessionStart(start) = body { Some(start) } else { None }).unwrap();
    assert_eq!(start.signals, ["session", "usage", "crash", "perf", "logs"]);
    #[cfg(unix)]
    {
        let exits: Vec<_> = items.iter().filter_map(|body| if let ItemBody::Crash(crash) = body { Some(crash) } else { None }).collect();
        assert_eq!(exits.len(), 2, "a successful exit is not reported");
        assert!(exits.iter().all(|crash| crash.kind == CrashKind::ChildExit && crash.fatal && crash.context.as_deref() == Some("exporter")));
        assert_eq!((exits[0].exit_code, exits[1].signal), (Some(3), Some(9)));
    }
    // The local report names the install and the session, and the mirror holds what was sent.
    let text = std::fs::read_to_string(report).unwrap();
    assert!(text.contains(&format!("install_id: {install}")) && text.contains(&format!("session_id: {session}")), "{text}");
    assert!(std::fs::read_to_string(dir.sdk().join("outbox.jsonl")).unwrap().contains("session_start"));
}
