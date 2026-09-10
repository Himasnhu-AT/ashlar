#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Ending a short-lived (command-line) run explicitly. Its own test binary: the dialog hook is
//! process-wide.

use std::time::{Duration, Instant};

use ashlar_telemetry::sdk::proto::{CrashKind, ItemBody, SessionStatus};
use ashlar_telemetry::sdk::transport::{Reply, Request, Transport, TransportError};
use ashlar_telemetry::testing::{self, Recorder};
use ashlar_telemetry::{ProcessKind, Telemetry, TelemetryConfig};

fn cfg() -> TelemetryConfig {
    TelemetryConfig::new("demo", "1.0.0").process(ProcessKind::Cli)
}

fn client(dir: &std::path::Path, fake: &Recorder) -> Telemetry {
    testing::client(&cfg(), dir, fake, Duration::from_secs(2))
}

fn unclean(fake: &Recorder) -> bool {
    fake.items().iter().any(|b| matches!(b, ItemBody::Crash(c) if c.kind == CrashKind::UncleanExit))
}

fn ends(fake: &Recorder, session: &str) -> Vec<SessionStatus> {
    let mut out = Vec::new();
    for env in fake.envelopes().into_iter().filter(|e| e.session_id == session) {
        out.extend(env.items.into_iter().filter_map(|i| match i.body {
            ItemBody::SessionEnd(end) => Some(end.status),
            _ => None,
        }));
    }
    out
}

/// A transport that takes `delay` to answer every request.
#[derive(Clone)]
struct Slow(Recorder, Duration);

impl Transport for Slow {
    fn send(&mut self, r: &Request<'_>) -> Result<Reply, TransportError> {
        std::thread::sleep(self.1);
        self.0.send(r)
    }
}

#[test]
fn a_cli_run_that_ends_and_exits_without_dropping_is_not_an_unclean_exit() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Recorder::default();
    let t = client(dir.path(), &fake);
    let session = t.tracer().session_id();
    // the dialog hook holds a clone; end must not depend on it being dropped
    t.pause_hangs_in_dialogs();
    t.command("doc.edit", true, Duration::from_millis(3));
    assert!(t.end(Duration::from_secs(5)), "everything sent: {:?}", fake.types());
    assert_eq!(ends(&fake, &session), vec![SessionStatus::Exited]);
    assert!(fake.events().contains(&"command.run".to_string()));
    // a command-line run has no hang watchdog and no frame or system performance
    let start = fake.items().into_iter().find_map(|b| if let ItemBody::SessionStart(s) = b { Some(s) } else { None }).unwrap();
    assert_eq!(start.signals, ["session", "usage", "crash", "logs"]);
    let envelope = fake.envelopes().into_iter().next().unwrap();
    assert_eq!(envelope.app.process, ProcessKind::Cli);
    // as `std::process::exit` does: nothing is dropped
    std::mem::forget(t);

    let next_fake = Recorder::default();
    let next = client(dir.path(), &next_fake);
    assert!(next.end(Duration::from_secs(5)));
    assert!(!unclean(&fake) && !unclean(&next_fake), "{:?}", next_fake.types());
    assert!(ends(&next_fake, &session).is_empty(), "the first session is not reported again");
}

#[test]
fn end_twice_is_fine_and_nothing_is_recorded_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Recorder::default();
    let t = client(dir.path(), &fake);
    let clone = t.clone();
    assert!(t.end(Duration::from_secs(5)));
    let sent = fake.items().len();
    assert!(t.end(Duration::from_secs(5)));
    assert!(clone.end(Duration::from_millis(1)));
    t.command("doc.edit", true, Duration::from_millis(1));
    t.saved();
    drop(clone);
    drop(t);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(fake.items().len(), sent);
    assert_eq!(fake.types().iter().filter(|t| **t == "session_end").count(), 1);
}

#[test]
fn end_waits_no_longer_than_its_cap_for_a_slow_server_and_the_next_run_sends_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let slow = Recorder::default();
    let target = ashlar_telemetry::Target { endpoint: "http://127.0.0.1:9".into(), key: "tk_test".into(), host: "127.0.0.1".into() };
    let t = Telemetry::with_transport(&cfg(), &target, dir.path(), Slow(slow.clone(), Duration::from_secs(3)), |c| {
        c.tick_interval = Duration::from_millis(10);
    });
    let session = t.tracer().session_id();
    t.command("doc.edit", true, Duration::from_millis(1));
    let started = Instant::now();
    assert!(!t.end(Duration::from_millis(100)));
    let took = started.elapsed();
    assert!(took < Duration::from_millis(600), "{took:?}");
    std::mem::forget(t);

    let fake = Recorder::default();
    let next = client(dir.path(), &fake);
    assert!(next.end(Duration::from_secs(5)));
    // what the slow server had not taken arrives with the next run, the session ended as exited
    let delivered = ends(&fake, &session).len() + ends(&slow, &session).len();
    assert_eq!(delivered, 1, "{:?} / {:?}", fake.types(), slow.types());
    assert!(!unclean(&fake));
}

/// A transport that never reaches the server.
struct Offline;

impl Transport for Offline {
    fn send(&mut self, _: &Request<'_>) -> Result<Reply, TransportError> {
        Err(TransportError::Network("offline".into()))
    }
}

/// Not a test of its own: a stdio server the test below stops with SIGTERM.
#[test]
fn server_child() {
    let Some(dir) = std::env::var_os("END_TEST_CHILD_DIR") else { return };
    let dir = std::path::PathBuf::from(dir);
    let target = ashlar_telemetry::Target { endpoint: "http://127.0.0.1:9".into(), key: "tk_test".into(), host: "127.0.0.1".into() };
    let t = Telemetry::with_transport(&cfg(), &target, &dir, Offline, |_| {});
    t.finish_on_termination(Duration::from_millis(50));
    t.command("doc.edit", true, Duration::from_millis(1));
    let report = dir.join("session.txt");
    std::fs::write(dir.join("session.part"), t.tracer().session_id()).unwrap();
    std::fs::rename(dir.join("session.part"), &report).unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
fn a_server_stopped_by_sigterm_ends_its_session_and_dies_of_the_signal() {
    use std::os::unix::process::ExitStatusExt;
    let dir = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "server_child", "--nocapture", "--test-threads=1"])
        .env("END_TEST_CHILD_DIR", dir.path())
        .env_remove("TRACER_DISABLED")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let report = dir.path().join("session.txt");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !report.exists() {
        assert!(Instant::now() < deadline && child.try_wait().unwrap().is_none(), "the child did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
    let session = std::fs::read_to_string(&report).unwrap();
    let status = std::process::Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    assert!(status.success());
    let ended = child.wait().unwrap();
    assert_eq!(ended.signal(), Some(15), "{ended:?}");

    let fake = Recorder::default();
    let next = client(dir.path(), &fake);
    assert!(next.end(Duration::from_secs(5)));
    assert_eq!(ends(&fake, &session), vec![SessionStatus::Exited], "{:?}", fake.types());
    assert!(!unclean(&fake));
    assert!(fake.events().contains(&"command.run".to_string()));
}
