//! Tests of the calling-thread paths, with no worker thread and no ambient kill switch. The flows that
//! need the worker and a transport are in `tests/pipeline.rs`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracer_proto::{Catalog, CatalogEvent, ConsentCategory, Crash, CrashKind, Item, ItemBody, Perf, SessionStatus, Signal};

use super::files::testing::TempDir;
use super::files::{lock, read_json, write_json};
use super::identity::derived_id;
use super::session::{Marker, SESSION_FILE};
use super::spool::SPOOL_DIR;
use super::{DROP_BUSY, DROP_QUEUE_FULL, Pipeline, SDK_DIR};
use crate::config::Config;
use crate::sink::Sink;

fn config(dir: &Path) -> Config {
    let mut config = Config::new("key", "host", "1.0.0");
    config.data_dir = dir.to_owned();
    let event = |name: &str, category: &str| CatalogEvent { name: name.to_owned(), category: category.to_owned() };
    config.catalog = Catalog { events: vec![event("file.open", "file"), event("edit.undo", "edit")] };
    config.listen_categories = vec!["file".to_owned()];
    config
}

/// A pipeline whose queue nobody drains.
fn idle(config: &Config) -> Arc<Pipeline> {
    Pipeline::start_with(config, None, false, false)
}

/// Every line in the spool as (session id, category byte, item).
fn spooled(dir: &Path) -> Vec<(String, u8, Item)> {
    let mut lines = Vec::new();
    let mut sessions: Vec<_> =
        std::fs::read_dir(dir.join(SDK_DIR).join(SPOOL_DIR)).into_iter().flatten().flatten().map(|entry| entry.path()).collect();
    sessions.sort();
    for session in sessions {
        let mut segments: Vec<_> = std::fs::read_dir(&session).unwrap().flatten().map(|entry| entry.path()).collect();
        segments.retain(|path| path.extension().is_some_and(|extension| extension == "jsonl"));
        segments.sort();
        for segment in segments {
            for line in std::fs::read_to_string(segment).unwrap().lines() {
                let item = serde_json::from_str(&line[1..]).unwrap();
                lines.push((session.file_name().unwrap().to_str().unwrap().to_owned(), line.as_bytes()[0], item));
            }
        }
    }
    lines
}

fn crash() -> ItemBody {
    ItemBody::Crash(Crash { kind: CrashKind::Panic, fatal: true, message: Some("boom".to_owned()), ..Crash::default() })
}

#[test]
fn session_start_is_on_disk_after_start_with_catalog_and_selection() {
    let dir = TempDir::new("pipeline");
    let pipeline = idle(&config(dir.path()));
    let lines = spooled(dir.path());
    assert_eq!(lines.len(), 1);
    let (session, category, item) = &lines[0];
    assert_eq!(session, &pipeline.session_id());
    assert_eq!(*category, b'u');
    let ItemBody::SessionStart(start) = &item.body else { panic!("expected session_start, got {item:?}") };
    assert_eq!(start.catalog_hash.as_ref().map(String::len), Some(16));
    assert_eq!(start.catalog.as_ref().map(|catalog| catalog.events.len()), Some(2));
    assert_eq!(start.listening, vec!["file".to_owned()]);
    assert_eq!(start.signals, vec!["session", "usage", "crash", "hang", "perf", "sys", "logs"]);
    let marker: Marker = read_json(&pipeline.session_dir().join(SESSION_FILE)).unwrap();
    assert_eq!(marker.header.session_id, pipeline.session_id());
    assert_eq!(marker.header.install_id, pipeline.install_id());
    assert!(!marker.panicked);
    assert_eq!(pipeline.session_dir(), dir.path().join(SDK_DIR).join(SPOOL_DIR).join(pipeline.session_id()));
}

/// Ends `pipeline` and leaves its folder as a killed process would have: marker present, lock free.
fn leave_unclean(pipeline: &Pipeline) -> String {
    let marker: Marker = read_json(&pipeline.session_dir().join(SESSION_FILE)).unwrap();
    pipeline.shutdown();
    std::fs::create_dir_all(pipeline.session_dir()).unwrap();
    assert!(write_json(&pipeline.session_dir().join(SESSION_FILE), &marker));
    pipeline.session_id()
}

