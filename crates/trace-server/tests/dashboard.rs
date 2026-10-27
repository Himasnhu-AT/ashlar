#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]
//! Dashboard tests: a real server, a small deterministic dataset ingested through the real route, then
//! rollups, then plain HTTP requests against every page.
mod common;

use std::collections::{BTreeMap, HashMap};

use ashlar_trace_proto::{
    AppInfo, Breadcrumb, Catalog, CatalogEvent, Crash, CrashKind, Device, Drops, Envelope, Event, FrameStats, Item, ItemBody, Perf, ProcessKind,
    SessionEnd, SessionStart, SessionStatus, SessionTotals,
};
use ashlar_trace_server::dashboard::auth::sha256_hex;
use ashlar_trace_server::dashboard::view::enc;
use ashlar_trace_server::ingest::DAY_MS;
use ashlar_trace_server::store::Store;
use common::{TestServer, now};

const SCRIPT: &str = "<script>alert(1)</script>";
const PASSWORD: &str = "correct horse battery";

// ---------------------------------------------------------------------------------------------------------
// a tiny browser: cookies, no redirects

struct Resp {
    status: u16,
    headers: HashMap<String, Vec<String>>,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).and_then(|values| values.first()).map_or("", String::as_str)
    }

    fn location(&self) -> &str {
        self.header("location")
    }
}

struct Browser {
    base: String,
    agent: ureq::Agent,
    cookies: BTreeMap<String, String>,
}

impl Browser {
    fn new(server: &TestServer) -> Self {
        let agent: ureq::Agent =
            ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).max_redirects_will_error(false).build().into();
        Self { base: server.base.clone(), agent, cookies: BTreeMap::new() }
    }

    /// A browser that holds a valid session, created directly in the database (no password hashing).
    fn signed_in(server: &TestServer) -> Self {
        let mut browser = Self::new(server);
        let token = "ab".repeat(32);
        ensure_admin(server);
        let admin_id = server.count("SELECT id FROM admins LIMIT 1");
        let now = ashlar_trace_server::now_ms();
        if server.count(&format!("SELECT count(*) FROM admin_sessions WHERE token_hash = '{}'", sha256_hex(&token))) == 0 {
            server.store.admin_session_create(&sha256_hex(&token), admin_id, now, now + DAY_MS).unwrap();
        }
        browser.cookies.insert("tracer_session".into(), token);
        browser
    }

    fn cookie_header(&self) -> String {
        self.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
    }

    fn finish(&mut self, mut response: ureq::http::Response<ureq::Body>) -> Resp {
        let mut headers: HashMap<String, Vec<String>> = HashMap::new();
        for (name, value) in response.headers() {
            headers.entry(name.as_str().to_string()).or_default().push(value.to_str().unwrap_or("").to_string());
        }
        for line in headers.get("set-cookie").cloned().unwrap_or_default() {
            let first = line.split(';').next().unwrap_or("");
            if let Some((name, value)) = first.split_once('=') {
                if line.contains("Max-Age=0") {
                    self.cookies.remove(name);
                } else {
                    self.cookies.insert(name.to_string(), value.to_string());
                }
            }
        }
        Resp { status: response.status().as_u16(), headers, body: response.body_mut().read_to_string().unwrap_or_default() }
    }

    fn get(&mut self, path: &str) -> Resp {
        let response = self.agent.get(format!("{}{path}", self.base)).header("cookie", self.cookie_header()).call().unwrap();
        self.finish(response)
    }

    fn post(&mut self, path: &str, fields: &[(&str, &str)]) -> Resp {
        let body = fields.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&");
        let response = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("cookie", self.cookie_header())
            .header("content-type", "application/x-www-form-urlencoded")
            .send(body)
            .unwrap();
        self.finish(response)
    }

    /// The CSRF token of the signed-in session, read from the app picker.
    fn csrf(&mut self) -> String {
        let page = self.get("/");
        assert_eq!(page.status, 200, "{}", page.body);
        between(&page.body, "name=\"csrf\" value=\"", "\"").to_string()
    }

    /// Signs in through the real form. Returns the response to the POST.
    fn login(&mut self, user: &str, password: &str) -> Resp {
        let form = self.get("/login");
        let csrf = between(&form.body, "name=\"csrf\" value=\"", "\"").to_string();
        self.post("/login", &[("username", user), ("password", password), ("csrf", &csrf)])
    }
}

fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let from = text.find(start).unwrap_or_else(|| panic!("{start:?} not found in {text}")) + start.len();
    let to = text[from..].find(end).unwrap() + from;
    &text[from..to]
}

fn ensure_admin(server: &TestServer) {
    if server.count("SELECT count(*) FROM admins") == 0 {
        let hash = ashlar_trace_server::cli::hash_password(PASSWORD).unwrap();
        server.store.create_admin("admin", &hash, ashlar_trace_server::now_ms()).unwrap();
    }
}

// ---------------------------------------------------------------------------------------------------------
// the dataset

fn today_start() -> u64 {
    (now() / DAY_MS as u64) * DAY_MS as u64
}

fn item(id: &str, offset: u64, body: ItemBody) -> Item {
    Item { id: id.into(), ts: today_start() + 1_000 + offset, body }
}

fn envelope(install: &str, session: &str, version: &str, os: &str, items: Vec<Item>) -> Envelope {
    Envelope {
        schema: 1,
        app: AppInfo { name: "demo".into(), version: version.into(), build: None, process: ProcessKind::Gui },
        install_id: install.into(),
        session_id: session.into(),
        device: Some(Device { os_name: os.into(), os_version: Some("14".into()), arch: "aarch64".into(), ..Device::default() }),
        sent_at: now(),
        items,
    }
}

fn event(id: &str, offset: u64, name: &str) -> Item {
    item(id, offset, ItemBody::Event(Event { name: name.into(), category: name.split('.').next().unwrap().into(), ..Event::default() }))
}

