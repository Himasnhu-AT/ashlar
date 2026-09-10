//! The pipeline through the public API, against a recording in-memory transport and temporary folders.
//! Nothing here touches the network.
//!
//! The kill switches are process-wide environment variables. The kill-switch test sets them in child
//! processes only, and every other test steps into a clean child process if it finds one set (see
//! `reran_without_kill_switches`), so the suite gives the same answer whatever the developer's shell has.

// This whole file is test code, but the lint only exempts `#[test]` functions, not their helpers.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ashlar_trace::proto::{
    CrashKind, Envelope, ForgetRequest, HEADER_APP_KEY, Item, ItemBody, MAX_ENVELOPE_BYTES, PropValue, SCHEMA_VERSION, SessionStatus, routes,
};
use ashlar_trace::transport::{Reply, Request, Transport, TransportError};
use ashlar_trace::{Builder, ConsentCategory, Prop, Signal, Tracer};

ashlar_trace::events! {
    enum Category { File = "file", Edit = "edit", View = "view" }
    enum AppEvent {
        FileOpen = "file.open" in File,
        FileSave = "file.save" in File,
        Undo = "edit.undo" in Edit,
        Zoom = "view.zoom" in View,
    }
}

const KEY: &str = "test-write-key";
const WAIT: Duration = Duration::from_secs(10);

// ---- helpers ----

/// A folder under the build's temporary directory, removed on drop.
struct TempDir(PathBuf, bool);