#[test]
fn an_unclean_session_is_reported_with_ids_derived_from_its_session_id() {
    let dir = TempDir::new("pipeline");
    let config = config(dir.path());
    let gone = leave_unclean(&idle(&config));
    let reports = |lines: Vec<(String, u8, Item)>| -> Vec<(u8, Item)> {
        lines
            .into_iter()
            .filter(|(session, _, _)| *session == gone)
            .map(|(_, category, item)| (category, item))
            .filter(|(_, item)| {
                !matches!(item.body, ItemBody::SessionStart(_) | ItemBody::SessionEnd(tracer_proto::SessionEnd { status: SessionStatus::Exited, .. }))
            })
            .collect()
    };
    let adopter = idle(&config);
    let first = reports(spooled(dir.path()));
    assert_eq!(first.len(), 2, "{first:?}");
    assert!(matches!(&first[0], (b'u', Item { body: ItemBody::SessionEnd(end), .. }) if end.status == SessionStatus::Abnormal));
    assert!(matches!(&first[1], (b'c', Item { body: ItemBody::Crash(crash), .. }) if crash.kind == CrashKind::UncleanExit && crash.fatal));
    assert_eq!(first[0].1.id, derived_id(&gone, "session_end"));
    assert_eq!(first[1].1.id, derived_id(&gone, "unclean_exit"));
    assert!(!dir.path().join(SDK_DIR).join(SPOOL_DIR).join(&gone).join(SESSION_FILE).exists());
    // Looking again reports nothing more.
    adopter.shared.maintain();
    assert_eq!(reports(spooled(dir.path())).len(), 2);
    adopter.shutdown();

    // Had the adopter died after writing the report but before removing the marker, the next process
    // would write it again; the same ids let the server keep one.
    let marker = Marker { panicked: false, ..read_json::<Marker>(&idle(&config).session_dir().join(SESSION_FILE)).unwrap() };
    let mut repeated = marker.clone();
    repeated.header.session_id = gone.clone();
    assert!(write_json(&dir.path().join(SDK_DIR).join(SPOOL_DIR).join(&gone).join(SESSION_FILE), &repeated));
    let _later = idle(&config);
    let again = reports(spooled(dir.path()));
    assert_eq!(again.len(), 4);
    assert_eq!((&again[2].1.id, &again[3].1.id), (&first[0].1.id, &first[1].1.id));
}

#[test]
fn the_session_summary_names_the_install_the_session_and_its_totals() {
    let dir = TempDir::new("pipeline");
    let mut config = config(dir.path());
    config.tick_interval = Duration::from_secs(3600);
    let pipeline = idle(&config);
    pipeline.count_error();
    pipeline.shared.sample(pipeline.shared.started + Duration::from_secs(90));
    let summary = pipeline.session_summary();
    let expected = format!(
        "install_id: {}\nsession_id: {}\nstarted_ms: {}\nduration_s: 90\nfocused_s: 90\nactive_s: 0\nidle_s: 90\nerrors: 1\n",
        pipeline.install_id(),
        pipeline.session_id(),
        pipeline.shared.started_ms
    );
    assert_eq!(summary, expected);
}

#[test]
fn a_full_queue_drops_and_counts_without_blocking() {
    let dir = TempDir::new("pipeline");
    let mut config = config(dir.path());
    config.queue_capacity = 3;
    let pipeline = idle(&config);
    let started = Instant::now();
    for _ in 0..10 {
        pipeline.submit(Signal::Perf, ItemBody::Perf(Perf::default()));
    }
    pipeline.track("file.open", "file", false, &[]);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(pipeline.shared.drops.take().get(DROP_QUEUE_FULL), Some(&8));
    // Nothing reached the disk: only the session start is there.
    assert_eq!(spooled(dir.path()).len(), 1);
}

#[test]
fn events_not_listened_to_never_reach_the_queue() {
    let dir = TempDir::new("pipeline");
    let mut config = config(dir.path());
    config.queue_capacity = 2;
    let pipeline = idle(&config);
    for _ in 0..50 {
        pipeline.track("edit.undo", "edit", false, &[]);
        pipeline.track("edit.anything", "edit", true, &[]);
    }
    // Had any of them been queued, the queue of two would have overflowed.
    assert!(pipeline.shared.drops.take().is_empty());
    pipeline.track("file.open", "file", false, &[]);
    pipeline.track("file.export", "file", true, &[]);
    pipeline.track("file.open", "file", false, &[]);
    assert_eq!(pipeline.shared.drops.take().get(DROP_QUEUE_FULL), Some(&1));
}

#[test]
fn submit_now_lands_on_disk_without_the_worker() {
    let dir = TempDir::new("pipeline");
    let pipeline = idle(&config(dir.path()));
    pipeline.submit_now(Signal::Crash, crash());
    let lines = spooled(dir.path());
    assert_eq!(lines.len(), 2);
    let (session, category, item) = &lines[1];
    assert_eq!((session, *category), (&pipeline.session_id(), b'c'));
    assert!(matches!(&item.body, ItemBody::Crash(crash) if crash.message.as_deref() == Some("boom")));
    assert_eq!(item.id.len(), 32);
    assert!(item.ts > 0);
}

