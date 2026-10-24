#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The MCP client, the server supervisor and the whole install → run → call → remove path, against the
//! fake server binary this crate builds (feature `fake-server`).

use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ashlar_integrations::mcp::{McpClient, McpError, ServerCommand};
use ashlar_integrations::server::{Server, ServerConfig};
use ashlar_integrations::{AcceptAll, Codesign, Config, Entry, Http, HttpResponse, Integrations, ServerStatus, Snapshot};
use serde_json::json;
use sha2::{Digest, Sha256};

const FAKE: &str = env!("CARGO_BIN_EXE_ashlar-integrations-fake-server");

fn command(args: &[&str], env: &[(&str, &str)]) -> ServerCommand {
    ServerCommand {
        program: PathBuf::from(FAKE),
        args: args.iter().map(|s| s.to_string()).collect(),
        env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        cwd: None,
        label: "fake".into(),
    }
}

const T: Duration = Duration::from_secs(10);

#[test]
fn the_client_talks_to_a_server() {
    let client = McpClient::start(&command(&[], &[("MARK", "yes")]), "tests", "1", T).unwrap();
    assert_eq!(client.info().name, "fake");
    assert_eq!(client.info().instructions.as_deref(), Some("a fake"));
    let tools = client.list_tools(T).unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["echo", "env", "slow", "crash", "write", "secret"]); // both pages
    let out = client.call_tool("echo", &json!({"text": "hi"}), T, None).unwrap();
    assert_eq!((out.text().as_str(), out.is_error), ("hi", false));
    // The child gets only the environment it is given.
    assert_eq!(client.call_tool("env", &json!({"name": "MARK"}), T, None).unwrap().text(), "yes");
    assert_eq!(client.call_tool("env", &json!({"name": "CARGO"}), T, None).unwrap().text(), "<unset>");
    // A tool's own failure is a result, not a transport error.
    assert!(client.call_tool("nope", &json!({}), T, None).unwrap().is_error);
}

#[test]
fn timeouts_and_cancels_leave_the_client_usable() {
    let client = McpClient::start(&command(&[], &[]), "tests", "1", T).unwrap();
    let err = client.call_tool("slow", &json!({"ms": 600}), Duration::from_millis(100), None).unwrap_err();
    assert!(matches!(err, McpError::Timeout { .. }), "{err}");
    let cancel = AtomicBool::new(true);
    assert_eq!(client.call_tool("slow", &json!({"ms": 600}), T, Some(&cancel)).unwrap_err(), McpError::Cancelled);
    // The late answers are dropped; the next call gets its own.
    assert_eq!(client.call_tool("echo", &json!({"text": "after"}), T, None).unwrap().text(), "after");
}

#[test]
fn a_crash_fails_the_call_and_bad_servers_are_refused() {
    let client = McpClient::start(&command(&[], &[]), "tests", "1", T).unwrap();
    let err = client.call_tool("crash", &json!({}), T, None).unwrap_err();
    assert!(err.is_exit(), "{err}");
    let deadline = Instant::now() + T;
    while client.is_alive() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!client.is_alive());

    assert!(matches!(McpClient::start(&command(&["--fail-init"], &[]), "t", "1", T).unwrap_err(), McpError::Rpc { .. }));
    assert!(matches!(McpClient::start(&command(&["--bad-version"], &[]), "t", "1", T).unwrap_err(), McpError::Protocol { .. }));
    let err = McpClient::start(&command(&["--silent"], &[]), "t", "1", Duration::from_millis(300)).unwrap_err();
    assert!(matches!(err, McpError::Timeout { .. }), "{err}");
    let missing = ServerCommand { program: "/nonexistent/server".into(), ..command(&[], &[]) };
    assert!(matches!(McpClient::start(&missing, "t", "1", T).unwrap_err(), McpError::Spawn(_)));
}

#[test]
fn dropping_the_client_stops_the_server() {
    let client = McpClient::start(&command(&[], &[]), "tests", "1", T).unwrap();
    let pid = client.pid().unwrap();
    drop(client);
    let alive = Command::new("/bin/kill").args(["-0", &pid.to_string()]).status().unwrap().success();
    assert!(!alive, "the server outlived its client");
}

fn server_config(starts: &Path) -> ServerConfig {
    ServerConfig {
        id: "fake".into(),
        command: command(&[], &[("FAKE_STARTS", starts.to_str().unwrap())]),
        startup_timeout: T,
        call_timeout: T,
        hidden: vec!["secret".into()],
        client_name: "tests".into(),
        client_version: "1".into(),
    }
}

fn starts(path: &Path) -> usize {
    fs::read_to_string(path).map(|s| s.lines().count()).unwrap_or(0)
}