impl TempDir {
    /// A folder another process created and will remove.
    fn existing(path: PathBuf) -> Self {
        Self(path, false)
    }

    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!("pipeline-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path, true)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// The SDK's own folder.
    fn sdk(&self) -> PathBuf {
        self.0.join("tracer")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// One request the transport was asked to deliver.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    key: String,
    gzip: bool,
    /// The body after decompression.
    body: Vec<u8>,
    /// The status answered, or 0 when the request failed as a network error.
    status: u16,
    at: Instant,
}

#[derive(Default)]
struct ServerState {
    seen: Vec<Seen>,
    offline: bool,
    /// How long each request takes.
    delay: Duration,
    need_catalog: bool,
    /// Answer 200 with a body that is not an ingest reply, as a proxy in between might.
    foreign_replies: bool,
    /// Replies to give before falling back to 200.
    script: VecDeque<Reply>,
}

/// A stand-in for the server. Clones share state, so a test keeps one and gives one to the SDK.
#[derive(Clone, Default)]
struct Server(Arc<Mutex<ServerState>>);

impl Transport for Server {
    fn send(&mut self, request: &Request<'_>) -> Result<Reply, TransportError> {
        let mut body = Vec::new();
        if request.gzip {
            flate2::read::GzDecoder::new(request.body).read_to_end(&mut body).unwrap();
        } else {
            body.extend_from_slice(request.body);
        }
        let delay = self.0.lock().unwrap().delay;
        std::thread::sleep(delay);
        let mut state = self.0.lock().unwrap();
        let reply = if state.offline {
            None
        } else if let Some(reply) = state.script.pop_front() {
            Some(reply)
        } else if state.foreign_replies {
            Some(Reply { status: 200, body: b"<html>ok</html>".to_vec(), retry_after: None })
        } else {
            let accepted = serde_json::from_slice::<Envelope>(&body).map(|envelope| envelope.items.len()).unwrap_or(0);
            let body = format!(r#"{{"accepted":{accepted},"duplicates":0,"rejected":0,"need_catalog":{}}}"#, state.need_catalog);
            Some(Reply { status: 200, body: body.into_bytes(), retry_after: None })
        };
        state.seen.push(Seen {
            path: request.path.to_owned(),
            key: request.app_key.to_owned(),
            gzip: request.gzip,
            body,
            status: reply.as_ref().map_or(0, |reply| reply.status),
            at: Instant::now(),
        });
        reply.ok_or_else(|| TransportError::Network("offline".to_owned()))
    }
}

impl Server {
    fn offline() -> Self {
        let server = Self::default();
        server.set_offline(true);
        server
    }

    fn set_offline(&self, offline: bool) {
        self.0.lock().unwrap().offline = offline;
    }

    fn set_delay(&self, delay: Duration) {
        self.0.lock().unwrap().delay = delay;
    }

    fn set_foreign_replies(&self, on: bool) {
        self.0.lock().unwrap().foreign_replies = on;
    }

    fn set_need_catalog(&self, need: bool) {
        self.0.lock().unwrap().need_catalog = need;
    }

    fn script(&self, status: u16, retry_after: Option<Duration>) {
        self.0.lock().unwrap().script.push_back(Reply { status, body: b"{}".to_vec(), retry_after });
    }

    fn seen(&self) -> Vec<Seen> {
        self.0.lock().unwrap().seen.clone()
    }

    /// Envelopes the server stored, in order.
    fn envelopes(&self) -> Vec<Envelope> {
        let stored = self.seen().into_iter().filter(|seen| seen.path == routes::INGEST && seen.status == 200);
        stored.map(|seen| serde_json::from_slice(&seen.body).unwrap()).collect()
    }

    /// Every stored item with the envelope it arrived in.
    fn items(&self) -> Vec<(Envelope, Item)> {
        let mut items = Vec::new();
        for envelope in self.envelopes() {
            let header = Envelope { items: Vec::new(), ..envelope.clone() };
            items.extend(envelope.items.into_iter().map(|item| (header.clone(), item)));
        }
        items
    }

    /// Stored usage events as (name, dynamic).
    fn events(&self) -> Vec<(String, bool)> {
        let events = self.items().into_iter().filter_map(|(_, item)| match item.body {
            ItemBody::Event(event) => Some((event.name, event.dynamic)),
            _ => None,
        });
        events.collect()
    }

    /// Drop counts the server received, added up by reason.
    fn drops(&self) -> BTreeMap<String, u64> {
        let mut total = BTreeMap::new();
        for (_, item) in self.items() {
            if let ItemBody::Drops(drops) = item.body {
                for (reason, count) in drops.reasons {
                    *total.entry(reason).or_insert(0) += count;
                }
            }
        }
        total
    }

    fn forgets(&self) -> Vec<ForgetRequest> {
        let stored = self.seen().into_iter().filter(|seen| seen.path == routes::FORGET && seen.status == 200);
        stored.map(|seen| serde_json::from_slice(&seen.body).unwrap()).collect()
    }
}

/// A builder with short intervals, only the session and usage signals, and every event listened to.
fn builder(dir: &TempDir, server: &Server) -> Builder {
    Tracer::builder(KEY, "host", "1.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(server.clone())
        .send_before_notice(true)
        .signals(&[Signal::Session, Signal::Usage])
        .catalog::<AppEvent>()
        .listen_all::<AppEvent>()
        .configure(|config| {
            config.tick_interval = Duration::from_millis(5);
            config.flush_interval = Duration::from_millis(20);
            config.retry_min = Duration::from_millis(20);
            config.retry_max = Duration::from_millis(80);
            config.heartbeat_interval = Duration::from_secs(3600);
            config.shutdown_timeout = Duration::from_secs(5);
        })
}

/// The kill switches come from the environment, so with `DO_NOT_TRACK` set in the shell that runs the
/// tests, a test here would find the SDK switched off. When a switch is set, the named test is run again
/// in a child process without it. True means that happened and the caller has nothing left to do.
fn reran_without_kill_switches(test: &str) -> bool {
    const SWITCHES: [&str; 2] = ["DO_NOT_TRACK", "TRACER_DISABLED"];
    if SWITCHES.iter().all(|name| std::env::var_os(name).is_none()) {
        return false;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", test, "--test-threads=1"]);
    for name in SWITCHES {
        child.env_remove(name);
    }
    let output = child.output().unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    // "1 passed" guards against a mistyped name, which would run nothing and still succeed.
    assert!(output.status.success() && report.contains("1 passed"), "{test} in a child process without kill switches:\n{report}");
    true
}

fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Total size of the files under a folder.
fn disk_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        total += if metadata.is_dir() { disk_bytes(&entry.path()) } else { metadata.len() };
    }
    total
}

/// Bytes of items waiting in the spool: the segment files of every session folder. The folder of a
/// running session also holds its lock, header and marker, which are not waiting for anything.
fn pending_bytes(dir: &TempDir) -> u64 {
    let mut total = 0;
    for session in std::fs::read_dir(dir.sdk().join("spool")).into_iter().flatten().flatten() {
        for file in std::fs::read_dir(session.path()).into_iter().flatten().flatten() {
            if file.path().extension().is_some_and(|extension| extension == "jsonl") {
                total += file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// The folder a running tracer owns in the spool.
fn session_dir(dir: &TempDir, tracer: &Tracer) -> PathBuf {
    dir.sdk().join("spool").join(tracer.session_id())
}

/// Size of the spool at one moment. The worker may be evicting and appending while the folder is read,
/// and a walk that straddles that would count a file that is gone together with one that replaced it, so
/// this reads until two walks in a row agree.
fn spool_bytes(dir: &TempDir) -> u64 {
    fn listing(dir: &Path, files: &mut BTreeMap<PathBuf, u64>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let Ok(metadata) = entry.metadata() else { continue };
            if metadata.is_dir() {
                listing(&entry.path(), files);
            } else {
                files.insert(entry.path(), metadata.len());
            }
        }
    }
    let snapshot = || {
        let mut files = BTreeMap::new();
        listing(&dir.sdk().join("spool"), &mut files);
        files
    };
    let mut previous = snapshot();
    for _ in 0..1000 {
        let current = snapshot();
        if current == previous {
            break;
        }
        previous = current;
    }
    previous.values().sum()
}

fn is_session_item(item: &Item) -> bool {
    matches!(item.body, ItemBody::SessionStart(_) | ItemBody::SessionHeartbeat(_) | ItemBody::SessionEnd(_))
}

// ---- usage ----

#[test]
fn only_listened_events_are_recorded() {
    if reran_without_kill_switches("only_listened_events_are_recorded") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = Tracer::builder(KEY, "host", "1.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(server.clone())
        .send_before_notice(true)
        .signals(&[Signal::Session, Signal::Usage])
        .catalog::<AppEvent>()
        .listen(Category::File)
        .listen_events(&[AppEvent::Undo])
        .inspect(true)
        .configure(|config| config.tick_interval = Duration::from_millis(5))
        .init();
    assert!(tracer.is_enabled());
    tracer.track(AppEvent::FileOpen);
    tracer.track(AppEvent::Zoom);
    tracer.track(AppEvent::Undo);
    tracer.track_id(Category::File, "file.export_pdf");
    tracer.track_id(Category::Edit, "edit.redo");
    tracer.track_id(Category::View, "view.reset");
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events(), vec![("file.open".to_owned(), false), ("edit.undo".to_owned(), false), ("file.export_pdf".to_owned(), true)]);
    // Not listened to means not recorded anywhere, and not a drop either.
    let outbox = std::fs::read_to_string(dir.sdk().join("outbox.jsonl")).unwrap();
    assert!(outbox.contains("session_start"));
    let mirrored: Vec<&str> = outbox.lines().filter(|line| line.contains(r#""type":"event""#)).collect();
    assert_eq!(mirrored.len(), 3);
    assert!(!mirrored.iter().any(|line| line.contains("view.") || line.contains("edit.redo")));
    assert!(server.drops().is_empty());
    let start = server.items().into_iter().find_map(|(_, item)| match item.body {
        ItemBody::SessionStart(start) => Some(start),
        _ => None,
    });
    let start = start.unwrap();
    assert_eq!(start.listening, vec!["file".to_owned()]);
    assert_eq!(start.signals, vec!["session".to_owned(), "usage".to_owned()]);
}

#[test]
fn track_id_and_property_limits_are_enforced_and_counted() {
    if reran_without_kill_switches("track_id_and_property_limits_are_enforced_and_counted") {
        return;
    }
    const LONG: &str = "this-value-is-longer-than-sixty-four-bytes-and-so-it-must-be-left-out";
    const KEYS: [&str; 18] =
        ["p00", "p01", "p02", "p03", "p04", "p05", "p06", "p07", "p08", "p09", "p10", "p11", "p12", "p13", "p14", "p15", "p16", "p17"];
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = builder(&dir, &server).init();
    tracer.track_id(Category::File, "Not A Valid Id");
    tracer.track_id(Category::File, "");
    tracer.track_id(Category::File, "file.valid_id");
    tracer.track_with(
        AppEvent::FileSave,
        &[("format", Prop::Str("pdf")), ("pages", Prop::Int(12)), ("ok", Prop::Bool(true)), ("too_long", Prop::Str(LONG)), ("Bad-Key", Prop::Int(1))],
    );
    let many: Vec<(&'static str, Prop)> = KEYS.iter().map(|key| (*key, Prop::Float(1.5))).collect();
    tracer.track_with(AppEvent::Zoom, &many);
    assert!(tracer.flush_blocking(WAIT));

    assert_eq!(server.events(), vec![("file.valid_id".to_owned(), true), ("file.save".to_owned(), false), ("view.zoom".to_owned(), false)]);
    let props: Vec<BTreeMap<String, PropValue>> = server
        .items()
        .into_iter()
        .filter_map(|(_, item)| match item.body {
            ItemBody::Event(event) if !event.dynamic => Some(event.props),
            _ => None,
        })
        .collect();
    let expected: BTreeMap<String, PropValue> =
        [("format".to_owned(), PropValue::Str("pdf".to_owned())), ("pages".to_owned(), PropValue::Int(12)), ("ok".to_owned(), PropValue::Bool(true))]
            .into();
    assert_eq!(props[0], expected);
    assert_eq!(props[1].len(), 16);
    let drops = server.drops();
    assert_eq!(drops.get("invalid_id"), Some(&2));
    assert_eq!(drops.get("prop_dropped"), Some(&4));
}

// ---- consent and kill switches ----

#[test]
fn consent_off_sends_nothing_for_the_category_and_removes_its_unsent_items() {
    if reran_without_kill_switches("consent_off_sends_nothing_for_the_category_and_removes_its_unsent_items") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::offline();
    let tracer = builder(&dir, &server).init();
    for _ in 0..20 {
        tracer.track(AppEvent::FileOpen);
    }
    assert!(!tracer.flush_blocking(WAIT));
    assert!(pending_bytes(&dir) > 0);
    assert!(tracer.consent(ConsentCategory::Usage));

    tracer.set_consent(ConsentCategory::Usage, false);
    assert!(!tracer.consent(ConsentCategory::Usage));
    for _ in 0..5 {
        tracer.track(AppEvent::FileOpen);
    }
    server.set_offline(false);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(pending_bytes(&dir), 0);
    assert!(server.events().is_empty());
    assert!(!server.items().iter().any(|(_, item)| is_session_item(item)));

    // The choice is saved, and holds for the next run until it is changed back.
    drop(tracer);
    let tracer = builder(&dir, &server).init();
    assert!(!tracer.consent(ConsentCategory::Usage));
    assert!(tracer.consent(ConsentCategory::Crash) && tracer.consent(ConsentCategory::Performance));
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    assert!(server.items().is_empty());
    tracer.set_consent(ConsentCategory::Usage, true);
    tracer.track(AppEvent::FileSave);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events(), vec![("file.save".to_owned(), false)]);
}

#[test]
fn the_notice_is_pending_until_shown() {
    if reran_without_kill_switches("the_notice_is_pending_until_shown") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = builder(&dir, &server).init();
    assert!(tracer.notice_pending());
    // Opt-out model: recording does not wait for the notice.
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events().len(), 1);
    tracer.notice_shown();
    assert!(!tracer.notice_pending());
    drop(tracer);
    assert!(!builder(&dir, &server).init().notice_pending());
    assert!(!Tracer::disabled().notice_pending());
}

/// Not a test of its own: the scenario [`kill_switches_make_no_request_and_write_no_spool`] runs in a
/// child process, because the switches are environment variables and those are process-wide.
#[test]
fn kill_switch_child() {
    let Some(dir) = std::env::var_os("TRACER_TEST_CHILD_DIR").map(PathBuf::from) else { return };
    let server = Server::default();
    let tracer = Tracer::builder(KEY, "host", "1.0.0")
        .data_dir(&dir)
        .endpoint("https://telemetry.example.com")
        .transport(server.clone())
        .send_before_notice(true)
        .catalog::<AppEvent>()
        .listen_all::<AppEvent>()
        .inspect(true)
        .configure(|config| {
            config.tick_interval = Duration::from_millis(5);
            config.flush_interval = Duration::from_millis(10);
        })
        .init();
    for _ in 0..10 {
        tracer.track(AppEvent::FileOpen);
    }
    let flushed = tracer.flush_blocking(WAIT);
    let install_id = tracer.install_id();
    drop(tracer);
    std::fs::write(dir.join("result.txt"), format!("requests={} flushed={flushed} install_id_len={}", server.seen().len(), install_id.len()))
        .unwrap();
}

fn run_child(variable: Option<(&str, &str)>) -> (TempDir, String) {
    let dir = TempDir::new();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "kill_switch_child", "--test-threads=1"]);
    command.env_remove("DO_NOT_TRACK").env_remove("TRACER_DISABLED").env("TRACER_TEST_CHILD_DIR", dir.path());
    if let Some((name, value)) = variable {
        command.env(name, value);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "child failed: {}", String::from_utf8_lossy(&output.stdout));
    let result = std::fs::read_to_string(dir.path().join("result.txt")).unwrap();
    (dir, result)
}

#[test]
fn kill_switches_make_no_request_and_write_no_spool() {
    for (name, value) in [("DO_NOT_TRACK", "1"), ("TRACER_DISABLED", "1"), ("DO_NOT_TRACK", "true"), ("TRACER_DISABLED", "yes")] {
        let (dir, result) = run_child(Some((name, value)));
        assert_eq!(result, "requests=0 flushed=true install_id_len=0", "{name}={value}");
        // Not only no spool: the SDK's folder was never created.
        assert!(!dir.sdk().exists(), "{name}={value}");
    }
    // Empty, `0` and `false` leave the SDK on, as does no variable at all.
    for variable in [None, Some(("DO_NOT_TRACK", "0")), Some(("DO_NOT_TRACK", "")), Some(("TRACER_DISABLED", "false"))] {
        let (dir, result) = run_child(variable);
        assert!(result.ends_with("flushed=true install_id_len=32") && !result.starts_with("requests=0 "), "{variable:?}: {result}");
        assert!(dir.sdk().join("install.json").exists(), "{variable:?}");
        assert!(dir.sdk().join("outbox.jsonl").exists(), "{variable:?}");
    }
}

// ---- delivery ----

#[test]
fn offline_then_online_delivers_the_backlog_exactly_once() {
    if reran_without_kill_switches("offline_then_online_delivers_the_backlog_exactly_once") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::offline();
    let tracer = builder(&dir, &server).init();
    let first_session = tracer.session_id();
    for _ in 0..150 {
        tracer.track(AppEvent::FileOpen);
    }
    assert!(!tracer.flush_blocking(WAIT));
    // Several attempts fail while offline; none of them may cost or duplicate anything.
    wait_for("retries while offline", || server.seen().len() >= 3);
    for _ in 0..50 {
        tracer.track(AppEvent::FileSave);
    }
    // The first run ends still offline, and a second run finds its backlog.
    drop(tracer);
    assert!(server.envelopes().is_empty());
    let tracer = builder(&dir, &server).init();
    for _ in 0..25 {
        tracer.track(AppEvent::Undo);
    }
    assert!(!tracer.flush_blocking(WAIT));
    server.set_offline(false);
    assert!(tracer.flush_blocking(WAIT));

    let items = server.items();
    let ids: HashSet<&str> = items.iter().map(|(_, item)| item.id.as_str()).collect();
    assert_eq!(ids.len(), items.len(), "an item was delivered twice");
    let count = |name: &str| server.events().iter().filter(|(event, _)| event == name).count();
    assert_eq!((count("file.open"), count("file.save"), count("edit.undo")), (150, 50, 25));
    // Each session's items arrive under that session's own header.
    for (envelope, item) in &items {
        match &item.body {
            ItemBody::Event(event) if event.name == "edit.undo" => assert_eq!(envelope.session_id, tracer.session_id()),
            ItemBody::Event(_) => assert_eq!(envelope.session_id, first_session),
            _ => {}
        }
        assert_eq!((envelope.schema, envelope.install_id.as_str()), (SCHEMA_VERSION, tracer.install_id().as_str()));
        assert_eq!((envelope.app.name.as_str(), envelope.app.version.as_str()), ("host", "1.0.0"));
        assert!(envelope.sent_at >= item.ts);
    }
    let ends: Vec<_> = items.iter().filter(|(_, item)| matches!(item.body, ItemBody::SessionEnd(_))).collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0].0.session_id, first_session);
    // Nothing is left, and nothing more is sent once there is nothing to send.
    assert_eq!(pending_bytes(&dir), 0);
    let requests = server.seen().len();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(server.seen().len(), requests);
    assert!(server.seen().iter().all(|seen| seen.gzip && seen.key == KEY && seen.path == routes::INGEST));
    assert_eq!(HEADER_APP_KEY, "x-tracer-key");
}

#[test]
fn a_large_backlog_is_split_into_envelopes_under_the_size_limit() {
    if reran_without_kill_switches("a_large_backlog_is_split_into_envelopes_under_the_size_limit") {
        return;
    }
    const TEXT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const KEYS: [&str; 16] = [
        "property_00",
        "property_01",
        "property_02",
        "property_03",
        "property_04",
        "property_05",
        "property_06",
        "property_07",
        "property_08",
        "property_09",
        "property_10",
        "property_11",
        "property_12",
        "property_13",
        "property_14",
        "property_15",
    ];
    let dir = TempDir::new();
    let server = Server::offline();
    let tracer = builder(&dir, &server).init();
    let props: Vec<(&'static str, Prop)> = KEYS.iter().map(|key| (*key, Prop::Str(TEXT))).collect();
    let total = 1500;
    for chunk in 0..total / 500 {
        for _ in 0..500 {
            tracer.track_with(AppEvent::FileSave, &props);
        }
        assert!(!tracer.flush_blocking(WAIT), "chunk {chunk}");
    }
    assert!(pending_bytes(&dir) > MAX_ENVELOPE_BYTES as u64);
    server.set_offline(false);
    assert!(tracer.flush_blocking(WAIT));
    let stored: Vec<Seen> = server.seen().into_iter().filter(|seen| seen.status == 200).collect();
    assert!(stored.len() >= 2);
    assert!(stored.iter().all(|seen| seen.body.len() <= MAX_ENVELOPE_BYTES));
    assert_eq!(server.events().len(), total);
    assert!(server.drops().is_empty());
}

#[test]
fn retry_after_is_honoured() {
    if reran_without_kill_switches("retry_after_is_honoured") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let pause = Duration::from_millis(600);
    server.script(429, Some(pause));
    let tracer = builder(&dir, &server).init();
    tracer.track(AppEvent::FileOpen);
    wait_for("the rate-limited request", || !server.seen().is_empty());
    // Neither the flush interval (20 ms) nor explicit flushes go ahead of the server's wait. The proof
    // is in the time between the two requests, so it does not depend on how fast this thread runs.
    for _ in 0..5 {
        tracer.flush_blocking(Duration::from_millis(20));
    }
    wait_for("delivery after the wait", || server.events().len() == 1);
    let seen = server.seen();
    assert_eq!(seen[0].status, 429);
    assert!(seen[1].at.duration_since(seen[0].at) >= pause, "retried after {:?}", seen[1].at.duration_since(seen[0].at));
    assert!(tracer.flush_blocking(WAIT));
    let ids: Vec<String> = server.items().into_iter().map(|(_, item)| item.id).collect();
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
}

#[test]
fn server_errors_are_retried_with_backoff() {
    if reran_without_kill_switches("server_errors_are_retried_with_backoff") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    for status in [500, 503, 429, 502] {
        server.script(status, None);
    }
    let tracer = builder(&dir, &server).init();
    tracer.track(AppEvent::FileOpen);
    wait_for("delivery after the failures", || server.events().len() == 1);
    let seen = server.seen();
    assert_eq!(seen.iter().map(|seen| seen.status).collect::<Vec<_>>()[..5], [500, 503, 429, 502, 200]);
    // The wait before each retry is at least half the backoff floor (20 ms), whatever the jitter.
    for pair in seen[..5].windows(2) {
        assert!(pair[1].at.duration_since(pair[0].at) >= Duration::from_millis(10));
    }
    assert!(tracer.flush_blocking(WAIT));
}

#[test]
fn an_unknown_key_stops_sending_but_keeps_the_data() {
    if reran_without_kill_switches("an_unknown_key_stops_sending_but_keeps_the_data") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    server.script(401, None);
    let tracer = builder(&dir, &server).init();
    for _ in 0..7 {
        tracer.track(AppEvent::FileOpen);
    }
    assert!(!tracer.flush_blocking(WAIT));
    assert!(!tracer.flush_blocking(WAIT));
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(server.seen().len(), 1, "nothing more may be sent after a 401");
    assert!(pending_bytes(&dir) > 0);
    drop(tracer);
    assert_eq!(server.seen().len(), 1);

    // "Until restart": the next run tries again, and nothing was lost.
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events().len(), 7);
}

#[test]
fn a_rejected_batch_is_deleted_and_counted() {
    if reran_without_kill_switches("a_rejected_batch_is_deleted_and_counted") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::offline();
    let tracer = builder(&dir, &server).init();
    for _ in 0..4 {
        tracer.track(AppEvent::FileOpen);
    }
    assert!(!tracer.flush_blocking(WAIT));
    server.script(400, None);
    server.set_offline(false);
    wait_for("the rejection", || server.seen().iter().any(|seen| seen.status == 400));
    tracer.track(AppEvent::FileSave);
    wait_for("the report of the rejection", || server.drops().contains_key("server_rejected"));
    assert!(tracer.flush_blocking(WAIT));
    // The session start and the four events went with the refused batch.
    assert_eq!(server.drops().get("server_rejected"), Some(&5));
    assert_eq!(server.events(), vec![("file.save".to_owned(), false)]);
}

#[test]
fn the_spool_stays_under_its_cap_and_evictions_are_reported() {
    if reran_without_kill_switches("the_spool_stays_under_its_cap_and_evictions_are_reported") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::offline();
    let cap = 24 * 1024;
    let tracer = builder(&dir, &server).configure(|config| config.spool_max_bytes = cap).init();
    let total = 3000u64;
    let mut fullest = 0;
    for _ in 0..total / 100 {
        for _ in 0..100 {
            tracer.track(AppEvent::FileOpen);
        }
        assert!(!tracer.flush_blocking(WAIT));
        let bytes = spool_bytes(&dir);
        assert!(bytes <= cap, "{bytes} bytes spooled under a cap of {cap}");
        fullest = fullest.max(bytes);
    }
    // The cap was really reached, not merely never approached.
    assert!(fullest > cap * 3 / 4, "{fullest}");
    server.set_offline(false);
    assert!(tracer.flush_blocking(WAIT));
    // One more round so that counts taken during the last cycle are sent too.
    tracer.track(AppEvent::FileSave);
    assert!(tracer.flush_blocking(WAIT));

    let drops = server.drops();
    let evicted = drops.get("spool_evicted").copied().unwrap_or(0);
    let delivered = server.items().iter().filter(|(_, item)| !matches!(item.body, ItemBody::Drops(_))).count() as u64;
    assert!(evicted > total / 2, "only {evicted} evicted");
    // Every item is accounted for: delivered, or counted as dropped. The extra two are the session
    // start and the closing event.
    assert_eq!(delivered + evicted + drops.get("queue_full").copied().unwrap_or(0), total + 2, "{drops:?}");
    assert_eq!(pending_bytes(&dir), 0);
}

#[test]
fn flush_blocking_reports_whether_everything_was_sent() {
    if reran_without_kill_switches("flush_blocking_reports_whether_everything_was_sent") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = builder(&dir, &server).configure(|config| config.flush_interval = Duration::from_secs(3600)).init();
    assert!(tracer.flush_blocking(WAIT));
    for _ in 0..30 {
        tracer.track(AppEvent::FileOpen);
    }
    // No interval is due: only the flush can have sent these.
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events().len(), 30);
    assert_eq!(pending_bytes(&dir), 0);

    // Offline, a flush still returns as soon as the attempt has failed, with data left.
    server.set_offline(true);
    tracer.track(AppEvent::FileOpen);
    assert!(!tracer.flush_blocking(WAIT));
    server.set_offline(false);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events().len(), 31);

    // With a request that hangs, it gives up at its timeout instead of waiting the request out.
    server.set_delay(Duration::from_millis(1500));
    tracer.track(AppEvent::FileOpen);
    let started = Instant::now();
    assert!(!tracer.flush_blocking(Duration::from_millis(100)));
    assert!(started.elapsed() >= Duration::from_millis(100) && started.elapsed() < Duration::from_millis(1200), "{:?}", started.elapsed());
    server.set_delay(Duration::ZERO);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events().len(), 32);
    assert!(Tracer::disabled().flush_blocking(Duration::ZERO));
}

