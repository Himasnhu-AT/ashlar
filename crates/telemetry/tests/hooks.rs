#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Every hook against a fake server: the command chokepoint, saves, log breadcrumbs (redacted),
//! frames, the hang watchdog and its pause around native dialogs, an old opt-out overridden, and
//! no endpoint meaning no client at all.

use std::io::Read;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use ashlar_command::{CommandError, CommandSpec, Host, Journal, Registry};
use ashlar_telemetry::sdk::proto::{Envelope, ItemBody};
use ashlar_telemetry::sdk::transport::{Reply, Request, Transport, TransportError};
use ashlar_telemetry::sdk::{ConsentCategory, Tracer};
use ashlar_telemetry::{Off, Target, Telemetry, TelemetryConfig};
use serde_json::{Value, json};

/// Records every envelope the SDK sends.
#[derive(Clone, Default)]
struct Fake {
    sent: Arc<Mutex<Vec<Envelope>>>,
}

impl Transport for Fake {
    fn send(&mut self, r: &Request<'_>) -> Result<Reply, TransportError> {
        let mut body = r.body.to_vec();
        if r.gzip {
            body.clear();
            flate2::read::GzDecoder::new(r.body).read_to_end(&mut body).map_err(|e| TransportError::Network(e.to_string()))?;
        }
        if let Ok(env) = serde_json::from_slice::<Envelope>(&body) {
            self.sent.lock().unwrap().push(env);
        }
        Ok(Reply { status: 200, body: br#"{"accepted":1}"#.to_vec(), retry_after: None })
    }
}

impl Fake {
    fn items(&self) -> Vec<ItemBody> {
        self.sent.lock().unwrap().iter().flat_map(|e| e.items.iter().map(|i| i.body.clone())).collect()
    }
    fn types(&self) -> Vec<&'static str> {
        self.items().iter().map(ItemBody::type_name).collect()
    }
    fn events(&self) -> Vec<(String, Value)> {
        self.items()
            .into_iter()
            .filter_map(|b| match b {
                ItemBody::Event(e) => Some((e.name, serde_json::to_value(&e.props).unwrap())),
                _ => None,
            })
            .collect()
    }
}

fn cfg() -> TelemetryConfig {
    TelemetryConfig::new("demo", "1.0.0").save_commands(&["doc.save"])
}

fn client(dir: &Path, fake: &Fake, hang_ms: u64) -> Telemetry {
    let target = Target { endpoint: "http://127.0.0.1:9".into(), key: "tk_test".into(), host: "127.0.0.1".into() };
    let t = Telemetry::with_transport(&cfg(), &target, dir, fake.clone(), |c| {
        c.tick_interval = Duration::from_millis(10);
        c.flush_interval = Duration::from_millis(20);
        c.heartbeat_interval = Duration::from_millis(50);
        c.perf_window = Duration::from_millis(50);
        c.retry_min = Duration::from_millis(10);
        c.retry_max = Duration::from_millis(20);
        c.hang_threshold = Duration::from_millis(hang_ms);
    });
    assert!(t.is_enabled() && t.status().active && t.status().server.as_deref() == Some("127.0.0.1"));
    t
}

fn settle(t: &Telemetry) {
    std::thread::sleep(Duration::from_millis(150));
    assert!(t.tracer().flush_blocking(Duration::from_secs(5)), "nothing waits for a notice");
}

// ---- a host on the command chokepoint ----

#[derive(Default)]
struct Doc {
    journal: Journal,
}

impl Host for Doc {
    type Error = CommandError;
    fn journal(&mut self) -> &mut Journal {
        &mut self.journal
    }
}

static COMMANDS: LazyLock<Registry<Doc>> = LazyLock::new(|| {
    Registry::new(vec![
        CommandSpec::new("doc.edit", "Edit", |_, _| Ok(Value::Null)),
        CommandSpec::new("doc.save", "Save", |_, _| Ok(Value::Null)),
        CommandSpec::new("doc.fail", "Fail", |_, _| Err(CommandError::failed("doc.fail", "no"))),
        CommandSpec::new("view.zoomIn", "Zoom In", |_, _| Ok(Value::Null)),
    ])
    .with_validation(ashlar_command::Validation::Off)
});

#[test]
fn commands_saves_frames_and_logs_are_sent_with_no_interaction() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let t = client(dir.path(), &fake, 2000);
    let mut doc = Doc::default();
    doc.journal.set_observer(t.command_observer());
    COMMANDS.execute(&mut doc, "doc.edit", json!({"path": "/home/someone/secret.txt", "text": "typed"})).unwrap();
    COMMANDS.execute(&mut doc, "doc.save", json!({})).unwrap();
    assert!(COMMANDS.execute(&mut doc, "doc.fail", json!({})).is_err());
    for _ in 0..5 {
        COMMANDS.execute(&mut doc, "view.zoomIn", json!({})).unwrap();
    }
    std::thread::sleep(ashlar_telemetry::REPEAT_WINDOW + Duration::from_millis(20));
    COMMANDS.execute(&mut doc, "view.zoomIn", json!({})).unwrap();
    for _ in 0..20 {
        let _turn = t.turn();
        t.frame(Duration::from_millis(4));
    }
    t.input(true, true);
    // breadcrumbs: only attached to a crash, so report a handled error after a log line
    let logger = t.wrap_logger(Box::new(Keep));
    logger.log(&log::Record::builder().level(log::Level::Warn).target("demo").args(format_args!("could not open /home/someone/Trip.mov")).build());
    t.tracer().report_handled(&"a handled error", Some("test.error"));
    settle(&t);

