//! Helpers shared by the stress and edge-case tests.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tracer_sdk::transport::{Reply, Request, Transport, TransportError};

tracer_sdk::events! {
    pub enum Category { File = "file", Edit = "edit", View = "view" }
    pub enum AppEvent {
        FileOpen = "file.open" in File,
        FileSave = "file.save" in File,
        Undo = "edit.undo" in Edit,
        Zoom = "view.zoom" in View,
    }
}

/// A folder under the build's temporary directory, removed on drop.
pub struct TempDir(PathBuf, bool);

impl TempDir {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!("stress-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path, true)
    }

    /// A folder another process created and will remove.
    pub fn existing(path: PathBuf) -> Self {
        Self(path, false)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The SDK's own folder.
    pub fn sdk(&self) -> PathBuf {
        self.0.join("tracer")
    }

    pub fn spool(&self) -> PathBuf {
        self.sdk().join("spool")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// A transport that accepts everything at once.
pub struct Instant200;

impl Transport for Instant200 {
    fn send(&mut self, _request: &Request<'_>) -> Result<Reply, TransportError> {
        Ok(Reply { status: 200, body: br#"{"accepted":1}"#.to_vec(), retry_after: None })
    }
}

/// A small seeded generator (SplitMix64), so that a failing run can be repeated from its seed.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    /// A number below `bound`.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

/// Total size of the files under a folder.
pub fn disk_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(metadata) = entry.metadata() else { continue };
        total += if metadata.is_dir() { disk_bytes(&entry.path()) } else { metadata.len() };
    }
    total
}

/// A transport that takes `delay` per request, then accepts it or fails as a network error.
pub struct Slow {
    pub delay: std::time::Duration,
    pub fail: bool,
}

impl Transport for Slow {
    fn send(&mut self, _request: &Request<'_>) -> Result<Reply, TransportError> {
        std::thread::sleep(self.delay);
        if self.fail {
            return Err(TransportError::Network("unreachable".to_owned()));
        }
        Ok(Reply { status: 200, body: br#"{"accepted":1}"#.to_vec(), retry_after: None })
    }
}

// ---- flood ----

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tracer_sdk::proto::{Envelope, ItemBody, routes};
use tracer_sdk::{ConsentCategory, Prop, Signal, Tracer};

/// One request a [`Chaos`] transport was given, and what it answered (0 for a network error).
pub struct Seen {
    pub path: String,
    pub status: u16,
    pub envelope: Option<Envelope>,
}

/// A transport that answers at random, from a seed: accepted, accepted with a body that is not a reply,
/// rate limited, server error, timeout, refused for good, and now and then an unknown key. Once
/// `healthy` is set it accepts everything.
#[derive(Clone)]
pub struct Chaos {
    pub seen: Arc<Mutex<Vec<Seen>>>,
    pub healthy: Arc<AtomicBool>,
    rng: Arc<Mutex<Rng>>,
}

impl Chaos {
    pub fn new(seed: u64) -> Self {
        Self { seen: Arc::default(), healthy: Arc::default(), rng: Arc::new(Mutex::new(Rng(seed))) }
    }
}

impl Transport for Chaos {
    fn send(&mut self, request: &Request<'_>) -> Result<Reply, TransportError> {
        let mut body = Vec::new();
        if request.gzip {
            flate2::read::GzDecoder::new(request.body).read_to_end(&mut body).unwrap();
        } else {
            body.extend_from_slice(request.body);
        }
        let envelope =
            (request.path == routes::INGEST).then(|| serde_json::from_slice::<Envelope>(&body).expect("the SDK sent a body that is not an envelope"));
        let roll = if self.healthy.load(Ordering::Relaxed) { 0 } else { self.rng.lock().unwrap().below(100) };
        let (status, reply): (u16, &[u8]) = match roll {
            0..55 => (200, br#"{"accepted":1}"#),
            55..65 => (200, b"<html>not a reply</html>"),
            65..75 => (429, b"{}"),
            75..85 => (500, b"{}"),
            85..92 => (0, b""),
            92..98 => (400, b"{}"),
            _ => (401, b"{}"),
        };
        self.seen.lock().unwrap().push(Seen { path: request.path.to_owned(), status, envelope });
        match status {
            0 => {
                std::thread::sleep(Duration::from_millis(1));
                Err(TransportError::Network("timed out".to_owned()))
            }
            429 => Ok(Reply { status, body: reply.to_vec(), retry_after: Some(Duration::from_millis(roll % 3)) }),
            _ => Ok(Reply { status, body: reply.to_vec(), retry_after: None }),
        }
    }
}

/// Ends the whole process if `work` has not finished in time: a deadlock must fail the run, not hang it.
pub fn with_deadline<T: Send>(what: &str, limit: Duration, work: impl FnOnce() -> T + Send) -> T {
    let done = Arc::new(AtomicBool::new(false));
    let (flag, what) = (done.clone(), what.to_owned());
    std::thread::spawn(move || {
        let started = Instant::now();
        while !flag.load(Ordering::Relaxed) {
            if started.elapsed() > limit {
                eprintln!("DEADLOCK OR STALL: {what} did not finish within {limit:?}; aborting");
                std::process::abort();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let result = work();
    done.store(true, Ordering::Relaxed);
    result
}

fn flood_tracer(dir: &TempDir, chaos: &Chaos, signals: &[Signal]) -> Tracer {
    Tracer::builder("key", "host", "1.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(chaos.clone())
        .send_before_notice(true)
        .listen(Category::File)
        .listen_events(&[AppEvent::Undo])
        .signals(signals)
        .configure(|config| {
            config.tick_interval = Duration::from_millis(2);
            config.flush_interval = Duration::from_millis(5);
            config.retry_min = Duration::from_millis(1);
            config.retry_max = Duration::from_millis(8);
            config.perf_window = Duration::from_millis(20);
            config.heartbeat_interval = Duration::from_millis(20);
            config.hang_threshold = Duration::from_millis(50);
            config.spool_max_bytes = 192 * 1024;
            config.shutdown_timeout = Duration::from_secs(20);
        })
        .init()
}

/// What one thread of the flood did that the accounting needs.
#[derive(Default)]
struct Did {
    /// Calls that passed the listening check with a valid event: each must end up somewhere.
    events: u64,
    /// Calls that passed the listening check with an id the worker must refuse.
    invalid: u64,
}

fn hammer(tracer: &Tracer, rng: &mut Rng, calls: u64) -> Did {
    const LONG: &str = "this-value-is-longer-than-sixty-four-bytes-and-so-it-must-be-left-out";
    let mut did = Did::default();
    let counter = tracer.counter("flood_counter");
    let gauge = tracer.gauge("flood_gauge");
    let logger = tracer.logger(None);
    let failure = std::io::Error::other("flood failure");
    for call in 0..calls {
        // A short pause now and then, so that the run lasts long enough for the worker to send, fail,
        // back off and send again many times while the calls keep coming.
        if call % 16 == 15 {
            std::thread::sleep(Duration::from_micros(300));
        }
        match rng.below(20) {
            0..4 => {
                tracer.track(AppEvent::FileOpen);
                did.events += 1;
            }
            4 => {
                tracer.track(AppEvent::Undo);
                did.events += 1;
            }
            5 => {
                tracer.track_with(
                    AppEvent::FileSave,
                    &[("count", Prop::Int(call as i64)), ("too_long", Prop::Str(LONG)), ("Bad Key", Prop::Bool(true)), ("ratio", Prop::Float(0.5))],
                );
                did.events += 1;
            }
            6 => {
                tracer.track_id(Category::File, "file.export_pdf");
                did.events += 1;
            }
            7 => {
                tracer.track_id(Category::File, "Not A Valid Id");
                did.invalid += 1;
            }
            8 => {
                // Not listened to: none of these may be recorded or counted.
                tracer.track(AppEvent::Zoom);
                tracer.track_id(Category::View, "view.reset");
                tracer.track_id(Category::Edit, "edit.redo");
            }
            9 => tracer.frame(Duration::from_micros(rng.below(40_000))),
            10 => drop(tracer.turn()),
            11 => {
                tracer.begin_turn();
                tracer.heartbeat();
                tracer.end_turn();
            }
            12 => {
                tracer.note_input();
                tracer.set_focused(call % 2 == 0);
            }
            13 => {
                counter.add(3);
                gauge.set(call as f64);
            }
            14 => logger.log(
                &log::Record::builder().level(log::Level::Warn).target("flood").args(format_args!("call {call} in /Users/someone/file")).build(),
            ),
            15 => tracer.report_handled(&failure, Some("flood")),
            16 => {
                tracer.first_frame();
                tracer.pause_hang_detection();
                tracer.resume_hang_detection();
            }
            17 => {
                let _ = (tracer.consent(ConsentCategory::Usage), tracer.notice_pending(), tracer.is_enabled());
            }
            18 => {
                assert_eq!(tracer.install_id().len(), 32);
                assert_eq!(tracer.session_id().len(), 32);
            }
            _ => drop(tracer.clone()),
        }
    }
    did
}

/// Everything the transport saw, added up.
#[derive(Default, Debug)]
pub struct Tally {
    pub delivered_events: u64,
    pub refused_events: u64,
    pub refused_items: u64,
    pub drops: BTreeMap<String, u64>,
    pub requests: usize,
    /// How many requests got each status (0 is a network error).
    pub statuses: BTreeMap<u16, usize>,
}

fn tally(chaos: &Chaos) -> Tally {
    let mut tally = Tally::default();
    let mut ids = HashSet::new();
    let seen = chaos.seen.lock().unwrap();
    tally.requests = seen.len();
    for request in seen.iter() {
        *tally.statuses.entry(request.status).or_insert(0) += 1;
        let Some(envelope) = &request.envelope else { continue };
        let stored = (200..300).contains(&request.status);
        let refused = request.status == 400;
        if !stored && !refused {
            continue;
        }
        for item in &envelope.items {
            assert!(ids.insert(item.id.clone()), "item {} was delivered or refused twice", item.id);
            match &item.body {
                ItemBody::Event(event) => {
                    assert!(["file.open", "file.save", "edit.undo", "file.export_pdf"].contains(&event.name.as_str()), "{event:?}");
                    if stored {
                        tally.delivered_events += 1;
                    } else {
                        tally.refused_events += 1;
                    }
                }
                // The counts in a refused batch are lost to the server, but this test can still read them.
                ItemBody::Drops(drops) => {
                    for (reason, count) in &drops.reasons {
                        *tally.drops.entry(reason.clone()).or_insert(0) += count;
                    }
                }
                _ => {}
            }
            tally.refused_items += u64::from(refused);
        }
    }
    tally
}

/// Many threads calling everything at once against a transport that misbehaves, then a healthy run that
/// takes what is left. With only the usage signal on, every accepted `track` call must be accounted for
/// exactly: delivered, refused by the server, or counted under a drop reason.
pub fn flood_with_exact_accounting(seed: u64, threads: u64, calls: u64) -> Tally {
    with_deadline(&format!("flood seed {seed}"), Duration::from_secs(180), || {
        let dir = TempDir::new();
        let chaos = Chaos::new(seed);
        let tracer = flood_tracer(&dir, &chaos, &[Signal::Usage]);
        let stop = AtomicBool::new(false);
        let (mut events, mut invalid) = (0, 0);
        std::thread::scope(|scope| {
            let flusher = scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    tracer.flush_blocking(Duration::from_millis(3));
                }
            });
            let workers: Vec<_> = (0..threads)
                .map(|index| {
                    let tracer = tracer.clone();
                    scope.spawn(move || hammer(&tracer, &mut Rng(seed.wrapping_mul(1_000_003) ^ index), calls))
                })
                .collect();
            for worker in workers {
                let did = worker.join().unwrap();
                events += did.events;
                invalid += did.invalid;
            }
            stop.store(true, Ordering::Relaxed);
            flusher.join().unwrap();
        });
        drop(tracer);

        // A new run with a server that works takes over whatever is still on disk, including after a 401.
        chaos.healthy.store(true, Ordering::Relaxed);
        let tracer = flood_tracer(&dir, &chaos, &[Signal::Usage]);
        assert!(tracer.flush_blocking(Duration::from_secs(60)), "seed {seed}: the healthy run could not drain the spool");
        drop(tracer);
        assert_eq!(std::fs::read_dir(dir.spool()).unwrap().count(), 0, "seed {seed}: something was left in the spool");

        let tally = tally(&chaos);
        let dropped = |reason: &str| tally.drops.get(reason).copied().unwrap_or(0);
        let accounted = tally.delivered_events + tally.refused_events + dropped("queue_full") + dropped("spool_evicted") + dropped("invalid_id");
        assert_eq!(events + invalid, accounted, "seed {seed}: {events} events and {invalid} invalid ids went in; {tally:?}");
        assert!(dropped("invalid_id") <= invalid, "seed {seed}: {tally:?}");
        assert_eq!(dropped("server_rejected"), tally.refused_items, "seed {seed}: {tally:?}");
        for never in ["spool_damaged", "io_error", "spool_busy", "item_too_large", "tick_panicked"] {
            assert_eq!(dropped(never), 0, "seed {seed}: unexpected {never}: {tally:?}");
        }
        tally
    })
}

/// The same flood with every signal on, while another thread switches consent, asks to be forgotten and
/// flushes. Deleting data on request is not a counted drop, so there is no exact sum here; what must hold
/// is that nothing panics, nothing deadlocks, nothing is delivered twice, and it ends cleanly.
pub fn flood_with_consent_and_forget(seed: u64, threads: u64, calls: u64) -> Tally {
    with_deadline(&format!("chaos flood seed {seed}"), Duration::from_secs(180), || {
        let dir = TempDir::new();
        let chaos = Chaos::new(seed);
        let tracer = flood_tracer(&dir, &chaos, &Signal::ALL);
        let stop = AtomicBool::new(false);
        let mut events = 0;
        std::thread::scope(|scope| {
            let meddler = scope.spawn(|| {
                let mut rng = Rng(seed ^ 0xfeed);
                let mut forgets = HashSet::new();
                while !stop.load(Ordering::Relaxed) {
                    let category = [ConsentCategory::Usage, ConsentCategory::Crash, ConsentCategory::Performance][rng.below(3) as usize];
                    match rng.below(8) {
                        0..3 => tracer.set_consent(category, rng.below(2) == 0),
                        3 => {
                            tracer.forget();
                            assert!(forgets.insert(tracer.install_id()), "forget gave an install id it had given before");
                        }
                        4 => tracer.notice_shown(),
                        5 => {
                            let _ = tracer.write_report(&dir.path().join("report.txt"));
                        }
                        _ => {
                            tracer.flush_blocking(Duration::from_millis(3));
                        }
                    }
                    std::thread::sleep(Duration::from_micros(200));
                }
                for category in [ConsentCategory::Usage, ConsentCategory::Crash, ConsentCategory::Performance] {
                    tracer.set_consent(category, true);
                }
            });
            let workers: Vec<_> = (0..threads)
                .map(|index| {
                    let tracer = tracer.clone();
                    scope.spawn(move || hammer(&tracer, &mut Rng(seed.wrapping_mul(7_919) ^ index), calls))
                })
                .collect();
            for worker in workers {
                events += worker.join().unwrap().events;
            }
            stop.store(true, Ordering::Relaxed);
            meddler.join().unwrap();
        });
        let install_id = tracer.install_id();
        drop(tracer);
        chaos.healthy.store(true, Ordering::Relaxed);
        let tracer = flood_tracer(&dir, &chaos, &Signal::ALL);
        assert_eq!(tracer.install_id(), install_id, "seed {seed}: the install id changed without a forget");
        assert!(tracer.flush_blocking(Duration::from_secs(60)), "seed {seed}: the healthy run could not drain the spool");
        drop(tracer);
        let tally = tally(&chaos);
        assert!(tally.delivered_events + tally.refused_events <= events, "seed {seed}: more came out than went in: {tally:?}");
        tally
    })
}

// ---- many processes on one data directory ----

/// A transport that writes every ingest request to a file of its own and accepts it. Processes that share
/// a data directory share the folder these go to, so a test can read afterwards what all of them sent.
pub struct FileSink {
    pub out: PathBuf,
    sent: u64,
}

impl FileSink {
    pub fn new(out: &Path) -> Self {
        std::fs::create_dir_all(out).unwrap();
        Self { out: out.to_owned(), sent: 0 }
    }
}

impl Transport for FileSink {
    fn send(&mut self, request: &Request<'_>) -> Result<Reply, TransportError> {
        if request.path == routes::INGEST {
            let mut body = Vec::new();
            flate2::read::GzDecoder::new(request.body).read_to_end(&mut body).unwrap();
            let name = format!("sent-{}-{}", std::process::id(), self.sent);
            self.sent += 1;
            // Written under another name first: a reader, or a kill, never sees half a file.
            std::fs::write(self.out.join(format!("{name}.part")), &body).unwrap();
            std::fs::rename(self.out.join(format!("{name}.part")), self.out.join(format!("{name}.json"))).unwrap();
        }
        Ok(Reply { status: 200, body: br#"{"accepted":1}"#.to_vec(), retry_after: None })
    }
}

fn shared_tracer(dir: &TempDir) -> Tracer {
    Tracer::builder("key", "host", "1.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(FileSink::new(&dir.path().join("sent")))
        .send_before_notice(true)
        .listen_all::<AppEvent>()
        .signals(&[Signal::Session, Signal::Usage, Signal::Crash, Signal::Logs])
        .configure(|config| {
            config.tick_interval = Duration::from_millis(5);
            config.flush_interval = Duration::from_millis(10);
            config.shutdown_timeout = Duration::from_secs(20);
        })
        .init()
}

/// The part a child process plays in [`many_processes`]. Returns false when this process is not one.
pub fn child_process() -> bool {
    let (Some(role), Some(path), Some(seed)) =
        (std::env::var_os("TRACER_CHILD_ROLE"), std::env::var_os("TRACER_CHILD_DIR"), std::env::var_os("TRACER_CHILD_SEED"))
    else {
        return false;
    };
    let dir = TempDir::existing(PathBuf::from(path));
    let mut rng = Rng(seed.to_str().unwrap().parse().unwrap());
    let role = role.to_str().unwrap().to_owned();
    let events = 20 + rng.below(200);
    let tracer = shared_tracer(&dir);
    let session = tracer.session_id();
    let note = |name: &str, text: String| {
        std::fs::write(dir.path().join(format!("{name}-{session}.part")), text).unwrap();
        std::fs::rename(dir.path().join(format!("{name}-{session}.part")), dir.path().join(format!("{name}-{session}.txt"))).unwrap();
    };
    note("start", format!("{role} {events} {}", tracer.install_id()));
    let logger = tracer.logger(None);
    logger.log(&log::Record::builder().level(log::Level::Warn).target("child").args(format_args!("child {session} at work")).build());
    for _ in 0..events {
        tracer.track(AppEvent::FileOpen);
        if rng.below(4) == 0 {
            std::thread::sleep(Duration::from_micros(rng.below(1_500)));
        }
    }
    if role == "clean" {
        assert!(tracer.flush_blocking(Duration::from_secs(30)));
        drop(tracer);
        return true;
    }
    // The others never end by themselves. A `victim` first makes sure all it recorded is on disk and says
    // so, which lets the parent hold it to an exact count; a `random` one is killed whenever.
    tracer.flush_blocking(Duration::from_secs(30));
    let breadcrumbs = dir.spool().join(&session).join("breadcrumbs.jsonl");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !breadcrumbs.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    note("ready", String::new());
    loop {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// What all the processes sent, as the server would hold it: one copy of each item.
fn sent_items(dir: &TempDir) -> Vec<(Envelope, tracer_sdk::proto::Item)> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for entry in std::fs::read_dir(dir.path().join("sent")).unwrap().flatten() {
        if entry.path().extension().is_some_and(|extension| extension == "json") {
            let envelope: Envelope = serde_json::from_slice(&std::fs::read(entry.path()).unwrap()).unwrap();
            let header = Envelope { items: Vec::new(), ..envelope.clone() };
            for item in envelope.items {
                if seen.insert(item.id.clone()) {
                    items.push((header.clone(), item));
                }
            }
        }
    }
    items
}

/// Starts `count` real processes on one data directory at random moments. Some end cleanly, some are
/// killed once everything they recorded is on disk, some are killed at a random moment. Afterwards every
/// session must have been reported exactly once, with nothing lost and nothing invented. `test` is the
/// name of the test in the calling file that runs [`child_process`].
pub fn many_processes(seed: u64, count: usize, test: &str) {
    with_deadline(&format!("many processes seed {seed}"), Duration::from_secs(180), || {
        let dir = TempDir::new();
        let mut rng = Rng(seed);
        let spawn = |role: &str, child_seed: u64| {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", test, "--ignored", "--test-threads=1"]);
            child.env_remove("DO_NOT_TRACK").env_remove("TRACER_DISABLED");
            child.env("TRACER_CHILD_ROLE", role).env("TRACER_CHILD_DIR", dir.path()).env("TRACER_CHILD_SEED", child_seed.to_string());
            child.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            child.spawn().unwrap()
        };
        let mut children = Vec::new();
        for index in 0..count {
            let role = match index % 10 {
                0..6 => "clean",
                6..9 => "victim",
                _ => "random",
            };
            children.push((role, spawn(role, rng.next()), Instant::now() + Duration::from_millis(rng.below(400))));
            std::thread::sleep(Duration::from_millis(rng.below(30)));
        }
        // Wait for each child in the way its role needs. A victim is killed only after it has said that all
        // it recorded is on disk; which file is its own is found from the start notes below.
        let ready = |session: &str| dir.path().join(format!("ready-{session}.txt")).exists();
        let starts = || -> Vec<(String, String, u64, String)> {
            let mut starts = Vec::new();
            for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(session) = name.strip_prefix("start-").and_then(|rest| rest.strip_suffix(".txt")) {
                    let text = std::fs::read_to_string(entry.path()).unwrap();
                    let mut parts = text.split(' ');
                    starts.push((
                        session.to_owned(),
                        parts.next().unwrap().to_owned(),
                        parts.next().unwrap().parse().unwrap(),
                        parts.next().unwrap().to_owned(),
                    ));
                }
            }
            starts
        };
        let victims = children.iter().filter(|(role, _, _)| *role == "victim").count();
        let deadline = Instant::now() + Duration::from_secs(90);
        while starts().iter().filter(|(session, role, _, _)| role == "victim" && ready(session)).count() < victims {
            assert!(Instant::now() < deadline, "seed {seed}: the victims never became ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        for (role, child, kill_at) in &mut children {
            match *role {
                "clean" => assert!(child.wait().unwrap().success(), "seed {seed}: a clean child failed"),
                "victim" => {
                    child.kill().unwrap();
                    child.wait().unwrap();
                }
                _ => {
                    std::thread::sleep(kill_at.saturating_duration_since(Instant::now()));
                    child.kill().unwrap();
                    child.wait().unwrap();
                }
            }
        }

        // One more process takes whatever the dead ones left.
        let tracer = shared_tracer(&dir);
        let own = tracer.session_id();
        let install_id = tracer.install_id();
        assert!(tracer.flush_blocking(Duration::from_secs(60)));
        drop(tracer);
        // Nothing is left but, at most, the bare folder of a child that was killed while it was still
        // setting it up: no header, no marker, no items. Those are swept once they are a minute old.
        for folder in std::fs::read_dir(dir.spool()).unwrap().flatten() {
            let inside: Vec<String> =
                std::fs::read_dir(folder.path()).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
            assert!(inside.iter().all(|name| name == "lock"), "seed {seed}: a session folder was left with {inside:?}");
        }

        let items = sent_items(&dir);
        assert!(items.iter().all(|(envelope, _)| envelope.install_id == install_id), "seed {seed}: the install id was not the same for all");
        let starts = starts();
        assert!(starts.len() >= count - count / 10, "seed {seed}: only {} of {count} children started", starts.len());
        for (session, role, events, child_install) in &starts {
            assert_eq!(child_install, &install_id, "seed {seed}");
            let of_session: Vec<_> = items.iter().filter(|(envelope, _)| &envelope.session_id == session).map(|(_, item)| &item.body).collect();
            let count_of = |pick: &dyn Fn(&ItemBody) -> bool| of_session.iter().filter(|body| pick(body)).count() as u64;
            let sent_events = count_of(&|body| matches!(body, ItemBody::Event(_)));
            let starts_sent = count_of(&|body| matches!(body, ItemBody::SessionStart(_)));
            let clean_ends = count_of(&|body| matches!(body, ItemBody::SessionEnd(end) if end.status == tracer_sdk::proto::SessionStatus::Exited));
            let abnormal_ends =
                count_of(&|body| matches!(body, ItemBody::SessionEnd(end) if end.status == tracer_sdk::proto::SessionStatus::Abnormal));
            let unclean: Vec<_> = of_session
                .iter()
                .filter_map(|body| match body {
                    ItemBody::Crash(crash) if crash.kind == tracer_sdk::proto::CrashKind::UncleanExit => Some(crash),
                    _ => None,
                })
                .collect();
            let what = format!(
                "seed {seed}, {role} session {session}: {sent_events}/{events} events, {starts_sent} starts, {clean_ends} clean and {abnormal_ends} abnormal ends, {} unclean exits",
                unclean.len()
            );
            assert_eq!(starts_sent, 1, "{what}");
            match role.as_str() {
                "clean" => assert!(sent_events == *events && clean_ends == 1 && abnormal_ends == 0 && unclean.is_empty(), "{what}"),
                "victim" => {
                    assert!(sent_events == *events && clean_ends == 0 && abnormal_ends == 1 && unclean.len() == 1, "{what}");
                    assert!(unclean[0].breadcrumbs.iter().any(|crumb| crumb.message.contains(session.as_str())), "{what}: its log line is missing");
                }
                _ => assert!(sent_events <= *events && clean_ends == 0 && abnormal_ends == 1 && unclean.len() == 1, "{what}"),
            }
        }
        // Nothing was sent under a session nobody started.
        let known: HashSet<&str> = starts.iter().map(|(session, _, _, _)| session.as_str()).chain([own.as_str()]).collect();
        let strangers: HashSet<&str> =
            items.iter().map(|(envelope, _)| envelope.session_id.as_str()).filter(|session| !known.contains(session)).collect();
        // A child killed before it could write its start note may still have got as far as its marker.
        assert!(strangers.len() <= count / 10 + 1, "seed {seed}: items under unknown sessions {strangers:?}");
    });
}