fn end(id: &str, offset: u64, status: SessionStatus, duration: u64, focused: u64, active: u64) -> Item {
    item(
        id,
        offset,
        ItemBody::SessionEnd(SessionEnd {
            status,
            totals: SessionTotals { seq: 1, duration_s: duration, focused_s: focused, active_s: active, idle_s: duration - active },
            errors: 0,
        }),
    )
}

fn frames(fast: u64, slow: u64) -> FrameStats {
    let mut stats = FrameStats::empty();
    for _ in 0..fast {
        stats.record(5_000);
    }
    for _ in 0..slow {
        stats.record(30_000);
    }
    stats
}

fn perf(id: &str, offset: u64, stats: FrameStats, extras: bool) -> Item {
    let mut p = Perf {
        window_s: 10,
        frames: Some(stats),
        cpu_pct: Some(12.5),
        rss_mb: Some(300),
        rss_peak_mb: Some(340),
        startup_ms: Some(250),
        ..Perf::default()
    };
    if extras {
        p.counters.insert("saves".into(), 7);
        p.gauges.insert("layers".into(), 4.0);
    }
    item(id, offset, ItemBody::Perf(p))
}

fn start(id: &str, catalog: bool) -> Item {
    let catalog = catalog.then(|| Catalog {
        events: ["file.open", "edit.undo", "export.pdf"]
            .into_iter()
            .map(|name| CatalogEvent { name: name.into(), category: name.split('.').next().unwrap().into() })
            .collect(),
    });
    item(id, 0, ItemBody::SessionStart(SessionStart { catalog_hash: Some("h1".into()), catalog, ..SessionStart::default() }))
}

/// The server with the demo dataset ingested and rolled up, plus an admin account.
///
/// Version 1.0.0: installs i1 and i2 on linux, i3 and i4 on macos. Sessions: s1 (i1) 600 s, s2 (i2) 300 s,
/// s3 (i3) crashes with a script tag in its message, s4 (i4) 200 s. Version 1.1.0: s5 (i1) 120 s with a
/// handled error. Events: file.open x4 from two installs, edit.undo x2 from one. Perf: 300 frames, 30 of
/// them slow, so p90 is a slow bucket. The catalog also declares `export.pdf`, which never fires.
fn seeded() -> TestServer {
    let server = TestServer::demo();
    let mut s1 = vec![start("s1-start", true)];
    for (i, name) in ["file.open", "file.open", "file.open", "edit.undo", "edit.undo"].iter().enumerate() {
        s1.push(event(&format!("s1-e{i}"), 10 + i as u64, name));
    }
    s1.push(perf("s1-perf", 100, frames(70, 30), true));
    s1.push(end("s1-end", 200, SessionStatus::Exited, 600, 500, 300));
    s1.push(item("s1-drops", 300, ItemBody::Drops(Drops { reasons: BTreeMap::from([("queue_full".to_string(), 5u64)]) })));
    let mut s2 = vec![start("s2-start", false), event("s2-e0", 10, "file.open")];
    s2.push(perf("s2-perf", 100, frames(100, 0), false));
    s2.push(end("s2-end", 200, SessionStatus::Exited, 300, 0, 0));
    let crash = Crash {
        kind: CrashKind::Panic,
        fatal: true,
        message: Some(format!("{SCRIPT} while saving")),
        location: Some("src/editor.rs:10:5".into()),
        thread: Some("main".into()),
        backtrace: Some("   0: demo::editor::save\n             at src/editor.rs:10\n   1: demo::main\n".into()),
        breadcrumbs: vec![
            Breadcrumb { ts: today_start() + 900, level: "info".into(), target: "demo::io".into(), message: "opened project".into() },
            Breadcrumb { ts: today_start() + 950, level: "error".into(), target: "demo::io".into(), message: format!("write failed {SCRIPT}") },
        ],
        perf: Some(Perf { window_s: 10, frames: Some(frames(9, 1)), cpu_pct: Some(40.0), rss_mb: Some(512), ..Perf::default() }),
        ..Crash::default()
    };
    let s3 = vec![start("s3-start", false), item("s3-crash", 100, ItemBody::Crash(crash)), end("s3-end", 200, SessionStatus::Crashed, 100, 0, 0)];
    let s4 = vec![start("s4-start", false), end("s4-end", 200, SessionStatus::Exited, 200, 0, 0)];
    let handled = Crash { kind: CrashKind::Handled, fatal: false, message: Some("disk full".into()), ..Crash::default() };
    let s5 = vec![start("s5-start", false), item("s5-crash", 100, ItemBody::Crash(handled)), end("s5-end", 200, SessionStatus::Exited, 120, 0, 0)];
    for (install, session, version, os, items) in [
        ("i1", "s1", "1.0.0", "linux", s1),
        ("i2", "s2", "1.0.0", "linux", s2),
        ("i3", "s3", "1.0.0", "macos", s3),
        ("i4", "s4", "1.0.0", "macos", s4),
        ("i1", "s5", "1.1.0", "linux", s5),
    ] {
        let reply = server.ingest(&envelope(install, session, version, os, items));
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.json()["rejected"], 0, "{}", reply.body);
    }
    let today = ashlar_trace_server::now_ms() / DAY_MS;
    server.store.rollup(today - 1, today).unwrap();
    ensure_admin(&server);
    server
}

fn issue_id(server: &TestServer, like: &str) -> i64 {
    server.count(&format!("SELECT id FROM issues WHERE title LIKE '%{like}%'"))
}

const PAGES: [&str; 8] = [
    "/",
    "/apps/demo",
    "/apps/demo/crashes",
    "/apps/demo/performance",
    "/apps/demo/usage",
    "/apps/demo/reports",
    "/apps/demo/reports?version=1.0.0",
    "/apps/demo/settings",
];