#[test]
fn the_supervisor_restarts_after_a_crash_but_not_forever() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("starts");
    let server = Server::new(server_config(&log));
    assert_eq!(server.status(), ServerStatus::Stopped);
    server.start().unwrap();
    assert!(matches!(server.status(), ServerStatus::Running { tools: 5, .. }));
    assert!(!server.tools().iter().any(|t| t.name == "secret"));
    assert!(server.call("secret", &json!({}), None).is_error);

    for round in 1..=2 {
        let out = server.call("crash", &json!({}), None);
        assert!(out.is_error && out.text().contains("restarts"), "{}", out.text());
        assert_eq!(server.call("echo", &json!({"text": "back"}), None).text(), "back", "round {round}");
    }
    assert_eq!(starts(&log), 3);
    // The third crash in the window: no more automatic restarts.
    let out = server.call("crash", &json!({}), None);
    assert!(out.is_error && !out.text().contains("restarts"), "{}", out.text());
    assert!(server.call("echo", &json!({"text": "x"}), None).is_error);
    assert!(matches!(server.status(), ServerStatus::Error { .. }));
    // By hand it comes back.
    server.restart().unwrap();
    assert_eq!(server.call("echo", &json!({"text": "again"}), None).text(), "again");
    server.stop();
    assert_eq!(server.status(), ServerStatus::Stopped);
}