#[test]
fn a_full_batch_is_sent_without_waiting_for_the_interval() {
    if reran_without_kill_switches("a_full_batch_is_sent_without_waiting_for_the_interval") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = builder(&dir, &server)
        .configure(|config| {
            config.flush_interval = Duration::from_secs(3600);
            config.batch_items = 10;
        })
        .init();
    wait_for("the startup send", || !server.envelopes().is_empty());
    for _ in 0..9 {
        tracer.track(AppEvent::FileOpen);
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(server.events().is_empty());
    tracer.track(AppEvent::FileOpen);
    wait_for("the batch", || server.events().len() == 10);
}

#[test]
fn without_an_endpoint_data_is_spooled_and_mirrored_but_never_sent() {
    if reran_without_kill_switches("without_an_endpoint_data_is_spooled_and_mirrored_but_never_sent") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = Tracer::builder(KEY, "host", "1.0.0")
        .data_dir(dir.path())
        .transport(server.clone())
        .send_before_notice(true)
        .catalog::<AppEvent>()
        .listen_all::<AppEvent>()
        .inspect(true)
        .configure(|config| {
            config.tick_interval = Duration::from_millis(5);
            config.flush_interval = Duration::from_millis(10);
            config.spool_max_bytes = 16 * 1024;
        })
        .init();
    for _ in 0..400 {
        tracer.track(AppEvent::FileOpen);
    }
    assert!(tracer.flush_blocking(WAIT));
    std::thread::sleep(Duration::from_millis(50));
    drop(tracer);
    assert!(server.seen().is_empty());
    let spooled = disk_bytes(&dir.sdk().join("spool"));
    assert!(spooled > 0 && spooled <= 16 * 1024);
    let outbox = std::fs::read_to_string(dir.sdk().join("outbox.jsonl")).unwrap();
    assert_eq!(outbox.lines().filter(|line| line.contains(r#""type":"event""#)).count(), 400);
    assert!(outbox.lines().all(|line| serde_json::from_str::<Item>(line).is_ok()));
}

// ---- session ----

#[test]
fn a_clean_run_reports_its_start_heartbeats_and_end() {
    if reran_without_kill_switches("a_clean_run_reports_its_start_heartbeats_and_end") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let started = Instant::now();
    let tracer = builder(&dir, &server).configure(|config| config.heartbeat_interval = Duration::from_millis(250)).init();
    let marker = session_dir(&dir, &tracer).join("session.json");
    assert!(marker.exists());
    assert_eq!(tracer.session_id().len(), 32);
    assert_eq!(tracer.install_id().len(), 32);
    tracer.set_focused(false);
    // Long enough for a whole second and for two heartbeats, however busy the machine is.
    std::thread::sleep(Duration::from_millis(1150));
    let heartbeats = || server.items().iter().filter(|(_, item)| matches!(item.body, ItemBody::SessionHeartbeat(_))).count();
    wait_for("two heartbeats", || heartbeats() >= 2);
    let session = tracer.session_id();
    drop(tracer);
    let longest = started.elapsed();
    // Everything was sent, so nothing of the session is left on disk: no marker, no folder.
    assert!(!marker.exists());
    assert_eq!(std::fs::read_dir(dir.sdk().join("spool")).unwrap().count(), 0);

    assert!(server.envelopes().iter().all(|envelope| envelope.session_id == session));
    let items: Vec<Item> = server.items().into_iter().map(|(_, item)| item).collect();
    assert!(matches!(items[0].body, ItemBody::SessionStart(_)));
    let beats: Vec<u32> = items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::SessionHeartbeat(totals) => Some(totals.seq),
            _ => None,
        })
        .collect();
    assert!(beats.len() >= 2 && beats.len() as u128 <= longest.as_millis() / 250, "{beats:?} in {longest:?}");
    assert_eq!(beats, (1..=beats.len() as u32).collect::<Vec<_>>());
    let ItemBody::SessionEnd(end) = &items.last().unwrap().body else { panic!("the last item is not the session end") };
    assert_eq!(end.status, SessionStatus::Exited);
    assert_eq!(end.errors, 0);
    assert_eq!(end.totals.seq as usize, beats.len());
    // At least the second slept through, and no more than the test itself took.
    assert!(end.totals.duration_s >= 1 && end.totals.duration_s <= longest.as_secs(), "{:?} in {longest:?}", end.totals);
    // Unfocused almost from the start: all of it is idle, none of it focused or active.
    assert_eq!((end.totals.focused_s, end.totals.active_s, end.totals.idle_s), (0, 0, end.totals.duration_s));
}

#[test]
fn input_makes_focused_time_active() {
    if reran_without_kill_switches("input_makes_focused_time_active") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = builder(&dir, &server).init();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1200) {
        tracer.note_input();
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(tracer);
    let end = server.items().into_iter().find_map(|(_, item)| match item.body {
        ItemBody::SessionEnd(end) => Some(end),
        _ => None,
    });
    let totals = end.unwrap().totals;
    // Focused by default, with input throughout: all of it is focused and active, none idle. Active
    // time starts a moment after the session does, so its whole seconds may be one behind.
    assert!(totals.duration_s >= 1, "{totals:?}");
    assert_eq!((totals.focused_s, totals.idle_s), (totals.duration_s, 0), "{totals:?}");
    assert!(totals.active_s >= 1 && totals.active_s + 1 >= totals.duration_s, "{totals:?}");
}

/// Runs a session, then leaves its folder as a killed process would: the marker still there (after the
/// clean shutdown removed it), no lock held, and none of the items it had recorded. `edit` can change the
/// marker first. The two-process tests further down do this with a process that really is killed.
fn leave_marker_behind(dir: &TempDir, server: &Server, edit: impl FnOnce(&mut serde_json::Value)) -> String {
    let tracer = builder(dir, server).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    let session = tracer.session_id();
    let folder = session_dir(dir, &tracer);
    let mut marker: serde_json::Value = serde_json::from_slice(&std::fs::read(folder.join("session.json")).unwrap()).unwrap();
    drop(tracer);
    assert!(!folder.join("session.json").exists());
    edit(&mut marker);
    let _ = std::fs::remove_dir_all(dir.sdk().join("spool"));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("lock"), b"").unwrap();
    std::fs::write(folder.join("session.json"), serde_json::to_vec(&marker).unwrap()).unwrap();
    session
}

#[test]
fn a_leftover_marker_is_reported_as_an_abnormal_end_and_an_unclean_exit() {
    if reran_without_kill_switches("a_leftover_marker_is_reported_as_an_abnormal_end_and_an_unclean_exit") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::offline();
    let previous = leave_marker_behind(&dir, &server, |marker| {
        marker["totals"] = serde_json::json!({"seq": 4, "duration_s": 240, "focused_s": 200, "active_s": 90, "idle_s": 150});
        marker["errors"] = serde_json::json!(3);
    });
    server.set_offline(false);

    // The next run is a newer version; the report must still carry the previous run's header.
    let tracer = Tracer::builder(KEY, "host", "2.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(server.clone())
        .send_before_notice(true)
        .signals(&[Signal::Session, Signal::Usage, Signal::Crash])
        .configure(|config| config.tick_interval = Duration::from_millis(5))
        .init();
    assert_ne!(tracer.session_id(), previous);
    assert!(tracer.flush_blocking(WAIT));

    let recovered: Vec<(Envelope, Item)> = server.items().into_iter().filter(|(envelope, _)| envelope.session_id == previous).collect();
    assert_eq!(recovered.len(), 2, "{recovered:?}");
    for (envelope, _) in &recovered {
        assert_eq!(envelope.app.version, "1.0.0");
        assert_eq!(envelope.install_id, tracer.install_id());
    }
    let ItemBody::SessionEnd(end) = &recovered[0].1.body else { panic!("expected a session end first") };
    assert_eq!(end.status, SessionStatus::Abnormal);
    assert_eq!((end.totals.seq, end.totals.duration_s, end.totals.focused_s, end.totals.active_s, end.totals.idle_s), (4, 240, 200, 90, 150));
    assert_eq!(end.errors, 3);
    let ItemBody::Crash(crash) = &recovered[1].1.body else { panic!("expected a crash second") };
    assert_eq!(crash.kind, CrashKind::UncleanExit);
    assert!(crash.fatal);
    // The new session is its own envelope, under the new version.
    let current: Vec<(Envelope, Item)> = server.items().into_iter().filter(|(envelope, _)| envelope.session_id == tracer.session_id()).collect();
    assert!(current.iter().all(|(envelope, _)| envelope.app.version == "2.0.0"));
    assert!(matches!(current[0].1.body, ItemBody::SessionStart(_)));

    // Reported once: a third run finds a clean shutdown and has nothing to add.
    drop(tracer);
    let before = server.items().len();
    let tracer = builder(&dir, &server).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    assert!(tracer.flush_blocking(WAIT));
    let later: Vec<(Envelope, Item)> = server.items().into_iter().skip(before).collect();
    assert!(later.iter().all(|(_, item)| matches!(item.body, ItemBody::SessionStart(_))), "{later:?}");
}

#[test]
fn a_leftover_marker_with_the_panicked_flag_is_a_crashed_end_and_no_extra_crash() {
    if reran_without_kill_switches("a_leftover_marker_with_the_panicked_flag_is_a_crashed_end_and_no_extra_crash") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::offline();
    let previous = leave_marker_behind(&dir, &server, |marker| {
        marker["panicked"] = serde_json::json!(true);
        // Two panics the host caught earlier, and the one that ended it.
        marker["errors"] = serde_json::json!(3);
    });
    server.set_offline(false);
    let tracer = builder(&dir, &server).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    assert!(tracer.flush_blocking(WAIT));
    let recovered: Vec<Item> = server.items().into_iter().filter(|(envelope, _)| envelope.session_id == previous).map(|(_, item)| item).collect();
    assert_eq!(recovered.len(), 1, "{recovered:?}");
    // `crashed` already accounts for the last panic; `errors` holds the two that were survived.
    assert!(matches!(&recovered[0].body, ItemBody::SessionEnd(end) if end.status == SessionStatus::Crashed && end.errors == 2), "{recovered:?}");
    assert!(!server.items().iter().any(|(_, item)| matches!(item.body, ItemBody::Crash(_))));
}

#[test]
fn a_leftover_marker_respects_consent_and_disabled_signals() {
    if reran_without_kill_switches("a_leftover_marker_respects_consent_and_disabled_signals") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::offline();
    let previous = leave_marker_behind(&dir, &server, |_| {});
    server.set_offline(false);
    // The crash signal is not enabled in this run, so only the session end is reported.
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    let recovered: Vec<Item> = server.items().into_iter().filter(|(envelope, _)| envelope.session_id == previous).map(|(_, item)| item).collect();
    assert_eq!(recovered.len(), 1);
    assert!(matches!(&recovered[0].body, ItemBody::SessionEnd(end) if end.status == SessionStatus::Abnormal));
}

// ---- ending explicitly (short-lived processes) ----

fn ends(server: &Server, session: &str) -> Vec<SessionStatus> {
    let ends = server.items().into_iter().filter(|(envelope, _)| envelope.session_id == session).filter_map(|(_, item)| match item.body {
        ItemBody::SessionEnd(end) => Some(end.status),
        _ => None,
    });
    ends.collect()
}

#[test]
fn shutdown_ends_the_session_for_a_process_that_exits_without_dropping() {
    if reran_without_kill_switches("shutdown_ends_the_session_for_a_process_that_exits_without_dropping") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::offline();
    let tracer = builder(&dir, &server).process(ashlar_trace::ProcessKind::Cli).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    let session = tracer.session_id();
    let marker = session_dir(&dir, &tracer).join("session.json");
    assert!(marker.exists());
    let clone = tracer.clone();
    tracer.track(AppEvent::FileOpen);
    // Offline: nothing can be sent, so it is not clean, but the end is on disk and the marker gone.
    assert!(!tracer.shutdown_within(Duration::from_millis(200)));
    assert!(!marker.exists());
    // Again, and on a clone: nothing more happens, and the answer is the same.
    assert!(!tracer.shutdown());
    assert!(!clone.shutdown_within(Duration::from_secs(5)));
    // After the end nothing is recorded.
    tracer.track(AppEvent::Undo);
    drop(clone);
    // As `std::process::exit` would: no handle is ever dropped.
    std::mem::forget(tracer);

    server.set_offline(false);
    let next = builder(&dir, &server).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    assert!(next.flush_blocking(WAIT));
    assert_eq!(ends(&server, &session), vec![SessionStatus::Exited]);
    assert!(!server.items().iter().any(|(_, item)| is_unclean_report(item)), "no unclean exit");
    assert_eq!(server.events(), vec![("file.open".to_owned(), false)]);
    let envelope = server.envelopes().into_iter().find(|envelope| envelope.session_id == session).unwrap();
    assert_eq!(envelope.app.process, ashlar_trace::ProcessKind::Cli);
}

#[test]
fn shutdown_within_waits_no_longer_than_it_is_told_for_a_slow_server() {
    if reran_without_kill_switches("shutdown_within_waits_no_longer_than_it_is_told_for_a_slow_server") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    server.set_delay(Duration::from_secs(3));
    let tracer = builder(&dir, &server).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    let session = tracer.session_id();
    let started = Instant::now();
    assert!(!tracer.shutdown_within(Duration::from_millis(100)));
    let took = started.elapsed();
    assert!(took < Duration::from_millis(1000), "{took:?}");
    std::mem::forget(tracer);
    // Whatever the slow request did not deliver goes with the next process.
    server.set_delay(Duration::ZERO);
    let next = builder(&dir, &server).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    wait_for("the earlier session's end", || ends(&server, &session) == vec![SessionStatus::Exited]);
    assert!(next.flush_blocking(WAIT));
    assert!(!server.items().iter().any(|(_, item)| is_unclean_report(item)), "no unclean exit");
}

#[test]
fn a_command_line_process_runs_no_hang_perf_or_sys_signals() {
    if reran_without_kill_switches("a_command_line_process_runs_no_hang_perf_or_sys_signals") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let cli = builder(&dir, &server).process(ashlar_trace::ProcessKind::Cli).signals(&Signal::ALL).init();
    assert!(cli.shutdown());
    let gui = builder(&dir, &server).signals(&Signal::ALL).init();
    assert!(gui.shutdown());
    let starts = session_starts(&server);
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0].signals, ["session", "usage", "crash", "logs"]);
    assert_eq!(starts[1].signals, ["session", "usage", "crash", "hang", "perf", "sys", "logs"]);
}