// ---------------------------------------------------------------------------------------------------------
// pages and numbers

#[test]
fn overview_shows_the_stability_and_activity_numbers() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let page = b.get("/apps/demo");
    assert_eq!(page.status, 200, "{}", page.body);
    // 5 sessions ended, 1 crashed: 80%. 4 distinct installs, 1 crashed: 75%.
    assert!(page.body.contains(">80.00%<"), "{}", page.body);
    assert!(page.body.contains(">75.00%<"));
    assert!(page.body.contains("4 of 5 ended sessions"));
    // Opens today: five GUI sessions. Active installs today, 7 and 30 days: the four distinct installs.
    assert!(page.body.contains("<dt>Opens today</dt><dd class=\"num\">5</dd>"), "{}", page.body);
    assert!(page.body.contains("<dt>Active installs, today</dt><dd class=\"num\">4</dd>"));
    assert!(page.body.contains("<dt>Active installs, 7 days</dt><dd class=\"num\">4</dd>"));
    assert!(page.body.contains("<dt>Active installs, 30 days</dt><dd class=\"num\">4</dd>"));
    // Charts and the newest open issues.
    assert!(page.body.contains("<svg"));
    assert!(page.body.contains("Newest open issues"));
    assert!(page.body.contains("disk full"));

    // Filtering to 1.0.0: 4 ended, 1 crashed.
    let v1 = b.get("/apps/demo?days=7&version=1.0.0");
    assert!(v1.body.contains(">75.00%<"));
    assert!(!v1.body.contains(">80.00%<"));
    assert!(v1.body.contains("3 of 4 ended sessions"));
    assert!(!v1.body.contains("disk full"));
    assert!(v1.body.contains("<option value=\"7\" selected>"));
}

#[test]
fn crash_list_filters_by_kind_version_and_status() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let all = b.get("/apps/demo/crashes");
    assert_eq!(all.status, 200);
    assert!(all.body.contains("disk full") && all.body.contains("while saving"));
    assert!(all.body.contains("Open"));

    let handled = b.get("/apps/demo/crashes?kind=handled");
    assert!(handled.body.contains("disk full") && !handled.body.contains("while saving"));
    let v10 = b.get("/apps/demo/crashes?version=1.0.0");
    assert!(v10.body.contains("while saving") && !v10.body.contains("disk full"));
    let resolved = b.get("/apps/demo/crashes?status=resolved");
    assert!(resolved.body.contains("No issues match these filters."));
    // Junk filter values are ignored rather than failing or reaching SQL.
    let junk = b.get("/apps/demo/crashes?kind=x%27%3B%20DROP%20TABLE%20issues&status=%00&days=abc&version=%27");
    assert_eq!(junk.status, 200);
    assert_eq!(server.count("SELECT count(*) FROM issues"), 2);
}

#[test]
fn issue_page_shows_details_and_escapes_everything() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let id = issue_id(&server, "while saving");
    let page = b.get(&format!("/apps/demo/crashes/{id}"));
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("demo::editor::save"));
    assert!(page.body.contains("src/editor.rs:10:5"));
    assert!(page.body.contains("opened project"));
    assert!(page.body.contains("macos 14"));
    assert!(page.body.contains("Last performance window"));
    assert!(page.body.contains("Resolve") && page.body.contains("Ignore"));
    assert!(page.body.contains("By version") && page.body.contains("By operating system"));
    // The script tag from the message and a breadcrumb is escaped, never raw.
    assert!(page.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!page.body.contains(SCRIPT));
    assert!(!page.body.contains("<script"));

    assert_eq!(b.get("/apps/demo/crashes/999999").status, 404);
    assert_eq!(b.get("/apps/demo/crashes/abc").status, 404);
    assert_eq!(b.get("/apps/nope/crashes").status, 404);
}

#[test]
fn performance_percentiles_come_from_merged_histograms() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let page = b.get("/apps/demo/performance");
    assert_eq!(page.status, 200, "{}", page.body);
    // 300 frames: 270 at 5 ms (bucket bound 6 ms) and 30 at 30 ms. Wait: 170 fast + 30 slow in 200 frames.
    let mut merged = frames(70, 30);
    merged.merge(&frames(100, 0));
    let ms = |q: f64| format!("{:.1} ms", f64::from(merged.quantile_us(q).unwrap()) / 1000.0);
    assert_eq!(ms(0.5), "6.0 ms");
    assert_eq!(ms(0.9), "30.0 ms");
    assert!(page.body.contains(&format!("<dt>Frame time p50</dt><dd class=\"num\">{}</dd>", ms(0.5))), "{}", page.body);
    assert!(page.body.contains(&format!("<dt>Frame time p90</dt><dd class=\"num\">{}</dd>", ms(0.9))));
    assert!(page.body.contains("<dt>Slow frames</dt><dd class=\"num\">15.00%</dd>"));
    assert!(page.body.contains("<dt>Frozen frames</dt><dd class=\"num\">0.00%</dd>"));
    assert!(page.body.contains("250 ms"));
    assert!(page.body.contains("12.5%"));
    // Breakdowns and host-defined metrics.
    assert!(page.body.contains("By version") && page.body.contains("<th scope=\"row\">1.0.0</th>"));
    assert!(page.body.contains("<th scope=\"row\">linux</th>"));
    assert!(page.body.contains("<th scope=\"row\">saves</th><td class=\"num\">7</td>"));
    assert!(page.body.contains("<th scope=\"row\">layers</th>"));
    // A version with no perf data renders a message, not a failure.
    let none = b.get("/apps/demo/performance?version=1.1.0");
    assert_eq!(none.status, 200);
    assert!(none.body.contains("No performance data in this range."));
}