/// Serves byte buffers by URL, honouring Range.
#[derive(Default)]
struct MemHttp {
    files: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemHttp {
    fn put(&self, url: &str, data: Vec<u8>) {
        self.files.lock().unwrap().insert(url.to_string(), data);
    }
}

impl Http for MemHttp {
    fn get(&self, url: &str, from: u64) -> Result<HttpResponse, String> {
        let files = self.files.lock().unwrap();
        let Some(data) = files.get(url) else {
            return Ok(HttpResponse { status: 404, body: Box::new(Cursor::new(Vec::new())) });
        };
        let (status, body) = if from > 0 { (206, data[from as usize..].to_vec()) } else { (200, data.clone()) };
        Ok(HttpResponse { status, body: Box::new(Cursor::new(body)) })
    }
}

fn sha(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

const ASSET: &[u8] = b"pretend these are model weights";

/// A bundle zip: `fake/fake` (the fake server) and `fake/integration.json`.
fn bundle_zip(dir: &Path, version: u32) -> Vec<u8> {
    let src = dir.join(format!("src-{version}"));
    let folder = src.join("fake");
    fs::create_dir_all(&folder).unwrap();
    fs::copy(FAKE, folder.join("fake")).unwrap();
    let manifest = json!({
        "id": "fake", "version": format!("1.0-{version}"),
        "env": {"FAKE_ASSET": "${data}/models/weights.bin"},
        "hidden_tools": ["secret"],
        "assets": [{"path": "models/weights.bin", "size": ASSET.len(), "sha256": sha(ASSET),
                    "urls": ["https://cdn.example.com/missing", "https://hub.example.com/weights.bin"]}]
    });
    fs::write(folder.join("integration.json"), manifest.to_string()).unwrap();
    let zip = dir.join(format!("fake-{version}.zip"));
    let ok = Command::new("/usr/bin/ditto").args(["-c", "-k", "--keepParent"]).arg(&folder).arg(&zip).status().unwrap().success();
    assert!(ok);
    fs::read(zip).unwrap()
}

fn entry(version: u32, zip: &[u8]) -> Entry {
    serde_json::from_value(json!({
        "id": "fake", "title": "Fake", "summary": "A fake integration", "version": version, "platform": "macos", "arch": "arm64",
        "url": format!("https://downloads.example.com/fake-{version}.zip"), "sha256": sha(zip), "size": zip.len(),
        "entry": "fake/fake", "args": [], "min_app_version": "0.1.0", "licence": "MIT", "models_size": ASSET.len()
    }))
    .unwrap()
}

fn config(root: &Path) -> Config {
    Config {
        root: root.to_path_buf(),
        catalog_url: None,
        platform: "macos".into(),
        arch: "arm64".into(),
        app_name: "tests".into(),
        app_version: "1.0.0".into(),
        team_id: None,
    }
}

fn wait(ix: &Integrations, what: &str, f: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let s = ix.snapshot();
        if f(&s) {
            return s;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {s:#?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn running(s: &Snapshot) -> bool {
    s.items.first().is_some_and(|i| i.job.is_none() && matches!(i.server, ServerStatus::Running { .. }))
}

#[cfg(target_os = "macos")]
#[test]
fn install_run_call_update_rollback_remove() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("integrations");
    let http = Arc::new(MemHttp::default());
    let zip1 = bundle_zip(dir.path(), 1);
    http.put("https://downloads.example.com/fake-1.zip", zip1.clone());
    http.put("https://hub.example.com/weights.bin", ASSET.to_vec());
    let wakes = Arc::new(Mutex::new(0usize));
    let w = Arc::clone(&wakes);
    let ix = Integrations::new(config(&root), http.clone(), Arc::new(AcceptAll), move || *w.lock().unwrap() += 1);
    ix.set_catalog(vec![entry(1, &zip1)]);
    let s = ix.snapshot();
    let item = &s.items[0];
    assert_eq!((item.installed, item.available, item.needs_assets), (None, Some(1), true));

    ix.install("fake").unwrap();
    assert!(ix.install("fake").is_err(), "a second job on the same integration is refused");
    let s = wait(&ix, "running", running);
    let item = &s.items[0];
    assert_eq!((item.installed, item.enabled, item.needs_assets), (Some(1), true, false));
    assert_eq!(item.installed_label.as_deref(), Some("1.0-1"));
    assert!(*wakes.lock().unwrap() > 0);
    assert_eq!(fs::read(root.join("fake/data/models/weights.bin")).unwrap(), ASSET);
    assert!(!root.join("downloads/fake-1.zip").exists(), "the zip is deleted once unpacked");

    // Tools, prefixed and sorted, without the hidden one.
    let names: Vec<String> = ix.tools().into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["fake__crash", "fake__echo", "fake__env", "fake__slow", "fake__write"]);
    assert_eq!(ix.call("fake__echo", &json!({"text": "hello"}), None).text(), "hello");
    let asset = ix.call("fake__env", &json!({"name": "FAKE_ASSET"}), None).text();
    assert_eq!(asset, root.join("fake/data/models/weights.bin").to_string_lossy());
    assert!(ix.needs_approval("fake__echo").is_none());
    assert!(ix.needs_approval("fake__write").unwrap().contains("files"));
    assert!(ix.call("fake__secret", &json!({}), None).is_error);
    assert!(ix.call("other__echo", &json!({}), None).is_error);
    assert!(ix.owns("fake__echo") && !ix.owns("echo"));

    // Disabled: no tools, server stopped; enabled again: back.
    ix.set_enabled("fake", false).unwrap();
    assert!(ix.tools().is_empty());
    assert_eq!(ix.snapshot().items[0].server, ServerStatus::Stopped);
    ix.set_enabled("fake", true).unwrap();
    wait(&ix, "running again", running);

    // A newer build: the old one is kept as previous, the models aren't downloaded again.
    let zip2 = bundle_zip(dir.path(), 2);
    http.put("https://downloads.example.com/fake-2.zip", zip2.clone());
    http.files.lock().unwrap().remove("https://hub.example.com/weights.bin");
    ix.set_catalog(vec![entry(2, &zip2)]);
    assert!(ix.snapshot().items[0].update_available);
    ix.install("fake").unwrap();
    let s = wait(&ix, "updated", |s| running(s) && s.items[0].installed == Some(2));
    assert_eq!((s.items[0].previous, s.items[0].update_available), (Some(1), false));
    assert!(root.join("fake/1").is_dir() && root.join("fake/2").is_dir());

    assert_eq!(ix.rollback("fake").unwrap(), 1);
    let s = wait(&ix, "rolled back", |s| running(s) && s.items[0].installed == Some(1));
    assert_eq!(s.items[0].previous, None);
    assert!(!root.join("fake/2").exists());

    // A fresh manager finds it installed and starts it.
    ix.shutdown();
    let ix2 = Integrations::new(config(&root), http.clone(), Arc::new(AcceptAll), || {});
    wait(&ix2, "restarted from disk", running);
    ix2.remove("fake").unwrap();
    wait(&ix2, "removed", |s| s.items.is_empty());
    assert!(!root.join("fake").exists());
    ix2.shutdown();
}

#[cfg(target_os = "macos")]
#[test]
fn a_bundle_signed_by_someone_else_is_never_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("integrations");
    let http = Arc::new(MemHttp::default());
    let zip = bundle_zip(dir.path(), 1);
    http.put("https://downloads.example.com/fake-1.zip", zip.clone());
    let starts = dir.path().join("starts");
    let ix = Integrations::new(config(&root), http, Arc::new(Codesign::new("ABCDE12345")), || {});
    ix.set_catalog(vec![entry(1, &zip)]);
    ix.install("fake").unwrap();
    let s = wait(&ix, "the failure", |s| s.items[0].job.is_none() && s.items[0].last_error.is_some());
    let item = &s.items[0];
    assert!(item.last_error.as_deref().unwrap().contains("signature"), "{item:#?}");
    assert_eq!(item.installed, None);
    assert!(!root.join("fake/1").exists() && !root.join("fake/1.tmp").exists());
    assert_eq!(starts_count(&starts), 0);
}

fn starts_count(p: &Path) -> usize {
    starts(p)
}

#[cfg(target_os = "macos")]
#[test]
fn a_corrupt_download_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("integrations");
    let http = Arc::new(MemHttp::default());
    let zip = bundle_zip(dir.path(), 1);
    let mut bad = zip.clone();
    bad[10] ^= 0xff;
    http.put("https://downloads.example.com/fake-1.zip", bad);
    let ix = Integrations::new(config(&root), http, Arc::new(AcceptAll), || {});
    ix.set_catalog(vec![entry(1, &zip)]);
    ix.install("fake").unwrap();
    let s = wait(&ix, "the failure", |s| s.items[0].job.is_none() && s.items[0].last_error.is_some());
    assert!(s.items[0].last_error.as_deref().unwrap().contains("sha256"), "{:?}", s.items[0].last_error);
    assert_eq!(s.items[0].installed, None);
}