#[test]
fn a_process_that_ends_its_session_and_exits_is_not_an_unclean_exit() {
    if reran_without_kill_switches("a_process_that_ends_its_session_and_exits_is_not_an_unclean_exit") {
        return;
    }
    let dir = TempDir::new();
    let mut child = spawn_child("exiter", &dir);
    let session = read_report(&dir, "exiter.txt", &mut child);
    assert_eq!(child.wait().unwrap().code(), Some(3));
    let server = Server::default();
    let next = builder(&dir, &server).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    assert!(next.flush_blocking(WAIT));
    assert_eq!(ends(&server, &session), vec![SessionStatus::Exited]);
    assert!(!server.items().iter().any(|(_, item)| is_unclean_report(item)), "no unclean exit");
    assert!(server.events().contains(&("file.open".to_owned(), false)));
}

#[test]
fn a_killed_process_that_records_no_session_and_no_crashes_is_not_an_unclean_exit() {
    if reran_without_kill_switches("a_killed_process_that_records_no_session_and_no_crashes_is_not_an_unclean_exit") {
        return;
    }
    let dir = TempDir::new();
    let mut child = spawn_child("quiet", &dir);
    let session = read_report(&dir, "quiet.txt", &mut child);
    assert!(!dir.sdk().join("spool").join(&session).join("session.json").exists(), "no marker");
    child.kill().unwrap();
    child.wait().unwrap();
    let server = Server::default();
    let next = builder(&dir, &server).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    assert!(next.flush_blocking(WAIT));
    // What it recorded is sent, and nothing reports it as a session that ended badly.
    assert!(server.events().contains(&("file.save".to_owned(), false)), "{:?}", server.events());
    assert!(server.items().iter().all(|(envelope, item)| envelope.session_id != session || matches!(item.body, ItemBody::Event(_))));
}