#[test]
fn usage_ranks_events_and_lists_declared_but_unfired_ones() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let page = b.get("/apps/demo/usage");
    assert_eq!(page.status, 200, "{}", page.body);
    // 1320 s over 5 sessions: 4m 24s.
    assert!(page.body.contains("<dt>Average session</dt><dd class=\"num\">4m 24s</dd>"), "{}", page.body);
    assert!(page.body.contains("<dt>Sessions</dt><dd class=\"num\">5</dd>"));
    // file.open: 4 events from 2 installs; edit.undo: 2 events from 1 install.
    assert!(page.body.contains("<th scope=\"row\">file.open</th><td>file</td><td class=\"num\">4</td><td class=\"num\">2</td>"), "{}", page.body);
    assert!(page.body.contains("<th scope=\"row\">edit.undo</th><td>edit</td><td class=\"num\">2</td><td class=\"num\">1</td>"));
    let never = &page.body[page.body.find("Declared but never fired").unwrap()..];
    assert!(never.contains("export.pdf") && !never.contains("file.open"));
    assert!(page.body.contains("Active (focused, recent input)"));
}

#[test]
fn reports_agree_across_page_and_downloads() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let index = b.get("/apps/demo/reports");
    assert_eq!(index.status, 200);
    assert!(index.body.contains("1.1.0") && index.body.contains("1.0.0"));

    let page = b.get("/apps/demo/reports?version=1.0.0");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("Release report: Demo 1.0.0"));
    assert!(page.body.contains(">75.00%<"));
    assert!(page.body.contains("There is no earlier release to compare with."));
    assert!(page.body.contains("while saving"));

    let json = b.get("/apps/demo/reports?version=1.0.0&format=json");
    assert_eq!(json.status, 200);
    assert_eq!(json.header("content-type"), "application/json");
    assert_eq!(json.header("content-disposition"), "attachment; filename=\"tracer-report-demo-1.0.0.json\"");
    let parsed: serde_json::Value = serde_json::from_str(&json.body).unwrap();
    let session = &parsed["release"]["session"];
    assert_eq!(session["sessions"], 4);
    assert_eq!(session["crash_free_sessions"], 0.75);
    assert_eq!(session["crash_free_installs"], 0.75);
    assert_eq!(parsed["release"]["active_installs"], 4);
    assert_eq!(parsed["release"]["perf"]["frames"], 200);
    assert_eq!(parsed["release"]["perf"]["p90_us"], 30_000);
    assert_eq!(parsed["features"][0]["name"], "file.open");
    assert_eq!(parsed["features"][0]["count"], 4);
    assert_eq!(parsed["top_issues"][0]["title"], format!("{SCRIPT} while saving"));
    // The JSON agrees with the page: 0.75 is "75.00%".
    assert!(page.body.contains(&format!("{:.2}%", session["crash_free_sessions"].as_f64().unwrap() * 100.0)));
    assert!(!json.body.contains(SCRIPT));

    let md = b.get("/apps/demo/reports?version=1.0.0&format=md");
    assert_eq!(md.status, 200);
    assert!(md.header("content-type").starts_with("text/markdown"));
    assert!(md.header("content-disposition").contains("tracer-report-demo-1.0.0.md"));
    assert!(md.body.starts_with("# Release report: Demo 1.0.0"));
    assert!(md.body.contains("| Crash-free sessions | 75.00% |"));
    assert!(!md.body.contains(SCRIPT) && md.body.contains("&lt;script&gt;"));

    let html = b.get("/apps/demo/reports?version=1.0.0&format=html");
    assert_eq!(html.status, 200);
    assert!(html.header("content-type").starts_with("text/html"));
    assert!(html.header("content-disposition").contains("attachment; filename=\"tracer-report-demo-1.0.0.html\""));
    assert!(html.body.starts_with("<!DOCTYPE html>"));
    assert!(html.body.contains("--accent"), "stylesheet is inlined");
    assert!(html.body.contains("&lt;script&gt;alert(1)&lt;/script&gt;") && !html.body.contains(SCRIPT));
    assert!(html.header("content-security-policy").contains("default-src 'none'"));

    // The newer release compares with the older one.
    let newer = b.get("/apps/demo/reports?version=1.1.0");
    assert!(newer.body.contains("Compared with 1.0.0"));
    let newer_json: serde_json::Value = serde_json::from_str(&b.get("/apps/demo/reports?version=1.1.0&format=json").body).unwrap();
    assert_eq!(newer_json["previous"]["version"], "1.0.0");
    assert_eq!(newer_json["release"]["session"]["crash_free_sessions"], 1.0);

    // An unknown version, and a version that tries to escape the file name.
    assert_eq!(b.get("/apps/demo/reports?version=9.9.9").status, 404);
    assert_eq!(b.get("/apps/demo/reports?version=9.9.9&format=json").status, 404);
}

#[test]
fn settings_shows_drops_and_masks_keys() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let page = b.get("/apps/demo/settings");
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("<th scope=\"row\">queue_full</th><td class=\"num\">5</td>"), "{}", page.body);
    assert!(!page.body.contains(&server.key), "the full key is not listed");
    assert!(page.body.contains("tk_test\u{2026}demo"));
    assert!(page.body.contains("/v1/ingest"));
}