#[test]
fn submit_now_tolerates_a_poisoned_lock() {
    let dir = TempDir::new("pipeline");
    let pipeline = idle(&config(dir.path()));
    let poisoner = pipeline.clone();
    let _ = std::thread::spawn(move || {
        let _disk = poisoner.shared.disk.lock().unwrap();
        let _clock = poisoner.shared.clock.lock().unwrap();
        let _marker = poisoner.shared.marker_removed.lock().unwrap();
        panic!("poison the pipeline's locks");
    })
    .join();
    assert!(pipeline.shared.disk.is_poisoned());
    pipeline.submit_now(Signal::Crash, crash());
    pipeline.mark_panicked();
    assert_eq!(spooled(dir.path()).len(), 2);
    assert!(read_json::<Marker>(&pipeline.session_dir().join(SESSION_FILE)).unwrap().panicked);
    pipeline.shutdown();
    assert_eq!(spooled(dir.path()).len(), 3);
}

#[test]
fn submit_now_gives_up_instead_of_deadlocking_on_a_held_lock() {
    let dir = TempDir::new("pipeline");
    let pipeline = idle(&config(dir.path()));
    let held = lock(&pipeline.shared.disk);
    // The same thread holds the lock, as it would if the panic happened inside the spool.
    pipeline.submit_now(Signal::Crash, crash());
    pipeline.mark_panicked();
    drop(held);
    assert_eq!(pipeline.shared.drops.take().get(DROP_BUSY), Some(&1));
    assert_eq!(spooled(dir.path()).len(), 1);
}

#[test]
fn mark_panicked_and_errors_reach_the_marker() {
    let dir = TempDir::new("pipeline");
    let pipeline = idle(&config(dir.path()));
    pipeline.count_error();
    pipeline.count_error();
    pipeline.mark_panicked();
    let marker: Marker = read_json(&pipeline.session_dir().join(SESSION_FILE)).unwrap();
    assert!(marker.panicked);
    assert_eq!(marker.errors, 2);
    assert!(marker.updated >= marker.started);
}