#[test]
fn a_courier_delivers_what_others_left_and_reports_unclean_exits_but_records_nothing_of_its_own() {
    if reran_without_kill_switches("a_courier_delivers_what_others_left_and_reports_unclean_exits_but_records_nothing_of_its_own") {
        return;
    }
    let dir = TempDir::new();
    let offline = Server::offline();
    // One run was killed; another ended cleanly but could not send.
    let mut victim = spawn_child("victim", &dir);
    let report = read_report(&dir, "victim.txt", &mut victim);
    let killed = report.split_once(' ').unwrap().0.to_owned();
    victim.kill().unwrap();
    victim.wait().unwrap();
    let ended = builder(&dir, &offline).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    let ended_session = ended.session_id();
    ended.track(AppEvent::FileOpen);
    assert!(!ended.shutdown_within(Duration::from_millis(50)));
    drop(ended);

    let server = Server::default();
    let courier = builder(&dir, &server).courier(true).signals(&[Signal::Session, Signal::Usage, Signal::Crash]).init();
    assert!(!session_dir(&dir, &courier).join("session.json").exists(), "no marker");
    assert!(courier.flush_blocking(WAIT));
    let own = courier.session_id();
    drop(courier);
    assert_eq!(ends(&server, &ended_session), vec![SessionStatus::Exited]);
    assert!(server.events().contains(&("file.open".to_owned(), false)));
    assert_eq!(ends(&server, &killed), vec![SessionStatus::Abnormal]);
    assert!(server.items().iter().any(|(envelope, item)| envelope.session_id == killed && is_unclean_report(item)));
    assert!(server.envelopes().iter().all(|envelope| envelope.session_id != own), "nothing of its own");
    // And it left nothing behind that the next process could take for a session.
    let next_server = Server::default();
    let next = builder(&dir, &next_server).init();
    assert!(next.flush_blocking(WAIT));
    assert!(next_server.envelopes().iter().all(|envelope| envelope.session_id == next.session_id()));
}

// ---- catalog ----

fn session_starts(server: &Server) -> Vec<ashlar_trace::proto::SessionStart> {
    let starts = server.items().into_iter().filter_map(|(_, item)| match item.body {
        ItemBody::SessionStart(start) => Some(start),
        _ => None,
    });
    starts.collect()
}