#[test]
fn an_empty_database_renders_every_page() {
    let server = TestServer::demo();
    ensure_admin(&server);
    let mut b = Browser::signed_in(&server);
    for path in PAGES.iter().filter(|p| !p.contains("version=")) {
        let page = b.get(path);
        assert_eq!(page.status, 200, "{path}: {}", page.body);
        assert!(!page.body.contains("NaN"), "{path}");
    }
    for path in ["/apps/demo", "/apps/demo/performance", "/apps/demo/usage", "/apps/demo/reports"] {
        let page = b.get(path);
        assert!(page.body.contains("No data yet for Demo"), "{path}");
        assert!(page.body.contains("/v1/ingest") && page.body.contains("Settings"));
    }
    assert!(b.get("/apps/demo/crashes").body.contains("No data yet"));
    assert_eq!(b.get("/apps/demo/reports?version=1.0.0").status, 404);
    // No apps at all.
    let dir = tempfile::tempdir().unwrap();
    let bare = TestServer::start(common::config_in(dir.path()));
    ensure_admin(&bare);
    let mut b = Browser::signed_in(&bare);
    let picker = b.get("/");
    assert_eq!(picker.status, 200);
    assert!(picker.body.contains("ashlar-trace-server app create"));
}

#[test]
fn every_page_renders_without_nan_and_with_security_headers() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let issue = format!("/apps/demo/crashes/{}", issue_id(&server, "disk full"));
    let mut paths: Vec<&str> = PAGES.to_vec();
    paths.push(&issue);
    paths.push("/apps/demo/usage?days=7&version=1.1.0");
    for path in paths {
        let page = b.get(path);
        assert_eq!(page.status, 200, "{path}");
        assert!(!page.body.contains("NaN"), "{path}");
        assert!(!page.body.contains("<script"), "{path}");
        assert!(!page.body.contains("style=\""), "{path}: inline styles would break the CSP");
        assert_eq!(page.header("x-content-type-options"), "nosniff", "{path}");
        assert_eq!(page.header("referrer-policy"), "no-referrer");
        assert_eq!(page.header("x-frame-options"), "DENY");
        assert_eq!(page.header("cache-control"), "no-store");
        let csp = page.header("content-security-policy");
        assert!(csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'") && !csp.contains("unsafe-inline"), "{csp}");
        assert!(page.header("content-type").starts_with("text/html"));
    }
    let css = b.get("/static/dashboard.css");
    assert_eq!(css.status, 200);
    assert!(css.header("content-type").starts_with("text/css"));
    assert!(css.body.contains("prefers-color-scheme: dark"));
    assert_eq!(css.header("x-content-type-options"), "nosniff");
    // The login page carries them too.
    let login = Browser::new(&server).get("/login");
    assert_eq!(login.header("x-frame-options"), "DENY");
    assert_eq!(login.header("cache-control"), "no-store");
}

// ---------------------------------------------------------------------------------------------------------
// authentication

#[test]
fn every_dashboard_route_requires_a_login() {
    let server = seeded();
    let mut b = Browser::new(&server);
    let issue = format!("/apps/demo/crashes/{}", issue_id(&server, "disk full"));
    for path in PAGES.iter().copied().chain([issue.as_str(), "/apps/demo/reports?version=1.0.0&format=json", "/apps/nope", "/apps/demo/crashes/abc"])
    {
        let page = b.get(path);
        assert_eq!(page.status, 303, "{path}");
        assert_eq!(page.location(), "/login", "{path}");
        assert!(!page.body.contains("Demo"), "{path} leaked content");
    }
    for path in ["/apps/demo/crashes/1/status", "/apps/demo/settings/keys", "/apps/demo/settings/keys/abc/revoke", "/logout"] {
        let page = b.post(path, &[("csrf", "x"), ("status", "resolved")]);
        assert_eq!(page.status, 303, "{path}");
        assert_eq!(page.location(), "/login", "{path}");
        // answered without reading the form, so the connection is not offered for reuse
        assert_eq!(page.headers.get("connection").map(Vec::as_slice), Some(&["close".to_string()][..]), "{path}");
    }
    // A forged or stale cookie does not help.
    b.cookies.insert("tracer_session".into(), "ff".repeat(32));
    assert_eq!(b.get("/apps/demo").status, 303);
    b.cookies.insert("tracer_session".into(), "not-a-token".into());
    assert_eq!(b.get("/apps/demo").status, 303);
    assert_eq!(server.count("SELECT count(*) FROM issues WHERE status = 'open'"), 2);
}

#[test]
fn login_logout_and_cookie_attributes() {
    let server = seeded();
    let mut b = Browser::new(&server);
    let login = b.login("admin", PASSWORD);
    assert_eq!(login.status, 303, "{}", login.body);
    assert_eq!(login.location(), "/");
    let cookie = login.headers["set-cookie"].iter().find(|c| c.starts_with("tracer_session=")).unwrap().clone();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Lax") && cookie.contains("Path=/"), "{cookie}");
    assert!(cookie.contains("Max-Age=1209600"));
    assert!(!cookie.contains("Secure"));
    // Only a hash of the token is stored.
    let token = b.cookies["tracer_session"].clone();
    assert_eq!(token.len(), 64);
    assert_eq!(server.count("SELECT count(*) FROM admin_sessions"), 1);
    assert_eq!(server.text("SELECT token_hash FROM admin_sessions"), sha256_hex(&token));
    assert!(server.count("SELECT expires_at - created_at FROM admin_sessions") >= 13 * DAY_MS);

    assert_eq!(b.get("/apps/demo").status, 200);
    let csrf = b.csrf();
    let out = b.post("/logout", &[("csrf", &csrf)]);
    assert_eq!(out.status, 303);
    assert_eq!(out.location(), "/login");
    assert_eq!(server.count("SELECT count(*) FROM admin_sessions"), 0);
    // Replaying the old cookie no longer works.
    b.cookies.insert("tracer_session".into(), token);
    let after = b.get("/apps/demo");
    assert_eq!(after.status, 303);
    assert_eq!(after.location(), "/login");
}