    let types = fake.types();
    for want in ["session_start", "event", "perf", "crash"] {
        assert!(types.contains(&want), "{want} missing from {types:?}");
    }
    let events = fake.events();
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    for want in ["command.run", "doc.edit", "doc.save", "doc.fail", "view.zoom_in", "app.document_saved"] {
        assert!(names.contains(&want), "{want} missing from {names:?}");
    }
    let runs: Vec<&Value> = events.iter().filter(|(n, _)| n == "command.run").map(|(_, p)| p).collect();
    assert_eq!(runs.iter().filter(|p| p["id"] == "view.zoomIn").count(), 2, "five quick runs fold into one, then one more");
    assert!(runs.iter().any(|p| p["id"] == "view.zoomIn" && p["repeats"] == 4), "{runs:?}");
    assert!(runs.iter().any(|p| p["id"] == "doc.fail" && p["ok"] == false));
    let all = serde_json::to_string(&fake.items()).unwrap();
    assert!(!all.contains("secret.txt") && !all.contains("typed") && !all.contains("Trip.mov"), "no params or paths: {all}");
    let crash = fake.items().into_iter().find_map(|b| if let ItemBody::Crash(c) = b { Some(c) } else { None }).unwrap();
    let crumbs = serde_json::to_string(&crash.breadcrumbs).unwrap();
    assert!(crumbs.contains("could not open <path>"), "{crumbs}");
    assert!(fake.items().iter().any(|b| matches!(b, ItemBody::Perf(p) if p.startup_ms.is_some())), "first frame recorded");
}

/// A logger that keeps nothing (the app's own).
struct Keep;

impl log::Log for Keep {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, _: &log::Record<'_>) {}
    fn flush(&self) {}
}

#[test]
fn an_old_opt_out_is_overridden_from_the_first_item() {
    let dir = tempfile::tempdir().unwrap();
    // an earlier build: the notice never closed, every category switched off
    let old = Tracer::builder("tk_test", "demo", "0.1.0").data_dir(dir.path()).transport(Fake::default()).init();
    for cat in [ConsentCategory::Crash, ConsentCategory::Performance, ConsentCategory::Usage] {
        old.set_consent(cat, false);
    }
    assert!(old.notice_pending());
    drop(old);
    let fake = Fake::default();
    let t = client(dir.path(), &fake, 2000);
    for cat in [ConsentCategory::Crash, ConsentCategory::Performance, ConsentCategory::Usage] {
        assert!(t.tracer().consent(cat), "{cat:?}");
    }
    assert!(!t.tracer().notice_pending());
    t.command("doc.edit", true, Duration::from_millis(1));
    settle(&t);
    let types = fake.types();
    assert!(types.contains(&"session_start") && types.contains(&"event"), "{types:?}");
}

#[test]
fn hangs_are_reported_but_not_while_a_native_dialog_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let t = client(dir.path(), &fake, 100);
    t.pause_hangs_in_dialogs();
    {
        let _turn = t.turn();
        // a dialog the user leaves open: blocked, but not hung
        ashlar_native::modal::around(|| std::thread::sleep(Duration::from_millis(400)));
    }
    settle(&t);
    let hangs = || fake.items().iter().filter(|b| matches!(b, ItemBody::Crash(c) if c.kind == ashlar_telemetry::sdk::proto::CrashKind::Hang)).count();
    assert_eq!(hangs(), 0, "{:?}", fake.types());
    {
        let _turn = t.turn();
        std::thread::sleep(Duration::from_millis(400));
    }
    settle(&t);
    assert_eq!(hangs(), 1, "{:?}", fake.types());
    ashlar_native::modal::set_hook(None);
}

#[test]
fn no_endpoint_means_no_client_and_no_network() {
    let dir = tempfile::tempdir().unwrap();
    let t = Telemetry::start(&TelemetryConfig::new("demo", "1.0.0"), Some(dir.path()));
    assert!(!t.is_enabled());
    assert_eq!(t.status().off, Some(Off::NoEndpoint));
    assert!(t.command_observer().is_none());
    t.command("doc.edit", true, Duration::from_millis(1));
    t.quit();
    assert!(!dir.path().join("tracer").exists(), "nothing recorded");
}