#[test]
fn session_time_is_focused_active_or_idle() {
    let dir = TempDir::new("pipeline");
    let mut config = config(dir.path());
    config.idle_after = Duration::from_secs(120);
    // A long tick, so that the steps of this test are not taken for the machine having slept.
    config.tick_interval = Duration::from_secs(3600);
    let pipeline = idle(&config);
    let start = pipeline.shared.started;
    let at = |seconds: u64| start + Duration::from_secs(seconds);
    let split = |totals: tracer_proto::SessionTotals| (totals.duration_s, totals.focused_s, totals.active_s, totals.idle_s);
    // A host that never says otherwise counts as focused; without input nothing is active.
    assert_eq!(split(pipeline.shared.sample(at(10))), (10, 10, 0, 10));
    // Input within `idle_after` makes focused time active.
    pipeline.note_input();
    assert_eq!(split(pipeline.shared.sample(at(20))), (20, 20, 10, 10));
    assert_eq!(split(pipeline.shared.sample(at(100))), (100, 100, 90, 10));
    // Long after the last input it is idle again.
    assert_eq!(split(pipeline.shared.sample(at(300))), (300, 300, 90, 210));
    // Without focus nothing is focused or active, even right after input.
    pipeline.set_focused(false);
    pipeline.shared.last_input.store(300_000, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(split(pipeline.shared.sample(at(310))), (310, 300, 90, 220));
    pipeline.set_focused(true);
    assert_eq!(split(pipeline.shared.sample(at(320))), (320, 310, 100, 220));
}

#[test]
fn shutdown_writes_the_session_end_and_removes_the_marker() {
    let dir = TempDir::new("pipeline");
    let pipeline = idle(&config(dir.path()));
    pipeline.count_error();
    pipeline.mark_panicked();
    pipeline.shutdown();
    pipeline.shutdown();
    assert!(!pipeline.session_dir().join(SESSION_FILE).exists());
    let lines = spooled(dir.path());
    assert_eq!(lines.len(), 2);
    // A panic was seen, but the thread ending the session is not panicking: the process survived it.
    assert!(matches!(&lines[1].2.body, ItemBody::SessionEnd(end) if end.status == SessionStatus::Exited && end.errors == 1));
    // The session is over: nothing more is accepted, and the marker does not come back.
    assert!(!pipeline.allows(Signal::Crash));
    pipeline.submit_now(Signal::Crash, crash());
    pipeline.mark_panicked();
    assert_eq!(spooled(dir.path()).len(), 2);
    assert!(!pipeline.session_dir().join(SESSION_FILE).exists());
}

#[test]
fn a_session_that_ends_while_panicking_is_crashed() {
    let dir = TempDir::new("pipeline");
    let pipeline = idle(&config(dir.path()));
    // What the panic hook does for each panic: one the host caught earlier, then the one that ends it.
    for _ in 0..2 {
        pipeline.count_error();
        pipeline.mark_panicked();
    }
    struct EndOnDrop(Arc<Pipeline>);
    impl Drop for EndOnDrop {
        fn drop(&mut self) {
            self.0.shutdown();
        }
    }
    let guard = EndOnDrop(pipeline);
    let _ = std::thread::spawn(move || {
        let _guard = guard;
        panic!("unwinding through the guard");
    })
    .join();
    let lines = spooled(dir.path());
    // `crashed` already says the last panic was not survived, so `errors` holds only the one that was.
    assert!(matches!(&lines.last().unwrap().2.body, ItemBody::SessionEnd(end) if end.status == SessionStatus::Crashed && end.errors == 1));
}

#[test]
fn allows_needs_the_signal_enabled_and_the_category_consented() {
    let dir = TempDir::new("pipeline");
    let mut config = config(dir.path());
    config.signals = vec![Signal::Session, Signal::Usage, Signal::Crash, Signal::Perf];
    let pipeline = idle(&config);
    assert!(pipeline.allows(Signal::Crash) && pipeline.allows(Signal::Perf) && pipeline.allows(Signal::Usage));
    assert!(!pipeline.allows(Signal::Hang) && !pipeline.allows(Signal::Sys) && !pipeline.allows(Signal::Logs));
    pipeline.set_consent(ConsentCategory::Crash, false);
    assert!(!pipeline.allows(Signal::Crash));
    assert!(pipeline.allows(Signal::Perf));
    pipeline.submit_now(Signal::Crash, crash());
    pipeline.submit(Signal::Crash, crash());
    assert_eq!(spooled(dir.path()).len(), 1);
    pipeline.set_consent(ConsentCategory::Crash, true);
    assert!(pipeline.allows(Signal::Crash));
    assert!(pipeline.consent(ConsentCategory::Crash));
}

#[test]
fn the_kill_switch_records_nothing_and_creates_nothing() {
    let dir = TempDir::new("pipeline");
    let pipeline = Pipeline::start_with(&config(dir.path()), None, true, true);
    for signal in Signal::ALL {
        assert!(!pipeline.allows(signal));
    }
    pipeline.track("file.open", "file", false, &[]);
    pipeline.submit(Signal::Crash, crash());
    pipeline.submit_now(Signal::Crash, crash());
    pipeline.count_drop("anything");
    pipeline.mark_panicked();
    pipeline.forget();
    // What a host does at every start, kill switch or not, must leave no trace either.
    pipeline.set_consent(ConsentCategory::Usage, false);
    assert!(!pipeline.consent(ConsentCategory::Usage));
    pipeline.notice_shown();
    assert!(pipeline.flush_blocking(Duration::from_millis(10)));
    assert!(!pipeline.notice_pending());
    assert_eq!(pipeline.install_id(), "");
    pipeline.shutdown();
    assert!(!dir.path().join(SDK_DIR).exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn the_kill_switch_leaves_saved_consent_and_identity_alone() {
    let dir = TempDir::new("pipeline");
    let install_id = {
        let pipeline = idle(&config(dir.path()));
        pipeline.set_consent(ConsentCategory::Performance, false);
        pipeline.shutdown();
        pipeline.install_id()
    };
    let before = std::fs::read(dir.path().join(SDK_DIR).join("consent.json")).unwrap();
    let pipeline = Pipeline::start_with(&config(dir.path()), None, true, true);
    assert_eq!(pipeline.install_id(), install_id);
    assert!(!pipeline.consent(ConsentCategory::Performance));
    assert!(pipeline.consent(ConsentCategory::Usage));
    assert!(!pipeline.allows(Signal::Usage));
    pipeline.shutdown();
    assert_eq!(std::fs::read(dir.path().join(SDK_DIR).join("consent.json")).unwrap(), before);
    assert!(!pipeline.session_dir().join(SESSION_FILE).exists());
}

#[test]
fn an_unusable_folder_switches_the_pipeline_off() {
    let dir = TempDir::new("pipeline");
    let blocker = dir.path().join("not-a-folder");
    std::fs::write(&blocker, b"x").unwrap();
    let pipeline = Pipeline::start_with(&config(&blocker), None, false, true);
    assert!(!pipeline.allows(Signal::Usage));
    pipeline.track("file.open", "file", false, &[]);
    pipeline.submit_now(Signal::Crash, crash());
    assert!(pipeline.flush_blocking(Duration::from_millis(10)));
    pipeline.shutdown();
    assert_eq!(std::fs::read(&blocker).unwrap(), b"x");
}

/// Regression tests for the faults found in review.
mod review {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use tracer_proto::routes;

    use super::*;
    use crate::transport::{Reply, Request, Transport, TransportError};

    /// Answers ingest with 200 and forget with whatever status it is set to (0 = network error).
    #[derive(Clone, Default)]
    struct Server(Arc<Mutex<ServerState>>);

    #[derive(Default)]
    struct ServerState {
        forget_status: u16,
        /// Path and status of every request.
        seen: Vec<(String, u16)>,
        /// Every ingest body, unpacked, one after the other.
        ingested: Vec<u8>,
    }

    impl Server {
        fn answering_forget_with(status: u16) -> Self {
            let server = Self::default();
            server.set_forget(status);
            server
        }

        fn set_forget(&self, status: u16) {
            self.0.lock().unwrap().forget_status = status;
        }

        /// Everything that was sent for ingest, as text.
        fn ingested(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap().ingested).into_owned()
        }

        fn count(&self, path: &str, status: u16) -> usize {
            self.0.lock().unwrap().seen.iter().filter(|seen| seen.0 == path && seen.1 == status).count()
        }
    }

    impl Transport for Server {
        fn send(&mut self, request: &Request<'_>) -> Result<Reply, TransportError> {
            let mut state = self.0.lock().unwrap();
            let status = if request.path == routes::FORGET { state.forget_status } else { 200 };
            state.seen.push((request.path.to_owned(), status));
            if request.path == routes::INGEST {
                use std::io::Read;
                let _ = flate2::read::GzDecoder::new(request.body).read_to_end(&mut state.ingested);
            }
            if status == 0 {
                return Err(TransportError::Network("down".to_owned()));
            }
            Ok(Reply { status, body: br#"{"accepted":1}"#.to_vec(), retry_after: None })
        }
    }

    fn sending(dir: &Path, server: &Server, edit: impl FnOnce(&mut Config)) -> Arc<Pipeline> {
        let mut config = config(dir);
        config.endpoint = Some("https://telemetry.example.com".to_owned());
        config.tick_interval = Duration::from_millis(5);
        config.flush_interval = Duration::from_millis(20);
        config.retry_min = Duration::from_millis(20);
        config.retry_max = Duration::from_millis(80);
        config.send_before_notice = true;
        edit(&mut config);
        Pipeline::start_with(&config, Some(Box::new(server.clone())), false, true)
    }

    fn worker_is_alive(pipeline: &Pipeline) -> bool {
        lock(&pipeline.worker).as_ref().is_some_and(|handle| !handle.is_finished())
    }

    fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_flush_with_an_endless_timeout_does_not_panic() {
        let dir = TempDir::new("review");
        let pipeline = Pipeline::start_with(&config(dir.path()), None, false, true);
        assert!(pipeline.flush_blocking(Duration::MAX));
        pipeline.shutdown();
    }

    #[test]
    fn an_endless_shutdown_timeout_does_not_panic_in_the_destructor() {
        let dir = TempDir::new("review");
        let mut config = config(dir.path());
        config.shutdown_timeout = Duration::MAX;
        let pipeline = Pipeline::start_with(&config, None, false, true);
        pipeline.shutdown();
        assert!(!worker_is_alive(&pipeline));
    }

    #[test]
    fn endless_intervals_do_not_kill_the_worker() {
        let dir = TempDir::new("review");
        let server = Server::answering_forget_with(200);
        let pipeline = sending(dir.path(), &server, |config| {
            config.tick_interval = Duration::MAX;
            config.flush_interval = Duration::MAX;
            config.heartbeat_interval = Duration::MAX;
            config.idle_after = Duration::MAX;
            config.retry_min = Duration::MAX;
            config.retry_max = Duration::MAX;
            config.spool_max_age = Duration::MAX;
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(worker_is_alive(&pipeline), "the worker thread died at its first tick");
        pipeline.flush_blocking(Duration::from_secs(10));
        assert!(worker_is_alive(&pipeline));
        pipeline.shutdown();
    }

    #[test]
    fn a_failed_send_with_the_longest_backoff_does_not_kill_the_worker() {
        let dir = TempDir::new("review");
        let server = Server::answering_forget_with(0);
        let pipeline = sending(dir.path(), &server, |config| {
            config.retry_min = Duration::MAX;
            config.retry_max = Duration::MAX;
        });
        pipeline.forget();
        pipeline.flush_blocking(Duration::from_secs(10));
        pipeline.flush_blocking(Duration::from_secs(10));
        assert!(worker_is_alive(&pipeline), "the worker thread died");
        pipeline.shutdown();
    }

    #[test]
    fn two_unclean_sessions_found_at_one_start_are_both_reported() {
        let dir = TempDir::new("review");
        // Two processes that had nothing unsent (session signal off) and were killed: marker, no segments.
        let mut quiet = config(dir.path());
        quiet.signals = vec![Signal::Crash];
        let (first, second) = (idle(&quiet), idle(&quiet));
        let gone = [first.session_id(), second.session_id()];
        drop((first, second));
        let _third = idle(&quiet);
        let mut reported: Vec<String> = spooled(dir.path())
            .into_iter()
            .filter(|(_, _, item)| matches!(&item.body, ItemBody::Crash(crash) if crash.kind == CrashKind::UncleanExit))
            .map(|(session, _, _)| session)
            .collect();
        reported.sort();
        let mut expected = gone.to_vec();
        expected.sort();
        assert_eq!(reported, expected);
    }

    #[test]
    fn forget_and_flush_at_the_same_time_do_not_deadlock() {
        let dir = TempDir::new("review");
        let server = Server::answering_forget_with(200);
        let pipeline = sending(dir.path(), &server, |config| config.signals = vec![Signal::Crash]);
        let progress = Arc::new(AtomicU64::new(0));
        let threads: Vec<_> = ["forget", "flush"]
            .into_iter()
            .map(|role| {
                let (pipeline, progress) = (pipeline.clone(), progress.clone());
                std::thread::spawn(move || {
                    for _ in 0..1500 {
                        if role == "forget" {
                            pipeline.forget();
                        } else {
                            pipeline.flush_blocking(Duration::from_secs(2));
                        }
                        progress.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        let (mut last, mut stalled) = (0, 0);
        while threads.iter().any(|thread| !thread.is_finished()) {
            std::thread::sleep(Duration::from_millis(250));
            let now = progress.load(Ordering::Relaxed);
            stalled = if now == last { stalled + 1 } else { 0 };
            last = now;
            assert!(stalled < 40, "no progress for 10 s after {now} calls: forget() and the worker are deadlocked");
        }
        pipeline.shutdown();
    }

    /// Makes a folder read-only or writable again. False when a read-only folder can still be written,
    /// as it can with rights that ignore the mode; the fault cannot be staged then.
    #[cfg(unix)]
    fn read_only(dir: &Path, on: bool) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(if on { 0o555 } else { 0o755 })).unwrap();
        !on || std::fs::write(dir.join("probe"), b"").is_err()
    }

    #[cfg(unix)]
    #[test]
    fn an_opt_out_holds_when_the_consent_file_cannot_be_written() {
        let dir = TempDir::new("review");
        let pipeline = idle(&config(dir.path()));
        pipeline.notice_shown();
        let sdk = dir.path().join(SDK_DIR);
        if !read_only(&sdk, true) {
            read_only(&sdk, false);
            return;
        }
        pipeline.set_consent(ConsentCategory::Usage, false);
        assert!(!pipeline.consent(ConsentCategory::Usage));
        for _ in 0..3 {
            pipeline.shared.sync_shared_files();
            assert!(!pipeline.consent(ConsentCategory::Usage), "an opt-out must not come back on");
        }
        // Once the file can be written again, the choice is saved without being made again.
        read_only(&sdk, false);
        pipeline.shared.sync_shared_files();
        assert!(!pipeline.consent(ConsentCategory::Usage));
        assert!(std::fs::read_to_string(sdk.join("consent.json")).unwrap().contains(r#""usage":false"#));
    }

    #[cfg(unix)]
    #[test]
    fn a_forget_holds_when_the_install_file_cannot_be_written() {
        let dir = TempDir::new("review");
        let pipeline = idle(&config(dir.path()));
        let before = pipeline.install_id();
        let sdk = dir.path().join(SDK_DIR);
        if !read_only(&sdk, true) {
            read_only(&sdk, false);
            return;
        }
        pipeline.forget();
        let rotated = pipeline.install_id();
        assert_ne!(rotated, before);
        for _ in 0..3 {
            pipeline.shared.sync_shared_files();
            assert_eq!(pipeline.install_id(), rotated, "the forgotten install id must not come back");
        }
        assert_eq!(lock(&pipeline.shared.identity).pending_forget, vec![before.clone()], "the forget request must not be lost");
        read_only(&sdk, false);
        pipeline.shared.sync_shared_files();
        let saved = std::fs::read_to_string(sdk.join("install.json")).unwrap();
        assert!(saved.contains(&rotated) && saved.contains(&before), "{saved}");
        assert_eq!(pipeline.install_id(), rotated);
    }

    #[test]
    fn a_forget_request_that_keeps_failing_does_not_hold_up_ingest() {
        for failing in [500, 0, 429] {
            let dir = TempDir::new("review");
            let server = Server::answering_forget_with(failing);
            let pipeline = sending(dir.path(), &server, |_| {});
            let old = pipeline.install_id();
            pipeline.forget();
            pipeline.submit(Signal::Perf, ItemBody::Perf(Perf::default()));
            // The forget is still owed, so the flush is not clean; but the item is gone from the spool.
            assert!(!pipeline.flush_blocking(Duration::from_secs(10)), "{failing}");
            assert!(spooled(dir.path()).is_empty(), "{failing}: ingest was held up by the failing forget request");
            assert!(server.count(routes::INGEST, 200) >= 1);
            assert_eq!(lock(&pipeline.shared.identity).pending_forget, vec![old.clone()], "{failing}");
            // It is tried again later, with backoff, and goes through once the server answers.
            server.set_forget(200);
            assert!(wait_until(|| pipeline.flush_blocking(Duration::from_secs(10))), "{failing}");
            assert!(lock(&pipeline.shared.identity).pending_forget.is_empty());
            pipeline.shutdown();
        }
    }

    #[test]
    fn a_forget_request_the_server_will_never_accept_is_given_up_and_counted() {
        let dir = TempDir::new("review");
        let server = Server::answering_forget_with(404);
        let pipeline = sending(dir.path(), &server, |_| {});
        pipeline.forget();
        assert!(pipeline.flush_blocking(Duration::from_secs(10)));
        assert!(lock(&pipeline.shared.identity).pending_forget.is_empty());
        assert_eq!(server.count(routes::FORGET, 404), 1);
        // The count leaves with the very batch that follows the refusal.
        assert!(pipeline.flush_blocking(Duration::from_secs(10)));
        assert!(server.ingested().contains(r#""reasons":{"forget_rejected":1}"#), "{}", server.ingested());
        pipeline.shutdown();
    }

    #[test]
    fn an_unknown_key_on_forget_stops_sending_and_keeps_the_request() {
        let dir = TempDir::new("review");
        let server = Server::answering_forget_with(401);
        let pipeline = sending(dir.path(), &server, |_| {});
        pipeline.forget();
        assert!(!pipeline.flush_blocking(Duration::from_secs(10)));
        assert!(!pipeline.flush_blocking(Duration::from_secs(10)));
        assert_eq!(server.count(routes::FORGET, 401), 1);
        assert_eq!(lock(&pipeline.shared.identity).pending_forget.len(), 1);
        pipeline.shutdown();
    }

    #[test]
    fn reading_the_install_id_does_not_wait_for_the_install_record() {
        let dir = TempDir::new("review");
        let pipeline = idle(&config(dir.path()));
        let expected = pipeline.install_id();
        // The worker holds this while it reads and writes the install file.
        let held = lock(&pipeline.shared.identity);
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = pipeline.clone();
        std::thread::spawn(move || {
            let _ = sender.send((reader.install_id(), reader.session_summary()));
        });
        let answer = receiver.recv_timeout(Duration::from_secs(5));
        drop(held);
        let (install_id, summary) = answer.expect("install_id() waited on the install record");
        assert_eq!(install_id, expected);
        assert!(summary.contains(&expected));
    }
}

/// Time misbehaving, and the documented limits at and just past their edge.
mod time_and_limits {
    use super::super::files::testing::CLOCK_SKEW_MS;
    use super::*;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    fn totals_tuple(totals: tracer_proto::SessionTotals) -> (u64, u64, u64, u64) {
        (totals.duration_s, totals.focused_s, totals.active_s, totals.idle_s)
    }

    #[test]
    fn the_wall_clock_jumping_about_leaves_session_time_untouched() {
        let dir = TempDir::new("time");
        let mut config = config(dir.path());
        config.tick_interval = Duration::from_secs(3600);
        let pipeline = idle(&config);
        let start = pipeline.shared.started;
        let mut previous = (0, 0, 0, 0);
        // Forwards a day, back two, forwards a year, back to where it was: session time comes from the
        // monotonic clock and only ever grows, by exactly what really passed.
        for (step, skew) in [(1u64, DAY_MS), (2, -DAY_MS), (3, 365 * DAY_MS), (4, 0), (5, i64::MIN), (6, i64::MAX)] {
            CLOCK_SKEW_MS.with(|cell| cell.set(skew));
            pipeline.submit_now(Signal::Crash, crash());
            pipeline.shared.write_marker(false);
            let now = totals_tuple(pipeline.shared.sample(start + Duration::from_secs(step * 100)));
            assert_eq!(now.0, step * 100, "skew {skew}");
            assert!(now.0 >= previous.0 && now.1 >= previous.1 && now.2 >= previous.2 && now.3 >= previous.3, "totals went backwards at skew {skew}");
            previous = now;
        }
        CLOCK_SKEW_MS.with(|cell| cell.set(0));
        // The items carry whatever the wall clock said, including nonsense; none of that broke anything.
        let stamps: Vec<u64> = spooled(dir.path()).into_iter().filter(|(_, category, _)| *category == b'c').map(|(_, _, item)| item.ts).collect();
        assert_eq!(stamps.len(), 6);
        assert!(stamps[0] > stamps[1] && stamps[2] > stamps[0], "{stamps:?}");
        assert!(stamps[4] == 0 && stamps[5] > 1 << 62, "{stamps:?}");
        pipeline.shutdown();
    }

    #[test]
    fn a_marker_written_under_a_clock_far_ahead_is_reported_dated_now() {
        let dir = TempDir::new("time");
        let config = config(dir.path());
        let first = idle(&config);
        CLOCK_SKEW_MS.with(|cell| cell.set(400 * DAY_MS));
        first.shared.write_marker(false);
        CLOCK_SKEW_MS.with(|cell| cell.set(0));
        let gone = first.session_id();
        drop(first);
        let before = super::super::files::now_ms();
        let _next = idle(&config);
        let reports: Vec<Item> = spooled(dir.path())
            .into_iter()
            .filter(|(session, _, item)| *session == gone && !matches!(item.body, ItemBody::SessionStart(_)))
            .map(|(_, _, item)| item)
            .collect();
        assert_eq!(reports.len(), 2);
        assert!(reports.iter().all(|item| item.ts >= before && item.ts <= super::super::files::now_ms()), "{reports:?}");
    }

    #[test]
    fn a_tick_delayed_by_minutes_adds_at_most_its_cap() {
        let dir = TempDir::new("time");
        let mut config = config(dir.path());
        config.tick_interval = Duration::from_secs(1);
        let pipeline = idle(&config);
        let start = pipeline.shared.started;
        pipeline.note_input();
        assert_eq!(totals_tuple(pipeline.shared.sample(start + Duration::from_secs(1))), (1, 1, 1, 0));
        // The process was suspended for an hour, or the machine slept and its clock kept counting.
        let after = totals_tuple(pipeline.shared.sample(start + Duration::from_secs(3601)));
        assert_eq!(after, (61, 61, 1, 60), "one tick may add a minute at most, and long after the last input it is idle time");
        assert_eq!(totals_tuple(pipeline.shared.sample(start + Duration::from_secs(3602))).0, 62);
        // A sample from before the last one changes nothing.
        assert_eq!(totals_tuple(pipeline.shared.sample(start)).0, 62);
    }

    #[test]
    fn the_queue_holds_exactly_its_capacity() {
        for capacity in [1usize, 2, 7] {
            let dir = TempDir::new("limits");
            let mut config = config(dir.path());
            config.queue_capacity = capacity;
            let pipeline = idle(&config);
            for _ in 0..capacity {
                pipeline.track("file.open", "file", false, &[]);
            }
            assert!(pipeline.shared.drops.take().is_empty(), "capacity {capacity}: an item within the capacity was dropped");
            pipeline.track("file.open", "file", false, &[]);
            assert_eq!(pipeline.shared.drops.take().get(DROP_QUEUE_FULL), Some(&1), "capacity {capacity}");
        }
        // A capacity of zero is taken as one rather than as a queue that accepts nothing or blocks.
        let dir = TempDir::new("limits");
        let mut config = config(dir.path());
        config.queue_capacity = 0;
        let pipeline = idle(&config);
        pipeline.track("file.open", "file", false, &[]);
        pipeline.track("file.open", "file", false, &[]);
        assert_eq!(pipeline.shared.drops.take().get(DROP_QUEUE_FULL), Some(&1));
    }

    #[test]
    fn more_properties_than_the_queue_copies_are_counted_on_the_host_thread() {
        let dir = TempDir::new("limits");
        let pipeline = idle(&config(dir.path()));
        let many: Vec<(&'static str, crate::Prop)> = (0..super::super::MAX_QUEUED_PROPS + 3).map(|_| ("key", crate::Prop::Int(1))).collect();
        pipeline.track("file.open", "file", false, &many);
        assert_eq!(pipeline.shared.drops.take().get("prop_dropped"), Some(&3));
        pipeline.track("file.open", "file", false, &many[..super::super::MAX_QUEUED_PROPS]);
        assert!(pipeline.shared.drops.take().is_empty());
    }

    #[test]
    fn an_unclean_report_too_big_for_one_line_goes_without_its_log_lines() {
        let dir = TempDir::new("limits");
        let config = config(dir.path());
        let first = idle(&config);
        let gone = first.session_id();
        // More saved log lines than fit in one spool line.
        let crumb =
            serde_json::to_string(&tracer_proto::Breadcrumb { ts: 1, level: "warn".to_owned(), target: "host".to_owned(), message: "x".repeat(500) })
                .unwrap();
        std::fs::write(first.session_dir().join("breadcrumbs.jsonl"), format!("{crumb}\n").repeat(600)).unwrap();
        drop(first);
        let _next = idle(&config);
        let crashes: Vec<Crash> = spooled(dir.path())
            .into_iter()
            .filter_map(|(session, _, item)| match item.body {
                ItemBody::Crash(crash) if session == gone => Some(crash),
                _ => None,
            })
            .collect();
        assert_eq!(crashes.len(), 1);
        assert_eq!(crashes[0].kind, CrashKind::UncleanExit);
        assert!(crashes[0].breadcrumbs.is_empty(), "the report must fit, so the log lines are left out");
        assert!(!dir.path().join(SDK_DIR).join(SPOOL_DIR).join(&gone).join("breadcrumbs.jsonl").exists());
    }
}