#[test]
fn the_catalog_is_sent_until_acknowledged_and_again_when_the_server_asks() {
    if reran_without_kill_switches("the_catalog_is_sent_until_acknowledged_and_again_when_the_server_asks") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::offline();

    // Run 1 never reaches the server, so nothing is acknowledged.
    drop(builder(&dir, &server).init());
    // Run 2 gets through, with its own start and the one left by run 1.
    server.set_offline(false);
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    let starts = session_starts(&server);
    assert_eq!(starts.len(), 2);
    let hash = starts[0].catalog_hash.clone().unwrap();
    for start in &starts {
        let catalog = start.catalog.as_ref().unwrap();
        let names: Vec<&str> = catalog.events.iter().map(|event| event.name.as_str()).collect();
        assert_eq!(names, vec!["edit.undo", "file.open", "file.save", "view.zoom"]);
        assert_eq!(catalog.events[0].category, "edit");
        assert_eq!(start.catalog_hash.as_ref(), Some(&hash));
    }

    // Run 3: acknowledged, so only the hash is sent. Then the server says it lost the catalog.
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    let start = session_starts(&server).pop().unwrap();
    assert_eq!((start.catalog, start.catalog_hash), (None, Some(hash.clone())));
    server.set_need_catalog(true);
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    server.set_need_catalog(false);

    // Run 4 sends it again, and run 5 is back to the hash alone.
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    assert!(session_starts(&server).pop().unwrap().catalog.is_some());
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    assert!(session_starts(&server).pop().unwrap().catalog.is_none());

    // A new app version has not been acknowledged, whatever the hash.
    let tracer = Tracer::builder(KEY, "host", "1.1.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(server.clone())
        .send_before_notice(true)
        .catalog::<AppEvent>()
        .configure(|config| config.tick_interval = Duration::from_millis(5))
        .init();
    assert!(tracer.flush_blocking(WAIT));
    let start = session_starts(&server).pop().unwrap();
    assert!(start.catalog.is_some());
    assert_eq!(start.catalog_hash, Some(hash));
}

// ---- forget ----

#[test]
fn forget_wipes_local_data_asks_the_server_and_changes_the_install_id() {
    if reran_without_kill_switches("forget_wipes_local_data_asks_the_server_and_changes_the_install_id") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = builder(&dir, &server).inspect(true).init();
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    let old_id = tracer.install_id();
    assert_eq!(server.envelopes()[0].install_id, old_id);

    // Some data is waiting locally when the user asks to be forgotten.
    server.set_offline(true);
    for _ in 0..10 {
        tracer.track(AppEvent::Undo);
    }
    assert!(!tracer.flush_blocking(WAIT));
    std::fs::write(session_dir(&dir, &tracer).join("breadcrumbs.jsonl"), b"{}\n").unwrap();
    assert!(pending_bytes(&dir) > 0);
    assert!(dir.sdk().join("outbox.jsonl").exists());

    tracer.forget();
    let new_id = tracer.install_id();
    assert_ne!(new_id, old_id);
    assert_eq!(new_id.len(), 32);
    assert_eq!(pending_bytes(&dir), 0);
    assert!(!dir.sdk().join("outbox.jsonl").exists());
    assert!(!session_dir(&dir, &tracer).join("breadcrumbs.jsonl").exists());
    let install = std::fs::read_to_string(dir.sdk().join("install.json")).unwrap();
    assert!(install.contains(&format!(r#""install_id":"{new_id}""#)));

    server.set_offline(false);
    tracer.track(AppEvent::FileSave);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.forgets(), vec![ForgetRequest { schema: SCHEMA_VERSION, install_id: old_id.clone() }]);
    let forget = server.seen().into_iter().find(|seen| seen.path == routes::FORGET && seen.status == 200).unwrap();
    assert_eq!(forget.key, KEY);
    // What was waiting is gone for good, and what follows is under the new id only.
    assert!(!server.events().iter().any(|(name, _)| name == "edit.undo"));
    let after: Vec<Envelope> = server.envelopes().into_iter().filter(|envelope| envelope.install_id != old_id).collect();
    assert!(!after.is_empty());
    assert!(after.iter().all(|envelope| envelope.install_id == new_id));
    assert!(after.iter().flat_map(|envelope| &envelope.items).any(|item| matches!(&item.body, ItemBody::Event(event) if event.name == "file.save")));
    let forget_index = server.seen().iter().position(|seen| seen.path == routes::FORGET && seen.status == 200).unwrap();
    assert!(
        server.seen()[..forget_index].iter().filter(|seen| seen.status == 200).all(|seen| !String::from_utf8_lossy(&seen.body).contains(&new_id))
    );

    // The request is not repeated, here or in the next run, and the new id sticks.
    drop(tracer);
    let tracer = builder(&dir, &server).init();
    assert_eq!(tracer.install_id(), new_id);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.forgets().len(), 1);
}

#[test]
fn a_forget_request_that_fails_is_retried_on_the_next_launch() {
    if reran_without_kill_switches("a_forget_request_that_fails_is_retried_on_the_next_launch") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    let old_id = tracer.install_id();
    server.set_offline(true);
    tracer.forget();
    assert!(!tracer.flush_blocking(WAIT));
    drop(tracer);
    assert!(server.forgets().is_empty());

    server.set_offline(false);
    let tracer = builder(&dir, &server).init();
    assert_ne!(tracer.install_id(), old_id);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.forgets(), vec![ForgetRequest { schema: SCHEMA_VERSION, install_id: old_id }]);
}

// ---- the disabled tracer ----

#[test]
fn a_tracer_without_a_data_dir_is_disabled_and_harmless() {
    if reran_without_kill_switches("a_tracer_without_a_data_dir_is_disabled_and_harmless") {
        return;
    }
    let server = Server::default();
    let tracer =
        Tracer::builder(KEY, "host", "1.0.0").endpoint("https://telemetry.example.com").transport(server.clone()).send_before_notice(true).init();
    assert!(!tracer.is_enabled());
    tracer.track(AppEvent::FileOpen);
    tracer.track_id(Category::File, "file.x");
    tracer.set_consent(ConsentCategory::Usage, false);
    tracer.forget();
    assert!(tracer.flush_blocking(Duration::ZERO));
    assert_eq!((tracer.install_id(), tracer.session_id()), (String::new(), String::new()));
    drop(tracer);
    assert!(server.seen().is_empty());
}

// ---- several processes on one data directory ----

/// Every item both servers stored, with the envelope it came in.
fn all_items(servers: &[&Server]) -> Vec<(Envelope, Item)> {
    servers.iter().flat_map(|server| server.items()).collect()
}

fn is_unclean_report(item: &Item) -> bool {
    match &item.body {
        ItemBody::Crash(crash) => crash.kind == CrashKind::UncleanExit,
        ItemBody::SessionEnd(end) => end.status != SessionStatus::Exited,
        _ => false,
    }
}

#[test]
fn two_tracers_on_one_data_directory_leave_each_other_alone() {
    if reran_without_kill_switches("two_tracers_on_one_data_directory_leave_each_other_alone") {
        return;
    }
    let dir = TempDir::new();
    let (first_server, second_server) = (Server::default(), Server::default());
    let signals = [Signal::Session, Signal::Usage, Signal::Crash];
    let first = builder(&dir, &first_server).signals(&signals).init();
    let second = builder(&dir, &second_server).signals(&signals).init();
    assert_eq!(first.install_id(), second.install_id());
    assert_ne!(first.session_id(), second.session_id());
    assert!(session_dir(&dir, &first).join("lock").exists() && session_dir(&dir, &second).join("session.json").exists());

    for _ in 0..5 {
        first.track(AppEvent::FileOpen);
    }
    for _ in 0..7 {
        second.track(AppEvent::Undo);
    }
    assert!(first.flush_blocking(WAIT) && second.flush_blocking(WAIT));

    // The first can no longer send, so its next items wait in its folder. The second, which sends every
    // few milliseconds, must not take them: not send them, not delete them.
    first_server.set_offline(true);
    for _ in 0..9 {
        first.track(AppEvent::FileSave);
    }
    assert!(!first.flush_blocking(WAIT));
    let waiting = pending_bytes(&dir);
    assert!(waiting > 0);
    for _ in 0..10 {
        second.track(AppEvent::Undo);
        assert!(second.flush_blocking(WAIT));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(pending_bytes(&dir), waiting);
    assert!(second_server.envelopes().iter().all(|envelope| envelope.session_id == second.session_id()));
    assert!(first_server.envelopes().iter().all(|envelope| envelope.session_id == first.session_id()));
    first_server.set_offline(false);
    assert!(first.flush_blocking(WAIT));

    let items = all_items(&[&first_server, &second_server]);
    assert_eq!(items.iter().map(|(_, item)| item.id.as_str()).collect::<HashSet<_>>().len(), items.len());
    let count = |server: &Server, name: &str| server.events().iter().filter(|(event, _)| event == name).count();
    assert_eq!((count(&first_server, "file.open"), count(&first_server, "file.save"), count(&first_server, "edit.undo")), (5, 9, 0));
    assert_eq!((count(&second_server, "edit.undo"), count(&second_server, "file.save")), (17, 0));
    // Neither took the other, running, for a session that ended badly.
    assert!(!items.iter().any(|(_, item)| is_unclean_report(item)), "{items:?}");

    // One ends cleanly while the other runs on; the one that ended leaves nothing behind.
    let ended = session_dir(&dir, &first);
    drop(first);
    assert!(!ended.exists());
    assert!(session_dir(&dir, &second).join("session.json").exists());
    assert!(second.flush_blocking(WAIT));
    drop(second);
    let items = all_items(&[&first_server, &second_server]);
    assert!(!items.iter().any(|(_, item)| is_unclean_report(item)));
    assert_eq!(items.iter().filter(|(_, item)| matches!(item.body, ItemBody::SessionEnd(_))).count(), 2);
    assert_eq!(std::fs::read_dir(dir.sdk().join("spool")).unwrap().count(), 0);
}

#[test]
fn consent_switched_off_in_one_tracer_stops_the_other() {
    if reran_without_kill_switches("consent_switched_off_in_one_tracer_stops_the_other") {
        return;
    }
    let dir = TempDir::new();
    let (first_server, second_server) = (Server::default(), Server::offline());
    let first = builder(&dir, &first_server).init();
    let second = builder(&dir, &second_server).init();
    for _ in 0..6 {
        second.track(AppEvent::Undo);
    }
    assert!(!second.flush_blocking(WAIT));
    assert!(pending_bytes(&dir) > 0);

    first.set_consent(ConsentCategory::Usage, false);
    wait_for("the other tracer to see the change", || !second.consent(ConsentCategory::Usage));
    // The other tracer drops what it had not sent, and records nothing more.
    wait_for("the other tracer to delete its unsent items", || pending_bytes(&dir) == 0);
    second.track(AppEvent::Undo);
    second_server.set_offline(false);
    assert!(second.flush_blocking(WAIT));
    assert!(second_server.items().is_empty());
    // A category switched off in the second is kept when the first changes another one.
    second.set_consent(ConsentCategory::Crash, false);
    first.set_consent(ConsentCategory::Usage, true);
    wait_for("both to agree", || second.consent(ConsentCategory::Usage) && !first.consent(ConsentCategory::Crash));
    second.notice_shown();
    wait_for("the notice state to be shared", || !first.notice_pending());
    second.track(AppEvent::FileOpen);
    assert!(second.flush_blocking(WAIT));
    assert_eq!(second_server.events(), vec![("file.open".to_owned(), false)]);
}

#[test]
fn forget_in_one_tracer_is_followed_by_the_other() {
    if reran_without_kill_switches("forget_in_one_tracer_is_followed_by_the_other") {
        return;
    }
    let dir = TempDir::new();
    let (first_server, second_server) = (Server::default(), Server::offline());
    let first = builder(&dir, &first_server).init();
    let second = builder(&dir, &second_server).init();
    let old_id = first.install_id();
    for _ in 0..6 {
        second.track(AppEvent::Undo);
    }
    assert!(!second.flush_blocking(WAIT));

    first.forget();
    let new_id = first.install_id();
    assert_ne!(new_id, old_id);
    wait_for("the other tracer to take the new id", || second.install_id() == new_id);
    wait_for("the other tracer to delete its unsent items", || pending_bytes(&dir) == 0);
    second_server.set_offline(false);
    second.track(AppEvent::FileSave);
    assert!(first.flush_blocking(WAIT) && second.flush_blocking(WAIT));
    // Nothing recorded under the old id leaves the second tracer after the forget.
    assert!(second_server.envelopes().iter().all(|envelope| envelope.install_id == new_id));
    assert_eq!(second_server.events(), vec![("file.save".to_owned(), false)]);
    let forgets: Vec<ForgetRequest> = first_server.forgets().into_iter().chain(second_server.forgets()).collect();
    assert!(!forgets.is_empty() && forgets.iter().all(|forget| forget.install_id == old_id), "{forgets:?}");
    drop((first, second));
    let after = builder(&dir, &first_server).init();
    assert_eq!(after.install_id(), new_id);
    assert!(after.flush_blocking(WAIT));
    assert!(first_server.envelopes().iter().chain(&second_server.envelopes()).filter(|envelope| envelope.install_id == old_id).all(|envelope| {
        // Only what the first tracer had sent before the forget.
        envelope.items.iter().all(|item| matches!(item.body, ItemBody::SessionStart(_)))
    }));
}

/// Not a test of its own: the part a second process plays in the tests below. `TRACER_TEST_ROLE` says
/// which part, and the process works in the data directory the parent test gives it.
#[test]
fn multiprocess_child() {
    let (Some(role), Some(path)) = (std::env::var_os("TRACER_TEST_ROLE"), std::env::var_os("TRACER_TEST_CHILD_DIR")) else { return };
    let dir = TempDir::existing(PathBuf::from(path));
    let pid = std::process::id();
    // Written under another name and renamed, so the parent never reads half a file.
    let report = |name: &str, text: String| {
        let temporary = dir.path().join(format!("{name}.{pid}.part"));
        std::fs::write(&temporary, text).unwrap();
        std::fs::rename(&temporary, dir.path().join(name)).unwrap();
    };
    match role.to_str().unwrap() {
        // Starts, says which install id it got, and ends.
        "starter" => {
            let tracer = builder(&dir, &Server::offline()).init();
            report(&format!("install-{pid}.txt"), tracer.install_id());
        }
        // Records a log line and some events it cannot send, then waits to be killed.
        "victim" => {
            let server = Server::offline();
            let tracer = builder(&dir, &server).signals(&[Signal::Session, Signal::Usage, Signal::Crash, Signal::Logs]).init();
            let logger = tracer.logger(None);
            logger.log(&log::Record::builder().args(format_args!("about to do the risky thing")).level(log::Level::Warn).target("host").build());
            for _ in 0..12 {
                tracer.track(AppEvent::FileSave);
            }
            assert!(!tracer.flush_blocking(WAIT));
            wait_for("the log line to be saved", || session_dir(&dir, &tracer).join("breadcrumbs.jsonl").exists());
            report("victim.txt", format!("{} {}", tracer.session_id(), tracer.install_id()));
            loop {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        // Runs until the parent switches usage consent off, then shows that it stopped recording.
        "listener" => {
            let server = Server::default();
            let tracer = builder(&dir, &server).init();
            tracer.track(AppEvent::FileOpen);
            tracer.track(AppEvent::FileOpen);
            assert!(tracer.flush_blocking(WAIT));
            report("listener-ready.txt", tracer.session_id());
            wait_for("consent to be switched off by the parent", || !tracer.consent(ConsentCategory::Usage));
            for _ in 0..5 {
                tracer.track(AppEvent::FileSave);
            }
            assert!(tracer.flush_blocking(WAIT));
            let names: Vec<String> = server.events().into_iter().map(|(name, _)| name).collect();
            drop(tracer);
            report("listener-done.txt", names.join(","));
        }
        // A command-line process: ends its session, offline, and exits without dropping anything.
        "exiter" => {
            let tracer = builder(&dir, &Server::offline())
                .process(ashlar_trace::ProcessKind::Cli)
                .signals(&[Signal::Session, Signal::Usage, Signal::Crash])
                .init();
            tracer.track(AppEvent::FileOpen);
            assert!(!tracer.shutdown_within(Duration::from_millis(50)));
            report("exiter.txt", tracer.session_id());
            std::process::exit(3);
        }
        // Records no session and no crashes, only usage it cannot send, then waits to be killed.
        "quiet" => {
            let tracer = builder(&dir, &Server::offline()).signals(&[Signal::Usage]).init();
            tracer.track(AppEvent::FileSave);
            assert!(!tracer.flush_blocking(WAIT));
            report("quiet.txt", tracer.session_id());
            loop {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        other => panic!("unknown role {other}"),
    }
}

fn spawn_child(role: &str, dir: &TempDir) -> std::process::Child {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", "multiprocess_child", "--test-threads=1"]);
    child.env_remove("DO_NOT_TRACK").env_remove("TRACER_DISABLED").env("TRACER_TEST_ROLE", role).env("TRACER_TEST_CHILD_DIR", dir.path());
    child.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    child.spawn().unwrap()
}

/// Waits for a file a child reports with, and returns its text.
fn read_report(dir: &TempDir, name: &str, child: &mut std::process::Child) -> String {
    let path = dir.path().join(name);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {name}");
        if let Some(status) = child.try_wait().unwrap() {
            assert!(path.exists(), "the child ended with {status} without writing {name}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn processes_starting_together_on_a_fresh_directory_agree_on_the_install_id() {
    if reran_without_kill_switches("processes_starting_together_on_a_fresh_directory_agree_on_the_install_id") {
        return;
    }
    let dir = TempDir::new();
    let mut children: Vec<std::process::Child> = (0..6).map(|_| spawn_child("starter", &dir)).collect();
    let server = Server::default();
    let own = builder(&dir, &server).init().install_id();
    let mut ids = vec![own];
    for child in &mut children {
        let name = format!("install-{}.txt", child.id());
        ids.push(read_report(&dir, &name, child));
        assert!(child.wait().unwrap().success());
    }
    assert_eq!(ids[0].len(), 32);
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 1, "{ids:?}");
}

#[test]
fn a_killed_process_is_reported_once_with_its_log_lines_by_whichever_process_sees_it_first() {
    if reran_without_kill_switches("a_killed_process_is_reported_once_with_its_log_lines_by_whichever_process_sees_it_first") {
        return;
    }
    let dir = TempDir::new();
    let (first_server, second_server) = (Server::default(), Server::default());
    let signals = [Signal::Session, Signal::Usage, Signal::Crash];
    let first = builder(&dir, &first_server).signals(&signals).init();
    let second = builder(&dir, &second_server).signals(&signals).init();
    let servers = [&first_server, &second_server];

    let mut victim = spawn_child("victim", &dir);
    let report = read_report(&dir, "victim.txt", &mut victim);
    let (victim_session, victim_install) = report.split_once(' ').unwrap();
    assert_eq!(victim_install, first.install_id());
    let victim_dir = dir.sdk().join("spool").join(victim_session);
    let of_victim = |items: Vec<(Envelope, Item)>| {
        items.into_iter().filter(|(envelope, _)| envelope.session_id == victim_session).map(|(_, item)| item).collect::<Vec<Item>>()
    };

    // While it runs, the two tracers here pass over it many times and leave it alone: nothing of it is
    // sent, nothing is deleted, and it is not taken for a session that ended badly.
    let waiting = disk_bytes(&victim_dir);
    for _ in 0..15 {
        first.track(AppEvent::FileOpen);
        assert!(first.flush_blocking(WAIT) && second.flush_blocking(WAIT));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(victim.try_wait().unwrap().is_none(), "the victim is still running");
    assert!(of_victim(all_items(&servers)).is_empty());
    assert!(!all_items(&servers).iter().any(|(_, item)| is_unclean_report(item)));
    assert!(disk_bytes(&victim_dir) >= waiting && victim_dir.join("session.json").exists() && victim_dir.join("breadcrumbs.jsonl").exists());

    victim.kill().unwrap();
    victim.wait().unwrap();
    wait_for("the unclean exit to be reported", || of_victim(all_items(&servers)).iter().any(is_unclean_report));
    // Both go on looking; nothing is reported a second time.
    for _ in 0..10 {
        assert!(first.flush_blocking(WAIT) && second.flush_blocking(WAIT));
        std::thread::sleep(Duration::from_millis(10));
    }
    let items = of_victim(all_items(&servers));
    assert_eq!(items.iter().map(|item| item.id.as_str()).collect::<HashSet<_>>().len(), items.len(), "{items:?}");
    let crashes: Vec<_> = items.iter().filter_map(|item| if let ItemBody::Crash(crash) = &item.body { Some(crash) } else { None }).collect();
    assert_eq!(crashes.len(), 1);
    assert_eq!((crashes[0].kind, crashes[0].fatal), (CrashKind::UncleanExit, true));
    assert!(
        crashes[0].breadcrumbs.iter().any(|crumb| crumb.message.contains("about to do the risky thing") && crumb.level == "warn"),
        "{:?}",
        crashes[0].breadcrumbs
    );
    let ends: Vec<_> = items.iter().filter_map(|item| if let ItemBody::SessionEnd(end) = &item.body { Some(end.status) } else { None }).collect();
    assert_eq!(ends, vec![SessionStatus::Abnormal]);
    // What it had recorded and could not send arrives too, once, under its own session.
    assert_eq!(items.iter().filter(|item| matches!(&item.body, ItemBody::Event(event) if event.name == "file.save")).count(), 12);
    assert_eq!(items.iter().filter(|item| matches!(item.body, ItemBody::SessionStart(_))).count(), 1);
    // One of the two did all of it, and the folder is gone.
    let reporters = servers.iter().filter(|server| !of_victim(server.items()).is_empty()).count();
    assert_eq!(reporters, 1);
    assert!(!victim_dir.exists());
    assert!(!all_items(&servers).iter().any(|(envelope, item)| envelope.session_id != victim_session && is_unclean_report(item)));
}

#[test]
fn consent_switched_off_here_stops_another_process() {
    if reran_without_kill_switches("consent_switched_off_here_stops_another_process") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = builder(&dir, &server).init();
    let mut listener = spawn_child("listener", &dir);
    let listener_session = read_report(&dir, "listener-ready.txt", &mut listener);
    assert_ne!(listener_session, tracer.session_id());
    tracer.set_consent(ConsentCategory::Usage, false);
    let recorded = read_report(&dir, "listener-done.txt", &mut listener);
    assert!(listener.wait().unwrap().success());
    // Only the two events from before the change; the five after it were never recorded.
    assert_eq!(recorded, "file.open,file.open");
    // And this process never touched the other's session.
    assert!(server.envelopes().iter().all(|envelope| envelope.session_id == tracer.session_id()));
}

// ---- review fixes ----

#[test]
fn retry_after_zero_does_not_mean_retry_on_every_tick() {
    if reran_without_kill_switches("retry_after_zero_does_not_mean_retry_on_every_tick") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    for _ in 0..3 {
        server.script(429, Some(Duration::ZERO));
    }
    // A long shortest-wait, so that the floor is unmistakable next to the 5 ms tick.
    let tracer = builder(&dir, &server).configure(|config| config.retry_min = Duration::from_millis(150)).init();
    tracer.track(AppEvent::FileOpen);
    wait_for("delivery after the rate limit", || server.events().len() == 1);
    let seen = server.seen();
    assert_eq!(seen.iter().map(|seen| seen.status).collect::<Vec<_>>()[..4], [429, 429, 429, 200]);
    for pair in seen[..4].windows(2) {
        let gap = pair[1].at.duration_since(pair[0].at);
        assert!(gap >= Duration::from_millis(150), "asked again after only {gap:?}");
    }
}

#[test]
fn a_reply_that_is_not_an_ingest_reply_does_not_acknowledge_the_catalog() {
    if reran_without_kill_switches("a_reply_that_is_not_an_ingest_reply_does_not_acknowledge_the_catalog") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    server.set_foreign_replies(true);
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    // The 200 was taken as delivery, but it said nothing about the catalog: the next run sends it again.
    server.set_foreign_replies(false);
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    assert!(session_starts(&server).pop().unwrap().catalog.is_some());
    // A real reply acknowledges it.
    let tracer = builder(&dir, &server).init();
    assert!(tracer.flush_blocking(WAIT));
    assert!(session_starts(&server).pop().unwrap().catalog.is_none());
}

// ---- the first-run notice ----

/// A builder as a host would write it: nothing about the notice, so sending waits for it.
fn first_run_builder(dir: &TempDir, server: &Server) -> Builder {
    Tracer::builder(KEY, "host", "1.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(server.clone())
        .signals(&[Signal::Session, Signal::Usage])
        .listen_all::<AppEvent>()
        .configure(|config| {
            config.tick_interval = Duration::from_millis(5);
            config.flush_interval = Duration::from_millis(20);
        })
}

#[test]
fn nothing_leaves_the_machine_before_the_notice_has_been_shown() {
    if reran_without_kill_switches("nothing_leaves_the_machine_before_the_notice_has_been_shown") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    // First run: the host has not shown its notice yet.
    let tracer = first_run_builder(&dir, &server).init();
    assert!(tracer.notice_pending());
    for _ in 0..5 {
        tracer.track(AppEvent::FileOpen);
    }
    assert!(!tracer.flush_blocking(WAIT), "data is waiting, so the flush is not clean");
    std::thread::sleep(Duration::from_millis(150));
    assert!(server.seen().is_empty(), "something was sent before the notice");
    assert!(pending_bytes(&dir) > 0, "what was recorded is kept");
    // The process ends without the notice ever being shown; still nothing is sent, and nothing is lost.
    drop(tracer);
    assert!(server.seen().is_empty());

    // Second run: the same until the host says the user has seen the notice. Then everything goes.
    let tracer = first_run_builder(&dir, &server).init();
    tracer.track(AppEvent::FileSave);
    assert!(!tracer.flush_blocking(WAIT));
    assert!(server.seen().is_empty());
    tracer.notice_shown();
    assert!(tracer.flush_blocking(WAIT));
    let mut names: Vec<String> = server.events().into_iter().map(|(name, _)| name).collect();
    names.sort();
    assert_eq!(names, ["file.open", "file.open", "file.open", "file.open", "file.open", "file.save"]);
    assert_eq!(session_starts(&server).len(), 2);
    drop(tracer);

    // Later runs send from the start: the notice was shown once, and that is remembered.
    let before = server.seen().len();
    let tracer = first_run_builder(&dir, &server).init();
    assert!(!tracer.notice_pending());
    assert!(tracer.flush_blocking(WAIT));
    assert!(server.seen().len() > before);
}

#[test]
fn an_opt_out_made_before_the_notice_is_never_sent() {
    if reran_without_kill_switches("an_opt_out_made_before_the_notice_is_never_sent") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = first_run_builder(&dir, &server).init();
    for _ in 0..5 {
        tracer.track(AppEvent::FileOpen);
    }
    assert!(!tracer.flush_blocking(WAIT));
    // The user reads the notice and switches usage data off before closing it.
    tracer.set_consent(ConsentCategory::Usage, false);
    tracer.notice_shown();
    assert!(tracer.flush_blocking(WAIT));
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    assert!(server.items().is_empty(), "{:?}", server.items());
    assert_eq!(pending_bytes(&dir), 0);
}

#[test]
fn send_before_notice_lets_a_host_that_shows_its_notice_elsewhere_send_at_once() {
    if reran_without_kill_switches("send_before_notice_lets_a_host_that_shows_its_notice_elsewhere_send_at_once") {
        return;
    }
    let dir = TempDir::new();
    let server = Server::default();
    let tracer = first_run_builder(&dir, &server).send_before_notice(true).init();
    tracer.track(AppEvent::FileOpen);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events().len(), 1);
    assert!(tracer.notice_pending(), "the option does not pretend the notice was shown");
}

/// Collects the SDK's own log lines, for the child process below.
struct OwnLines(Mutex<Vec<String>>);

impl log::Log for OwnLines {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        if record.target() == "ashlar_trace" {
            self.0.lock().unwrap().push(format!("{} {}", record.level(), record.args()));
        }
    }

    fn flush(&self) {}
}

/// Not a test of its own: run in a child process by the test below, because it installs the process's
/// one global logger.
#[test]
fn held_sending_child() {
    let Some(path) = std::env::var_os("TRACER_TEST_HELD_DIR") else { return };
    static LINES: OwnLines = OwnLines(Mutex::new(Vec::new()));
    log::set_logger(&LINES).unwrap();
    log::set_max_level(log::LevelFilter::Trace);
    let dir = TempDir::existing(PathBuf::from(path));
    let server = Server::default();
    // Its own logger wraps the global one, as a host with `Signal::Logs` would have it.
    let tracer = first_run_builder(&dir, &server).signals(&[Signal::Session, Signal::Usage, Signal::Logs, Signal::Crash]).init();
    for _ in 0..4 {
        tracer.track(AppEvent::FileOpen);
        assert!(!tracer.flush_blocking(WAIT));
        std::thread::sleep(Duration::from_millis(60));
    }
    tracer.notice_shown();
    assert!(tracer.flush_blocking(WAIT));
    drop(tracer);
    let lines = LINES.0.lock().unwrap().clone();
    let sent = String::from_utf8_lossy(&server.seen().iter().flat_map(|seen| seen.body.clone()).collect::<Vec<u8>>()).into_owned();
    std::fs::write(dir.path().join("held.txt"), format!("lines={}\nleaked={}\n{}", lines.len(), sent.contains("notice_shown"), lines.join("\n")))
        .unwrap();
}

#[test]
fn a_host_that_forgot_the_notice_is_told_once_why_nothing_arrives() {
    if reran_without_kill_switches("a_host_that_forgot_the_notice_is_told_once_why_nothing_arrives") {
        return;
    }
    let dir = TempDir::new();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", "held_sending_child", "--test-threads=1"]).env("TRACER_TEST_HELD_DIR", dir.path());
    let output = child.output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
    let report = std::fs::read_to_string(dir.path().join("held.txt")).unwrap();
    // One warning for four held flushes and several held intervals, and it is not sent anywhere.
    assert!(report.starts_with("lines=1\nleaked=false\nWARN "), "{report}");
    assert!(report.contains("notice_shown"), "{report}");
}

// ---- listening without declaring ----

#[test]
fn listening_declares_the_events_itself_in_any_order() {
    if reran_without_kill_switches("listening_declares_the_events_itself_in_any_order") {
        return;
    }
    let base = |dir: &TempDir, server: &Server| {
        Tracer::builder(KEY, "host", "1.0.0")
            .data_dir(dir.path())
            .endpoint("https://telemetry.example.com")
            .transport(server.clone())
            .send_before_notice(true)
            .signals(&[Signal::Session, Signal::Usage])
            .configure(|config| config.tick_interval = Duration::from_millis(5))
    };
    let catalog_names = |server: &Server| -> Vec<String> {
        let start = session_starts(server).pop().unwrap();
        start.catalog.unwrap().events.into_iter().map(|event| event.name).collect()
    };
    let all = ["edit.undo", "file.open", "file.save", "view.zoom"];

    // A category alone, with no `catalog` call: its events are recorded, and the whole catalog is sent.
    let (dir, server) = (TempDir::new(), Server::default());
    let tracer = base(&dir, &server).listen(Category::File).init();
    tracer.track(AppEvent::FileOpen);
    tracer.track(AppEvent::Undo);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events(), vec![("file.open".to_owned(), false)]);
    assert_eq!(catalog_names(&server), all);

    // Single events alone.
    let (dir, server) = (TempDir::new(), Server::default());
    let tracer = base(&dir, &server).listen_events(&[AppEvent::Undo]).init();
    tracer.track(AppEvent::FileOpen);
    tracer.track(AppEvent::Undo);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events(), vec![("edit.undo".to_owned(), false)]);
    assert_eq!(catalog_names(&server), all);

    // Listening first and declaring afterwards, or several times over, declares each event once.
    let (dir, server) = (TempDir::new(), Server::default());
    let tracer = base(&dir, &server).listen_all::<AppEvent>().catalog::<AppEvent>().listen(Category::View).init();
    tracer.track(AppEvent::Zoom);
    assert!(tracer.flush_blocking(WAIT));
    assert_eq!(server.events(), vec![("view.zoom".to_owned(), false)]);
    assert_eq!(catalog_names(&server), all);
}