#[test]
fn secure_cookie_flag_follows_the_setting() {
    let server = TestServer::demo_with(|config| config.secure_cookies = true);
    ensure_admin(&server);
    let mut b = Browser::new(&server);
    let login = b.login("admin", PASSWORD);
    assert_eq!(login.status, 303);
    let cookies = &login.headers["set-cookie"];
    let session = cookies.iter().find(|c| c.starts_with("__Host-tracer_session=")).expect("a __Host- session cookie");
    assert!(session.contains("; Secure") && session.contains("Path=/") && !session.contains("Domain"), "{session}");
    assert!(cookies.iter().any(|c| c.starts_with("__Secure-tracer_login=") && c.contains("; Secure")));
}

#[test]
fn cookies_are_secure_by_default_and_plain_http_is_an_explicit_opt_out() {
    assert!(ashlar_trace_server::config::Config::default().secure_cookies);
    let server = TestServer::demo();
    ensure_admin(&server);
    let login = Browser::new(&server).login("admin", PASSWORD);
    let session = login.headers["set-cookie"].iter().find(|c| c.starts_with("tracer_session=")).expect("a plain session cookie").clone();
    assert!(!session.contains("Secure"), "{session}");
}

#[test]
fn a_wrong_password_looks_exactly_like_an_unknown_user() {
    let server = seeded();
    let mut b = Browser::new(&server);
    let wrong = b.login("admin", "not the password");
    let unknown = Browser::new(&server).login("nobody", "not the password");
    assert_eq!(wrong.status, 401);
    assert_eq!(unknown.status, 401);
    assert!(wrong.body.contains("Invalid username or password."));
    // Identical apart from the per-form token.
    let strip = |body: &str| {
        let at = body.find("name=\"csrf\" value=\"").unwrap() + "name=\"csrf\" value=\"".len();
        format!("{}{}", &body[..at], &body[at + 64..])
    };
    assert_eq!(strip(&wrong.body), strip(&unknown.body));
    assert!(!wrong.headers.contains_key("tracer_session") && !b.cookies.contains_key("tracer_session"));
    assert_eq!(server.count("SELECT count(*) FROM admin_sessions"), 0);
    // Oversized input is refused the same way, not crashed on.
    let long = "p".repeat(5000);
    assert_eq!(Browser::new(&server).login("admin", &long).status, 401);
}

#[test]
fn login_needs_its_form_token_and_is_rate_limited() {
    // The login limit is independent of the ingest limit: switching ingest limiting off leaves it on.
    let server = TestServer::demo_with(|config| config.rate_per_min = 0);
    ensure_admin(&server);
    let mut b = Browser::new(&server);
    // No token, wrong token, and no cookie are all refused before any password check.
    assert_eq!(b.post("/login", &[("username", "admin"), ("password", PASSWORD)]).status, 403);
    b.get("/login");
    assert_eq!(b.post("/login", &[("username", "admin"), ("password", PASSWORD), ("csrf", &"0".repeat(64))]).status, 403);
    assert_eq!(Browser::new(&server).post("/login", &[("username", "admin"), ("password", PASSWORD), ("csrf", "x")]).status, 403);
    assert_eq!(server.count("SELECT count(*) FROM admin_sessions"), 0);
    // After ten attempts from one address the limiter answers 429 with Retry-After.
    let mut limited = None;
    for _ in 0..14 {
        let reply = b.post("/login", &[("username", "admin"), ("password", "bad"), ("csrf", "x")]);
        if reply.status == 429 {
            limited = Some(reply);
            break;
        }
    }
    let limited = limited.expect("the login limiter never answered 429");
    assert!(limited.header("retry-after").parse::<u64>().unwrap() >= 1);
    // Even the right password is refused while limited.
    assert_eq!(b.login("admin", PASSWORD).status, 429);
}

#[test]
fn password_checks_wait_for_a_free_slot() {
    let server = TestServer::demo();
    ensure_admin(&server);
    let slots = server.state.login_slots.clone();
    let all = slots.clone().try_acquire_many_owned(ashlar_trace_server::http::LOGIN_CONCURRENCY as u32).unwrap();
    let base = server.base.clone();
    let started = std::time::Instant::now();
    let worker = std::thread::spawn(move || {
        let mut browser = Browser {
            base,
            agent: ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).max_redirects_will_error(false).build().into(),
            cookies: BTreeMap::new(),
        };
        browser.login("admin", PASSWORD).status
    });
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert!(!worker.is_finished(), "the password check ran without a free slot");
    drop(all);
    assert_eq!(worker.join().unwrap(), 303);
    assert!(started.elapsed() >= std::time::Duration::from_millis(700));
}

#[test]
fn repeated_failures_for_one_user_name_slow_every_attempt_the_same_way() {
    let server = seeded();
    // Five wrong guesses at one name; the extra wait starts after the third.
    for _ in 0..4 {
        assert_eq!(Browser::new(&server).login("admin", "wrong").status, 401);
    }
    let timed = |user: &str, password: &str| {
        let started = std::time::Instant::now();
        let status = Browser::new(&server).login(user, password).status;
        (status, started.elapsed())
    };
    let (status, wrong_for_real_user) = timed("admin", "wrong");
    assert_eq!(status, 401);
    assert!(wrong_for_real_user >= std::time::Duration::from_millis(500), "{wrong_for_real_user:?}");
    // A name that does not exist gets nothing different once it has the same history.
    for _ in 0..4 {
        Browser::new(&server).login("ghost", "wrong");
    }
    let (_, wrong_for_ghost) = timed("ghost", "wrong");
    assert!(wrong_for_ghost >= std::time::Duration::from_millis(500), "{wrong_for_ghost:?}");
}

#[test]
fn the_login_page_explains_how_to_create_the_first_admin() {
    let server = TestServer::demo();
    let page = Browser::new(&server).get("/login");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("ashlar-trace-server admin create"));
    assert!(!page.body.contains("type=\"password\""));
}

