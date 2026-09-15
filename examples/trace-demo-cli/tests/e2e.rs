#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! End-to-end tests: the real SDK inside the real `demo-cli` process, talking to the real server.
//!
//! Each test starts the server in-process on an ephemeral port with a temp database, runs `demo-cli` as a
//! child process, and then looks at what the server stored by opening its SQLite file read-only.

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ashlar_trace_proto::{AppInfo, Catalog, CatalogEvent, Envelope, Event, Item, ItemBody, ProcessKind, SessionStart, SessionTotals};
use ashlar_trace_server::config::Config;
use ashlar_trace_server::http::{self, AppState};
use ashlar_trace_server::store::{SqliteStore, Store};
use rusqlite::{Connection, OpenFlags, params};

const WAIT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------------------------------------
// the server

struct Server {
    dir: tempfile::TempDir,
    store: Arc<SqliteStore>,
    key: String,
    db_path: PathBuf,
    addr: Option<SocketAddr>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// A database with one app named "demo" and its write key. Nothing listens yet.
    fn offline() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config = Config { data_dir: dir.path().join("server"), rate_per_min: 0, secure_cookies: false, ..Config::default() };
        std::fs::create_dir_all(&config.data_dir).unwrap();
        let db_path = config.db_path();
        let store = Arc::new(SqliteStore::open(&db_path).unwrap());
        let key = ashlar_trace_server::cli::new_write_key().unwrap();
        store.create_app("demo", "Demo", &key, ashlar_trace_server::now_ms()).unwrap();
        Self { dir, store, key, db_path, addr: None, shutdown: None, thread: None }
    }

    /// A database with one app, listening on an ephemeral port.
    fn start() -> Self {
        let mut server = Self::offline();
        server.listen("127.0.0.1:0".parse().unwrap());
        server
    }

    fn listen(&mut self, bind: SocketAddr) {
        let config = Config { data_dir: self.dir.path().join("server"), rate_per_min: 0, secure_cookies: false, ..Config::default() };
        let state = AppState::new(self.store.clone() as Arc<dyn Store>, config);
        let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        self.thread = Some(std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(bind).await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();
                http::serve(listener, http::router(state), async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
            });
        }));
        self.addr = Some(addr_rx.recv().unwrap());
        self.shutdown = Some(shutdown);
    }

    fn base(&self) -> String {
        format!("http://{}", self.addr.unwrap())
    }

    fn db(&self) -> Db {
        Db { path: self.db_path.clone() }
    }

    fn rollup(&self) {
        ashlar_trace_server::jobs::rollup_recent(self.store.as_ref(), ashlar_trace_server::now_ms(), 3, 90).unwrap();
    }

    fn demo(&self) -> Demo {
        Demo {
            key: self.key.clone(),
            endpoint: self.addr.map(|addr| format!("http://{addr}")).unwrap_or_default(),
            data_dir: tempfile::tempdir().unwrap(),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Read-only access to the server's SQLite file.
struct Db {
    path: PathBuf,
}

impl Db {
    fn conn(&self) -> Connection {
        Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
    }

    fn count(&self, sql: &str) -> i64 {
        self.conn().query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn text(&self, sql: &str) -> String {
        self.conn().query_row(sql, [], |row| row.get(0)).unwrap()
    }

    fn strings(&self, sql: &str) -> Vec<String> {
        let conn = self.conn();
        let mut statement = conn.prepare(sql).unwrap();
        statement.query_map([], |row| row.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
    }

    /// Rows of the table that belong to `install`.
    fn rows_of(&self, install: &str) -> i64 {
        let conn = self.conn();
        ["installs", "sessions", "events", "perf", "crashes", "daily_installs"]
            .iter()
            .map(|table| {
                conn.query_row(&format!("SELECT count(*) FROM {table} WHERE install_id = ?1"), params![install], |row| row.get::<_, i64>(0)).unwrap()
            })
            .sum()
    }
}

/// Polls `check` until it returns true, or fails the test after `WAIT` with `what`.
fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

// ---------------------------------------------------------------------------------------------------------
// the host process

struct Demo {
    key: String,
    endpoint: String,
    data_dir: tempfile::TempDir,
}

struct Run {
    status: std::process::ExitStatus,
    stderr: String,
}

impl Demo {
    fn command(&self, scenario: &str, extra: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_demo-cli"));
        command
            .arg(scenario)
            .args(["--fast", "--data-dir"])
            .arg(self.data_dir.path())
            .args(["--endpoint", &self.endpoint, "--key", &self.key])
            .args(extra)
            .env_remove("DO_NOT_TRACK")
            .env_remove("TRACER_DISABLED")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn spawn(&self, scenario: &str, extra: &[&str]) -> Child {
        self.command(scenario, extra).spawn().unwrap()
    }

    fn run(&self, scenario: &str, extra: &[&str]) -> Run {
        finish(self.spawn(scenario, extra))
    }

    fn sdk_dir(&self) -> PathBuf {
        self.data_dir.path().join("tracer")
    }

    fn install_id(&self) -> String {
        let text = std::fs::read_to_string(self.sdk_dir().join("install.json")).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["install_id"].as_str().unwrap().to_owned()
    }
}

/// Waits for a child with a deadline, so a hung scenario fails the test instead of the run.
fn finish(mut child: Child) -> Run {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let mut stderr = String::new();
            std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
            let mut stdout = String::new();
            std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
            assert_eq!(stdout, "", "demo-cli must not write to stdout");
            return Run { status, stderr };
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("demo-cli did not finish in 30 s");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap()
}

// ---------------------------------------------------------------------------------------------------------
// a dashboard login

struct Browser {
    base: String,
    agent: ureq::Agent,
    cookie: String,
}

impl Browser {
    fn login(server: &Server) -> Self {
        let hash = ashlar_trace_server::cli::hash_password("correct horse battery").unwrap();
        server.store.create_admin("admin", &hash, ashlar_trace_server::now_ms()).unwrap();
        let agent: ureq::Agent =
            ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).max_redirects_will_error(false).build().into();
        let base = server.base();
        let mut page = agent.get(format!("{base}/login")).call().unwrap();
        let form_cookies = cookies(&page);
        let form = page.body_mut().read_to_string().unwrap();
        let marker = "name=\"csrf\" value=\"";
        let from = form.find(marker).unwrap() + marker.len();
        let csrf = &form[from..from + form[from..].find('"').unwrap()];
        let response = agent
            .post(format!("{base}/login"))
            .header("cookie", &form_cookies)
            .header("content-type", "application/x-www-form-urlencoded")
            .send(format!("username=admin&password=correct+horse+battery&csrf={csrf}"))
            .unwrap();
        assert_eq!(response.status().as_u16(), 303, "login should redirect");
        let cookie = cookies(&response).split("; ").find(|cookie| cookie.starts_with("tracer_session=")).expect("a session cookie").to_owned();
        Self { base, agent, cookie }
    }

    fn get(&self, path: &str) -> (u16, String) {
        let mut response = self.agent.get(format!("{}{path}", self.base)).header("cookie", &self.cookie).call().unwrap();
        (response.status().as_u16(), response.body_mut().read_to_string().unwrap())
    }
}

/// The `name=value` part of every cookie a response sets, joined for a `Cookie` header.
fn cookies<B>(response: &ureq::http::Response<B>) -> String {
    let all = response.headers().get_all("set-cookie").iter().filter_map(|value| value.to_str().ok());
    all.filter_map(|line| line.split(';').next()).collect::<Vec<_>>().join("; ")
}

// ---------------------------------------------------------------------------------------------------------
// hand-built envelopes

fn now() -> u64 {
    ashlar_trace_server::now_ms() as u64
}

fn envelope(version: &str, install: &str, session: &str, items: Vec<Item>) -> Envelope {
    Envelope {
        schema: 1,
        app: AppInfo { name: "demo".into(), version: version.into(), build: None, process: ProcessKind::Gui },
        install_id: install.into(),
        session_id: session.into(),
        device: None,
        sent_at: now(),
        items,
    }
}

fn item(id: &str, body: ItemBody) -> Item {
    Item { id: id.into(), ts: now() - 1000, body }
}

fn post_envelope(server: &Server, envelope: &Envelope) -> serde_json::Value {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut response =
        agent.post(format!("{}/v1/ingest", server.base())).header("x-tracer-key", &server.key).send(serde_json::to_vec(envelope).unwrap()).unwrap();
    assert_eq!(response.status().as_u16(), 200);
    json(&response.body_mut().read_to_string().unwrap())
}

// ---------------------------------------------------------------------------------------------------------
// scenarios

#[test]
fn normal_session_is_stored_in_full() {
    let server = Server::start();
    let demo = server.demo();
    let run = demo.run("normal", &["--version", "1.2.3", "--build", "abc1234"]);
    assert!(run.status.success(), "{}", run.stderr);
    let db = server.db();
    wait_for("the session to end", || db.count("SELECT count(*) FROM sessions WHERE status = 'exited'") == 1);

    // One install, one session, ended cleanly, with plausible totals.
    assert_eq!(db.count("SELECT count(*) FROM installs"), 1);
    assert_eq!(db.text("SELECT install_id FROM installs"), demo.install_id());
    assert_eq!(db.count("SELECT count(*) FROM sessions"), 1);
    assert_eq!(db.text("SELECT version FROM sessions"), "1.2.3");
    assert_eq!(db.text("SELECT build FROM sessions"), "abc1234");
    assert_eq!(db.text("SELECT process FROM sessions"), "gui");
    let duration = db.count("SELECT duration_s FROM sessions");
    assert!((1..=10).contains(&duration), "duration_s = {duration}");
    assert_eq!(db.count("SELECT focused_s + idle_s - duration_s FROM sessions"), 0, "focused + idle should add up to the duration");
    assert!(db.count("SELECT active_s FROM sessions") <= duration);
    assert_eq!(db.count("SELECT errors FROM sessions"), 1, "one handled error");

    // The tracked events, with their category, props and the dynamic id.
    assert_eq!(
        db.strings("SELECT name || ':' || category || ':' || dynamic FROM events ORDER BY name"),
        ["edit.palette_command:edit:1", "edit.undo:edit:0", "file.open:file:0", "file.save:file:0", "view.zoom:view:0"]
    );
    let props = json(&db.text("SELECT props FROM events WHERE name = 'file.save'"));
    assert_eq!(props, json(r#"{"count":3,"format":"native","ratio":0.5,"recent":true}"#));

    // Performance: the frame histogram, a counter and a gauge, and the startup time.
    assert_eq!(db.count("SELECT sum(frame_count) FROM perf"), 42);
    assert_eq!(db.count("SELECT sum(frozen) FROM perf"), 1);
    // A frozen frame is also slower than 16.7 ms, so "slow" counts both the 30 ms frame and the 800 ms one.
    assert_eq!(db.count("SELECT sum(slow) FROM perf"), 2);
    assert!(db.count("SELECT max(frame_max_us) FROM perf") >= 800_000);
    assert!(db.count("SELECT count(*) FROM perf WHERE startup_ms IS NOT NULL") >= 1, "first_frame() should give a startup time");
    let counters = db.strings("SELECT counters FROM perf WHERE counters IS NOT NULL AND counters != '{}'");
    assert_eq!(counters.iter().map(|text| json(text)["saves"].as_u64().unwrap_or(0)).sum::<u64>(), 7, "{counters:?}");
    let gauges = db.strings("SELECT gauges FROM perf WHERE gauges IS NOT NULL AND gauges != '{}'");
    assert!(gauges.iter().any(|text| json(text)["layers"].as_f64() == Some(4.0)), "{gauges:?}");

    // The handled error is an issue of kind `handled`, with the log lines as breadcrumbs.
    assert_eq!(db.count("SELECT count(*) FROM issues"), 1);
    assert_eq!(db.text("SELECT kind FROM issues"), "handled");
    assert_eq!(db.text("SELECT title FROM issues"), "could not write the preview");
    assert_eq!(db.count("SELECT fatal FROM crashes"), 0);
    assert_eq!(db.text("SELECT context FROM crashes"), "preview.write");
    let breadcrumbs = db.text("SELECT breadcrumbs FROM crashes");
    assert!(breadcrumbs.contains("opened a document") && breadcrumbs.contains("disk is almost full"), "{breadcrumbs}");

    // The catalog is stored.
    assert_eq!(
        db.strings("SELECT name || ':' || category FROM catalog_events WHERE version = '1.2.3' ORDER BY name"),
        ["edit.undo:edit", "file.open:file", "file.save:file", "view.zoom:view"]
    );

    // Nothing is left on the device.
    let leftovers = std::fs::read_dir(demo.sdk_dir().join("spool")).map_or(0, Iterator::count);
    assert_eq!(leftovers, 0, "a fully delivered session leaves no spool folder");

    // Rollups and the dashboard.
    server.rollup();
    assert_eq!(db.count("SELECT sessions FROM daily_sessions"), 1);
    assert_eq!(db.count("SELECT opens FROM daily_sessions"), 1);
    assert_eq!(db.count("SELECT exited FROM daily_sessions"), 1);
    assert_eq!(db.count("SELECT errored FROM daily_sessions"), 1);
    assert_eq!(db.count("SELECT count(*) FROM daily_installs"), 1);
    assert_eq!(db.count("SELECT count(*) FROM daily_events"), 5);
    assert_eq!(db.count("SELECT frozen FROM daily_perf"), 1);

    let browser = Browser::login(&server);
    let (status, overview) = browser.get("/apps/demo");
    assert_eq!(status, 200);
    assert!(overview.contains("Demo") && overview.contains("could not write the preview"), "{overview}");
    let (status, crashes) = browser.get("/apps/demo/crashes");
    assert_eq!(status, 200);
    assert!(crashes.contains("could not write the preview"));
    let (status, usage) = browser.get("/apps/demo/usage");
    assert_eq!(status, 200);
    assert!(usage.contains("file.open"));
    let (status, report) = browser.get("/apps/demo/reports?version=1.2.3&format=json");
    assert_eq!(status, 200, "{report}");
    let report = json(&report);
    assert_eq!(report["release"]["session"]["sessions"], 1);
    assert_eq!(report["release"]["session"]["crash_free_sessions"], 1.0);
    assert_eq!(report["release"]["perf"]["frames"], 42);
    assert_eq!(report["release"]["perf"]["max_us"], 800_000);
    assert_eq!(report["top_issues"][0]["title"], "could not write the preview");
    assert_eq!(report["top_issues"][0]["kind"], "handled");
}

#[test]
fn panic_is_reported_as_a_crashed_session_with_one_panic_issue() {
    let server = Server::start();
    let demo = server.demo();
    let run = demo.run("panic", &[]);
    assert_eq!(run.status.code(), Some(101), "{}", run.stderr);
    assert!(run.stderr.contains("demo panic in the main loop"));

    let relaunch = demo.run("relaunch", &[]);
    assert!(relaunch.status.success(), "{}", relaunch.stderr);
    let db = server.db();
    wait_for("the panic to arrive", || db.count("SELECT count(*) FROM crashes") >= 1);

    assert_eq!(db.strings("SELECT status FROM sessions ORDER BY started_at"), ["crashed", "exited"]);
    assert_eq!(db.count("SELECT count(*) FROM issues"), 1);
    assert_eq!(db.text("SELECT kind FROM issues"), "panic");
    assert_eq!(db.count("SELECT count(*) FROM crashes WHERE kind = 'unclean_exit'"), 0, "a panic is not also an unclean exit");
    assert_eq!(db.count("SELECT count(*) FROM crashes"), 1);
    assert_eq!(db.text("SELECT message FROM crashes"), "demo panic in the main loop");
    assert!(db.text("SELECT location FROM crashes").contains("main.rs"));
    // the host's own frame: `demo_cli::crash_on_purpose` in debug builds; release builds
    // (line-tables-only debug info) name inlined frames without the crate path
    let backtrace = db.text("SELECT backtrace FROM crashes");
    assert!(backtrace.contains("crash_on_purpose"), "the backtrace should name the host's frames:\n{backtrace}");
    let breadcrumbs = db.text("SELECT breadcrumbs FROM crashes");
    assert!(breadcrumbs.contains("opened a document") && breadcrumbs.contains("about to fail"), "{breadcrumbs}");
    assert_eq!(db.text("SELECT session_id FROM crashes"), db.text("SELECT session_id FROM sessions WHERE status = 'crashed'"));
}

#[test]
fn panic_that_ends_the_process_is_fatal() {
    let server = Server::start();
    let demo = server.demo();
    assert_eq!(demo.run("panic", &[]).status.code(), Some(101));
    assert!(demo.run("relaunch", &[]).status.success());
    let db = server.db();
    wait_for("the panic to arrive", || db.count("SELECT count(*) FROM crashes") >= 1);
    assert_eq!(db.count("SELECT fatal FROM crashes WHERE kind = 'panic'"), 1);
}

#[test]
fn nothing_reaches_the_server_before_the_first_run_notice_is_acknowledged() {
    let server = Server::start();
    let demo = server.demo();
    // The first run records a whole session but its user never closes the notice.
    let run = demo.run("normal", &["--first-run", "--wrap-logger"]);
    assert!(run.status.success(), "{}", run.stderr);
    assert!(run.stderr.contains("notice_shown"), "the host should be told why nothing was sent: {}", run.stderr);
    let db = server.db();
    assert_eq!(db.count("SELECT count(*) FROM sessions") + db.count("SELECT count(*) FROM installs") + db.count("SELECT count(*) FROM events"), 0);
    assert!(demo.sdk_dir().join("spool").read_dir().unwrap().count() >= 1, "what was recorded is kept on disk");

    // On the next run the notice is acknowledged, and both sessions arrive.
    let relaunch = demo.run("relaunch", &[]);
    assert!(relaunch.status.success(), "{}", relaunch.stderr);
    wait_for("both sessions to arrive", || db.count("SELECT count(*) FROM sessions WHERE status = 'exited'") == 2);
    assert!(db.count("SELECT count(*) FROM events") >= 1);
}

#[test]
fn abort_is_found_on_the_next_launch_as_an_unclean_exit() {
    let server = Server::start();
    let demo = server.demo();
    let run = demo.run("abort", &[]);
    assert!(!run.status.success());
    // The session start went out while the process was alive; its end never did.
    assert_eq!(db_count(&server, "SELECT count(*) FROM sessions WHERE status != 'ok'"), 0);

    let relaunch = demo.run("relaunch", &[]);
    assert!(relaunch.status.success(), "{}", relaunch.stderr);
    let db = server.db();
    wait_for("the unclean exit to arrive", || db.count("SELECT count(*) FROM crashes") >= 1);

    assert_eq!(db.strings("SELECT status FROM sessions ORDER BY started_at"), ["abnormal", "exited"]);
    assert_eq!(db.count("SELECT count(*) FROM crashes"), 1);
    assert_eq!(db.text("SELECT kind FROM crashes"), "unclean_exit");
    assert_eq!(db.text("SELECT kind FROM issues"), "unclean_exit");
    let breadcrumbs = db.text("SELECT breadcrumbs FROM crashes");
    assert!(breadcrumbs.contains("about to abort"), "{breadcrumbs}");
    assert_eq!(db.text("SELECT session_id FROM crashes"), db.text("SELECT session_id FROM sessions WHERE status = 'abnormal'"));
}

#[test]
fn a_wrapped_host_logger_still_feeds_breadcrumbs_and_still_receives_every_line() {
    let server = Server::start();
    let demo = server.demo();
    let run = demo.run("abort", &["--wrap-logger"]);
    assert!(run.stderr.contains("[WARN] about to abort"), "the host's own logger must still see the line: {}", run.stderr);
    assert!(demo.run("relaunch", &[]).status.success());
    let db = server.db();
    wait_for("the unclean exit to arrive", || db.count("SELECT count(*) FROM crashes") >= 1);
    assert!(db.text("SELECT breadcrumbs FROM crashes").contains("about to abort"));
}

fn db_count(server: &Server, sql: &str) -> i64 {
    server.db().count(sql)
}

#[test]
fn hang_is_reported_with_the_time_blocked() {
    let server = Server::start();
    let demo = server.demo();
    let run = demo.run("hang", &["--hang-ms", "400"]);
    assert!(run.status.success(), "{}", run.stderr);
    let db = server.db();
    wait_for("the hang to arrive", || db.count("SELECT count(*) FROM crashes WHERE kind = 'hang'") >= 1);

    assert_eq!(db.count("SELECT count(*) FROM issues"), 1);
    assert_eq!(db.text("SELECT kind FROM issues"), "hang");
    assert_eq!(db.count("SELECT count(*) FROM crashes"), 1, "one report per hang");
    assert!(db.count("SELECT blocked_ms FROM crashes") >= 400, "blocked_ms = {}", db.count("SELECT blocked_ms FROM crashes"));
    assert_eq!(db.count("SELECT fatal FROM crashes"), 0);
    wait_for("the session to end", || db.count("SELECT count(*) FROM sessions WHERE status = 'exited'") == 1);
}

#[test]
fn child_that_aborts_is_reported_with_its_signal() {
    let server = Server::start();
    let demo = server.demo();
    let run = demo.run("child", &[]);
    assert!(run.status.success(), "{}", run.stderr);
    let db = server.db();
    wait_for("the child exit to arrive", || db.count("SELECT count(*) FROM crashes WHERE kind = 'child_exit'") >= 1);

    assert_eq!(db.count("SELECT count(*) FROM crashes WHERE kind = 'child_exit'"), 1);
    // The child died without ending its own session; the next launch reports that too.
    assert!(demo.run("relaunch", &[]).status.success());
    wait_for("the child's own unclean exit to arrive", || db.count("SELECT count(*) FROM crashes WHERE kind = 'unclean_exit'") >= 1);
    assert_eq!(db.count("SELECT count(*) FROM sessions WHERE process = 'child' AND status = 'abnormal'"), 1);
    assert_eq!(db.count("SELECT count(*) FROM sessions WHERE process = 'gui' AND status = 'exited'"), 2);
    #[cfg(unix)]
    assert_eq!(db.count("SELECT signal FROM crashes WHERE kind = 'child_exit'"), 6, "SIGABRT");
    #[cfg(not(unix))]
    assert!(db.count("SELECT count(*) FROM crashes WHERE kind = 'child_exit' AND exit_code IS NOT NULL") == 1);
    assert!(
        db.text("SELECT context FROM crashes WHERE kind = 'child_exit'").contains("demo-child")
            || db.text("SELECT message FROM crashes WHERE kind = 'child_exit'").contains("demo-child")
    );
}

#[test]
fn data_recorded_offline_arrives_exactly_once_and_a_replay_changes_nothing() {
    let mut server = Server::offline();
    // Reserve a port nobody listens on, so the first run is refused.
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut demo = server.demo();
    demo.endpoint = format!("http://{bind}");

    let run = demo.run("normal", &["--flush-timeout-ms", "600"]);
    assert!(run.status.success(), "{}", run.stderr);
    assert!(run.stderr.contains("flush timed out"), "the run should know it could not deliver: {}", run.stderr);
    let spool = demo.sdk_dir().join("spool");
    assert_eq!(std::fs::read_dir(&spool).unwrap().count(), 1, "the undelivered session stays on disk");
    let backup = tempfile::tempdir().unwrap();
    copy_dir(&spool, backup.path());

    server.listen(bind);
    let relaunch = demo.run("relaunch", &[]);
    assert!(relaunch.status.success(), "{}", relaunch.stderr);
    let db = server.db();
    wait_for("the old session to arrive", || db.count("SELECT count(*) FROM sessions WHERE status = 'exited'") == 2);
    assert_eq!(db.count("SELECT count(*) FROM events"), 5);
    assert_eq!(db.count("SELECT count(*) FROM crashes"), 1);
    assert_eq!(db.count("SELECT sum(frame_count) FROM perf"), 42);
    assert_eq!(db.count("SELECT count(*) FROM installs"), 1);
    let counts = [
        db.count("SELECT count(*) FROM events"),
        db.count("SELECT count(*) FROM perf"),
        db.count("SELECT count(*) FROM crashes"),
        db.count("SELECT count FROM issues"),
        db.count("SELECT count(*) FROM sessions"),
    ];
    assert_eq!(std::fs::read_dir(&spool).unwrap().count(), 0, "everything delivered, nothing left");

    // Put the same spool back, as if the client had never heard the answer, and run again.
    copy_dir(backup.path(), &spool);
    let replay = demo.run("relaunch", &[]);
    assert!(replay.status.success(), "{}", replay.stderr);
    assert_eq!(std::fs::read_dir(&spool).unwrap().count(), 0, "the replayed folder is sent and removed before the process exits");
    let again = [
        db.count("SELECT count(*) FROM events"),
        db.count("SELECT count(*) FROM perf"),
        db.count("SELECT count(*) FROM crashes"),
        db.count("SELECT count FROM issues"),
        db.count("SELECT count(*) FROM sessions") - 1, // the second relaunch has a session of its own
    ];
    assert_eq!(counts, again, "replaying delivered items must not change anything");
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn switching_usage_off_stops_sessions_and_events_but_not_crashes() {
    let server = Server::start();
    let demo = server.demo();
    // A long flush interval, so nothing is sent before the switch takes effect.
    let run = demo.run("normal", &["--no-usage", "--flush-ms", "60000"]);
    assert!(run.status.success(), "{}", run.stderr);
    let db = server.db();
    wait_for("the handled error to arrive", || db.count("SELECT count(*) FROM crashes") >= 1);

    assert_eq!(db.count("SELECT count(*) FROM events"), 0);
    assert_eq!(
        db.count("SELECT count(*) FROM sessions"),
        0,
        "no session rows when usage is off: {:?}",
        db.strings("SELECT session_id || ' ' || status || ' ' || duration_s || ' ' || errors FROM sessions")
    );
    assert_eq!(db.text("SELECT kind FROM crashes"), "handled");
    assert!(db.count("SELECT count(*) FROM perf") >= 1, "performance is still on");
}

#[test]
fn do_not_track_sends_nothing_and_creates_nothing() {
    // A listener that only counts connections, standing in for any server.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counter = connections.clone();
    std::thread::spawn(move || {
        for _stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
        }
    });

    let data_dir = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_demo-cli"));
    command
        .args(["normal", "--fast", "--run-ms", "600", "--key", "tk_x", "--data-dir"])
        .arg(data_dir.path())
        .args(["--endpoint", &format!("http://{addr}")])
        .env("DO_NOT_TRACK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let run = finish(command.spawn().unwrap());
    assert!(run.status.success(), "{}", run.stderr);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(connections.load(Ordering::SeqCst), 0, "no request may be made");
    assert!(!data_dir.path().join("tracer").exists(), "no tracer folder may be created");
}

#[test]
fn only_listened_categories_are_recorded() {
    let server = Server::start();
    let demo = server.demo();
    let run = demo.run("normal", &["--listen", "file"]);
    assert!(run.status.success(), "{}", run.stderr);
    let db = server.db();
    wait_for("the session to end", || db.count("SELECT count(*) FROM sessions WHERE status = 'exited'") == 1);

    assert_eq!(db.strings("SELECT name FROM events ORDER BY name"), ["file.open", "file.save"]);
    assert_eq!(db.count("SELECT count(*) FROM events WHERE category = 'edit' OR category = 'view'"), 0, "includes the dynamic id");
    // The catalog still lists everything the host declared.
    assert_eq!(db.count("SELECT count(*) FROM catalog_events"), 4);
}

#[test]
fn two_processes_on_one_data_dir_both_end_cleanly() {
    let server = Server::start();
    let demo = server.demo();
    let long = demo.spawn("normal", &["--run-ms", "2500"]);
    let short = demo.spawn("normal", &["--run-ms", "300"]);
    let short = finish(short);
    assert!(short.status.success(), "{}", short.stderr);
    let long = finish(long);
    assert!(long.status.success(), "{}", long.stderr);
    let db = server.db();
    wait_for("both sessions to end", || db.count("SELECT count(*) FROM sessions WHERE status = 'exited'") == 2);

    assert_eq!(db.count("SELECT count(*) FROM sessions"), 2);
    assert_eq!(db.count("SELECT count(*) FROM sessions WHERE status = 'abnormal'"), 0);
    assert_eq!(db.count("SELECT count(*) FROM crashes WHERE kind = 'unclean_exit'"), 0, "a live process must not be reported as dead");
    assert_eq!(db.count("SELECT count(*) FROM installs"), 1, "both processes agree on one install id");
    assert_eq!(db.count("SELECT count(*) FROM events"), 10);
}

#[test]
fn forget_deletes_the_install_and_the_next_run_has_a_new_id() {
    let server = Server::start();
    let demo = server.demo();
    assert!(demo.run("normal", &[]).status.success());
    let old = demo.install_id();
    let db = server.db();
    wait_for("the first run to arrive", || db.rows_of(&old) > 0);

    let run = demo.run("forget", &[]);
    assert!(run.status.success(), "{}", run.stderr);
    let new = demo.install_id();
    assert_ne!(old, new);
    wait_for("the server to forget the old install", || db.rows_of(&old) == 0);
    // The items the run queued after forgetting belong to the new id.
    let relaunch = demo.run("relaunch", &[]);
    assert!(relaunch.status.success(), "{}", relaunch.stderr);
    assert_eq!(demo.install_id(), new);
    wait_for("the new install to be seen", || db.count(&format!("SELECT count(*) FROM installs WHERE install_id = '{new}'")) == 1);
    assert_eq!(db.rows_of(&old), 0);
    let local = std::fs::read_to_string(demo.sdk_dir().join("install.json")).unwrap();
    assert!(!local.contains(&old), "the old id must be gone from the device: {local}");
}

#[test]
fn undeclared_events_are_stored_flagged_when_the_server_holds_the_catalog() {
    let server = Server::start();
    let event = |id: &str, name: &str| item(id, ItemBody::Event(Event { name: name.into(), category: "file".into(), ..Event::default() }));
    let catalog =
        Catalog { events: ["file.open", "file.save"].iter().map(|name| CatalogEvent { name: (*name).into(), category: "file".into() }).collect() };
    let start = item(
        "ee000000000000000000000000000001",
        ItemBody::SessionStart(SessionStart { catalog_hash: Some("hash-v2".into()), catalog: Some(catalog), ..SessionStart::default() }),
    );
    let reply = post_envelope(&server, &envelope("2.0.0", "aa", "bb", vec![start]));
    assert_eq!(reply["accepted"], 1);

    let items = vec![
        event("ee000000000000000000000000000002", "file.open"),
        event("ee000000000000000000000000000003", "file.undeclared"),
        event("ee000000000000000000000000000004", "file.save"),
        item(
            "ee000000000000000000000000000005",
            ItemBody::SessionHeartbeat(SessionTotals { seq: 1, duration_s: 1, focused_s: 1, active_s: 1, idle_s: 0 }),
        ),
    ];
    let reply = post_envelope(&server, &envelope("2.0.0", "aa", "bb", items.clone()));
    // The catalog is client input, so an undeclared name is never refused (that would lose real data if the
    // catalog were wrong or forged): it is stored and flagged.
    assert_eq!((reply["accepted"].as_u64(), reply["rejected"].as_u64(), reply["duplicates"].as_u64()), (Some(4), Some(0), Some(0)), "{reply}");
    assert_eq!(reply["need_catalog"], false);
    assert_eq!(server.db().strings("SELECT name FROM events ORDER BY name"), ["file.open", "file.save", "file.undeclared"]);
    assert_eq!(server.db().strings("SELECT name FROM events WHERE declared = 0"), ["file.undeclared"]);

    // The same envelope again: every item is a duplicate.
    let again = post_envelope(&server, &envelope("2.0.0", "aa", "bb", items));
    assert_eq!((again["accepted"].as_u64(), again["duplicates"].as_u64()), (Some(0), Some(4)), "{again}");
    assert_eq!(server.db().count("SELECT count(*) FROM events"), 3);
}
