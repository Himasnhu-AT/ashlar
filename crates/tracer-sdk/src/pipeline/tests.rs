//! Tests of the calling-thread paths, with no worker thread and no ambient kill switch. The flows that
//! need the worker and a transport are in `tests/pipeline.rs`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracer_proto::{Catalog, CatalogEvent, ConsentCategory, Crash, CrashKind, Item, ItemBody, Perf, SessionStatus, Signal};

use super::files::testing::TempDir;
use super::files::{lock, read_json};
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
    let marker: Marker = read_json(&dir.path().join(SDK_DIR).join(SESSION_FILE)).unwrap();
    assert_eq!(marker.header.session_id, pipeline.session_id());
    assert_eq!(marker.header.install_id, pipeline.install_id());
    assert!(!marker.panicked);
    assert_eq!(pipeline.data_dir(), dir.path().join(SDK_DIR));
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
    assert!(read_json::<Marker>(&dir.path().join(SDK_DIR).join(SESSION_FILE)).unwrap().panicked);
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
    let marker: Marker = read_json(&dir.path().join(SDK_DIR).join(SESSION_FILE)).unwrap();
    assert!(marker.panicked);
    assert_eq!(marker.errors, 2);
    assert!(marker.updated >= marker.started);
}

#[test]
fn session_time_is_focused_active_or_idle() {
    let dir = TempDir::new("pipeline");
    let mut config = config(dir.path());
    config.idle_after = Duration::from_secs(120);
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
    assert!(!dir.path().join(SDK_DIR).join(SESSION_FILE).exists());
    let lines = spooled(dir.path());
    assert_eq!(lines.len(), 2);
    // A panic was seen, but the thread ending the session is not panicking: the process survived it.
    assert!(matches!(&lines[1].2.body, ItemBody::SessionEnd(end) if end.status == SessionStatus::Exited && end.errors == 1));
    // The session is over: nothing more is accepted, and the marker does not come back.
    assert!(!pipeline.allows(Signal::Crash));
    pipeline.submit_now(Signal::Crash, crash());
    pipeline.mark_panicked();
    assert_eq!(spooled(dir.path()).len(), 2);
    assert!(!dir.path().join(SDK_DIR).join(SESSION_FILE).exists());
}

#[test]
fn a_session_that_ends_while_panicking_is_crashed() {
    let dir = TempDir::new("pipeline");
    let pipeline = idle(&config(dir.path()));
    pipeline.mark_panicked();
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
    assert!(matches!(&lines.last().unwrap().2.body, ItemBody::SessionEnd(end) if end.status == SessionStatus::Crashed));
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
    assert!(!dir.path().join(SDK_DIR).join(SESSION_FILE).exists());
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