#[test]
fn state_changes_need_the_csrf_token() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let id = issue_id(&server, "while saving");
    let status = || server.text(&format!("SELECT status FROM issues WHERE id = {id}"));
    let path = format!("/apps/demo/crashes/{id}/status");
    assert_eq!(b.post(&path, &[("status", "resolved")]).status, 403);
    assert_eq!(b.post(&path, &[("status", "resolved"), ("csrf", "wrong")]).status, 403);
    assert_eq!(b.post(&path, &[("status", "resolved"), ("csrf", &"0".repeat(64))]).status, 403);
    assert_eq!(status(), "open");
    let keys = |sql: &str| server.count(sql);
    assert_eq!(b.post("/apps/demo/settings/keys", &[]).status, 403);
    assert_eq!(b.post("/apps/demo/settings/keys", &[("csrf", "nope")]).status, 403);
    assert_eq!(keys("SELECT count(*) FROM app_keys"), 1);
    assert_eq!(b.post("/logout", &[("csrf", "nope")]).status, 403);
    assert_eq!(b.get("/apps/demo").status, 200, "a refused logout leaves the session alone");
    // A token from another session is no good either.
    let other = Browser::signed_in(&server).csrf();
    assert_eq!(other, b.csrf(), "same session, same token");
    let foreign = sha256_hex("some other session");
    assert_eq!(b.post(&path, &[("status", "resolved"), ("csrf", &foreign)]).status, 403);
    // GET never changes anything.
    assert_eq!(b.get(&format!("{path}?status=resolved&csrf={other}")).status, 405);
    assert_eq!(status(), "open");
}

// ---------------------------------------------------------------------------------------------------------
// actions

#[test]
fn resolve_ignore_and_reopen_change_the_status() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let csrf = b.csrf();
    let id = issue_id(&server, "while saving");
    let other = issue_id(&server, "disk full");
    let status = |id: i64| server.text(&format!("SELECT status FROM issues WHERE id = {id}"));
    let path = format!("/apps/demo/crashes/{id}/status");

    let done = b.post(&path, &[("status", "resolved"), ("csrf", &csrf)]);
    assert_eq!((done.status, done.location()), (303, format!("/apps/demo/crashes/{id}").as_str()));
    assert_eq!(status(id), "resolved");
    let page = b.get(&format!("/apps/demo/crashes/{id}"));
    assert!(page.body.contains("badge resolved") && page.body.contains("Reopen"));
    assert!(b.get("/apps/demo/crashes?status=resolved").body.contains("while saving"));
    assert!(!b.get("/apps/demo").body.contains("while saving"), "resolved issues leave the newest-open list");

    assert_eq!(b.post(&path, &[("status", "ignored"), ("csrf", &csrf)]).status, 303);
    assert_eq!(status(id), "ignored");
    assert_eq!(b.post(&path, &[("status", "open"), ("csrf", &csrf)]).status, 303);
    assert_eq!(status(id), "open");
    assert_eq!(status(other), "open", "other issues are untouched");

    assert_eq!(b.post(&path, &[("status", "deleted"), ("csrf", &csrf)]).status, 400);
    assert_eq!(b.post(&path, &[("csrf", &csrf)]).status, 400);
    assert_eq!(b.post("/apps/demo/crashes/424242/status", &[("status", "open"), ("csrf", &csrf)]).status, 404);
    assert_eq!(status(id), "open");
}

#[test]
fn an_issue_of_another_app_cannot_be_changed() {
    let server = seeded();
    server.store.create_app("other", "Other", "tk_other_key", ashlar_trace_server::now_ms()).unwrap();
    let mut b = Browser::signed_in(&server);
    let csrf = b.csrf();
    let id = issue_id(&server, "while saving");
    assert_eq!(b.post(&format!("/apps/other/crashes/{id}/status"), &[("status", "resolved"), ("csrf", &csrf)]).status, 404);
    assert_eq!(b.get(&format!("/apps/other/crashes/{id}")).status, 404);
    assert_eq!(server.text(&format!("SELECT status FROM issues WHERE id = {id}")), "open");
}

#[test]
fn write_keys_can_be_created_and_revoked() {
    let server = seeded();
    let mut b = Browser::signed_in(&server);
    let csrf = b.csrf();
    let created = b.post("/apps/demo/settings/keys", &[("csrf", &csrf)]);
    assert_eq!(created.status, 200, "{}", created.body);
    let key = between(&created.body, "<p><code>", "</code>").to_string();
    assert!(key.starts_with("tk_") && key.len() > 30, "{key}");
    assert!(created.body.contains("New write key"));
    assert_eq!(server.count("SELECT count(*) FROM app_keys WHERE revoked_at IS NULL"), 2);

    // The new key works, and the page lists it masked from then on.
    let envelope = serde_json::to_vec(&common::envelope("i9", "s9", vec![])).unwrap();
    assert_eq!(server.post_raw("/v1/ingest", Some(&key), false, &envelope).status, 200);
    let listing = b.get("/apps/demo/settings");
    assert!(!listing.body.contains(&key) && !listing.body.contains("New write key"));

    // Revoke it through its id from the form action.
    let id = ashlar_trace_server::dashboard::key_id(&key);
    assert!(listing.body.contains(&format!("/apps/demo/settings/keys/{id}/revoke")));
    let revoked = b.post(&format!("/apps/demo/settings/keys/{id}/revoke"), &[("csrf", &csrf)]);
    assert_eq!(revoked.status, 303);
    assert_eq!(server.post_raw("/v1/ingest", Some(&key), false, &envelope).status, 401);
    assert!(b.get("/apps/demo/settings").body.contains("Revoked"));
    // Revoking again, or a made-up id, is a 404, and the original key still works.
    assert_eq!(b.post(&format!("/apps/demo/settings/keys/{id}/revoke"), &[("csrf", &csrf)]).status, 404);
    assert_eq!(b.post("/apps/demo/settings/keys/0000000000000000/revoke", &[("csrf", &csrf)]).status, 404);
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, &envelope).status, 200);

    // The seeded key can be revoked too.
    let original = ashlar_trace_server::dashboard::key_id(&server.key);
    assert_eq!(b.post(&format!("/apps/demo/settings/keys/{original}/revoke"), &[("csrf", &csrf)]).status, 303);
    assert_eq!(server.post_raw("/v1/ingest", Some(&server.key), false, &envelope).status, 401);
}

#[test]
fn a_key_of_another_app_cannot_be_revoked_here() {
    let server = seeded();
    server.store.create_app("other", "Other", "tk_other_key", ashlar_trace_server::now_ms()).unwrap();
    let mut b = Browser::signed_in(&server);
    let csrf = b.csrf();
    let foreign = ashlar_trace_server::dashboard::key_id("tk_other_key");
    assert_eq!(b.post(&format!("/apps/demo/settings/keys/{foreign}/revoke"), &[("csrf", &csrf)]).status, 404);
    assert!(server.store.lookup_key("tk_other_key").unwrap().is_some());
}

#[test]
fn hostile_text_is_escaped_on_every_page() {
    // Version, event name, OS and crash text all come from clients.
    let server = seeded();
    let evil = "<img src=x onerror=alert(1)>";
    let mut env = envelope("i7", "s7", evil, evil, vec![]);
    env.items = vec![start("s7-start", false), event("s7-e", 10, "file.open"), end("s7-end", 200, SessionStatus::Exited, 5, 0, 0)];
    assert_eq!(server.ingest(&env).status, 200);
    let today = ashlar_trace_server::now_ms() / DAY_MS;
    server.store.rollup(today - 1, today).unwrap();
    let mut b = Browser::signed_in(&server);
    for path in ["/apps/demo", "/apps/demo/usage", "/apps/demo/performance", "/apps/demo/reports", "/apps/demo/crashes"] {
        let page = b.get(path);
        assert_eq!(page.status, 200, "{path}");
        assert!(!page.body.contains("<img src=x"), "{path}");
    }
    let path = format!("/apps/demo/reports?version={}", enc(evil));
    let report = b.get(&path);
    assert_eq!(report.status, 200, "{}", report.body);
    assert!(!report.body.contains("<img src=x"));
    for (format, ext) in [("html", "html"), ("md", "md"), ("json", "json")] {
        let download = b.get(&format!("{path}&format={format}"));
        assert_eq!(download.status, 200);
        assert!(!download.body.contains("<img"), "{format}");
        let disposition = download.header("content-disposition");
        assert!(disposition.ends_with(&format!(".{ext}\"")), "{disposition}");
        assert!(disposition.bytes().all(|byte| byte.is_ascii_graphic() || byte == b' '), "{disposition}");
        assert!(!disposition.contains('<') && !disposition.contains('/'), "{disposition}");
    }
}

#[test]
fn the_issue_page_shows_every_detail_a_crash_carries() {
    use ashlar_trace_proto::{Crash, CrashKind, Device, FrameStats, Gpu, Item, ItemBody, Perf};
    let server = TestServer::demo();
    let t = common::now();
    let mut frames = FrameStats::empty();
    for us in [4_000u32, 9_000, 20_000, 900_000] {
        frames.record(us);
    }
    let crash = Crash {
        kind: CrashKind::Hang,
        fatal: false,
        message: Some("main loop stopped".into()),
        location: Some("src/ui.rs:10:5".into()),
        thread: Some("main".into()),
        backtrace: Some("0: demo::ui::run\n1: demo::main".into()),
        blocked_ms: Some(12_500),
        exit_code: Some(3),
        signal: Some(11),
        context: Some("save".into()),
        perf: Some(Perf { window_s: 5, frames: Some(frames), cpu_pct: Some(37.5), rss_mb: Some(512), rss_peak_mb: Some(640), ..Perf::default() }),
        ..Crash::default()
    };
    let mut envelope = common::envelope("i1", "s1", vec![Item { id: "c1".into(), ts: t - 1000, body: ItemBody::Crash(crash) }]);
    envelope.app.build = Some("abc1234".into());
    envelope.device = Some(Device {
        os_name: "macos".into(),
        os_version: Some("26.0".into()),
        arch: "aarch64".into(),
        cpu_brand: Some("Example CPU".into()),
        cpu_cores: Some(10),
        ram_mb: Some(32768),
        gpu: Some(Gpu { name: "Example GPU".into(), backend: Some("metal".into()), ..Gpu::default() }),
        locale: Some("en-US".into()),
    });
    assert_eq!(server.ingest(&envelope).status, 200);
    let mut browser = Browser::signed_in(&server);
    let page = browser.get("/apps/demo/crashes/1");
    assert_eq!(page.status, 200, "{}", page.body);
    for expected in [
        "Example CPU (10 cores)",
        "32,768 MB",
        "Example GPU via metal",
        "en-US",
        "Blocked for",
        "12,500 ms",
        "Exit code",
        "Signal",
        "Context",
        "Build",
        "abc1234",
        "Frame time p99",
        "Slow frames",
        "37.5%",
        "Peak memory",
        "640 MB",
    ] {
        assert!(page.body.contains(expected), "the issue page lacks {expected:?}");
    }
    // Resolved and ignored issues offer the other transitions.
    let csrf = browser.csrf();
    browser.post("/apps/demo/crashes/1/status", &[("status", "ignored"), ("csrf", &csrf)]);
    let ignored = browser.get("/apps/demo/crashes/1");
    assert!(ignored.body.contains("Reopen"));
    browser.post("/apps/demo/crashes/1/status", &[("status", "resolved"), ("csrf", &csrf)]);
    let resolved = browser.get("/apps/demo/crashes/1");
    assert!(resolved.body.contains("Reopen") && resolved.body.contains("Ignore"));
}
