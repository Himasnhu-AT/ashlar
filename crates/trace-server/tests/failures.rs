//! Failure injection: a locked, unwritable or half-migrated database, a newer schema, and killing the real
//! server while it is being loaded.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]

mod common;

use std::collections::HashSet;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{TestServer, envelope, item};
use tracer_proto::{Event, ItemBody};
use tracer_server::MIGRATIONS;
use tracer_server::store::{SqliteStore, Store};

fn event(id: &str) -> tracer_proto::Item {
    item(id, ItemBody::Event(Event { name: "file.open".into(), category: "file".into(), ..Event::default() }))
}

#[test]
fn a_database_locked_by_another_process_gives_503_with_retry_after_and_recovers() {
    let server = TestServer::demo();
    assert_eq!(server.ingest(&envelope("i", "s", vec![event("before")])).status, 200);
    // Another connection takes the write lock and keeps it past the server's 5 s busy timeout.
    let blocker = rusqlite::Connection::open(&server.db_path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let started = Instant::now();
    let reply = server.ingest(&envelope("i", "s", vec![event("during")]));
    assert!(started.elapsed() >= Duration::from_secs(4), "it should have waited for the lock");
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(reply.retry_after.as_deref(), Some("1"));
    // Reads keep working while writes are blocked.
    assert_eq!(server.get("/v1/health").status, 200);
    blocker.execute_batch("ROLLBACK;").unwrap();
    // The rejected envelope was not stored, and the same envelope now goes through (the client retries it).
    assert_eq!(server.count("SELECT count(*) FROM events"), 1);
    assert_eq!(server.ingest(&envelope("i", "s", vec![event("during")])).status, 200);
    assert_eq!(server.count("SELECT count(*) FROM events"), 2);
}

#[test]
fn opening_a_file_that_is_not_a_database_is_a_clean_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tracer.db");
    std::fs::write(
        &path,
        b"this is definitely not a sqlite database, just text padded out to be long enough ........................................",
    )
    .unwrap();
    let error = SqliteStore::open(&path).err().expect("must refuse").to_string();
    assert!(error.contains("not a database") || error.contains("malformed"), "{error}");
}

#[test]
#[cfg(unix)]
fn an_unwritable_database_is_refused_or_fails_writes_without_panicking() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tracer.db");
    drop(SqliteStore::open(&path).unwrap());
    for name in ["tracer.db", "tracer.db-wal", "tracer.db-shm"] {
        let file = dir.path().join(name);
        if file.exists() {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        }
    }
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let outcome = SqliteStore::open(&path);
    let refused_or_failing = match &outcome {
        Err(_) => true,
        Ok(store) => store.create_app("x", "X", "tk_unwritable_key", 1).is_err(),
    };
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    // A user that ignores file permissions (root) cannot show this; everywhere else it must hold.
    assert!(refused_or_failing || std::env::var("USER").is_ok_and(|user| user == "root"));
}

#[test]
fn a_migration_that_fails_part_way_leaves_the_database_untouched_and_usable_after_the_cause_is_fixed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tracer.db");
    // A database at version 3 where something is already sitting in the way of migration 4.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        for (_, sql) in &MIGRATIONS[..3] {
            conn.execute_batch(sql).unwrap();
        }
        conn.execute_batch("PRAGMA user_version = 3; CREATE TABLE dirty_days (in_the_way TEXT);").unwrap();
    }
    let error = SqliteStore::open(&path).err().expect("migration 4 must fail").to_string();
    assert!(error.contains("dirty_days") || error.contains("already exists"), "{error}");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 3, "the version must not advance past a failed migration");
        let leftovers: i64 =
            conn.query_row("SELECT count(*) FROM sqlite_master WHERE name IN ('daily_active', 'sessions_by_version')", [], |r| r.get(0)).unwrap();
        assert_eq!(leftovers, 0, "the half-applied migration must have been rolled back");
        conn.execute_batch("DROP TABLE dirty_days;").unwrap();
    }
    let store = SqliteStore::open(&path).unwrap();
    store.create_app("demo", "Demo", "tk_after_repair", 1).unwrap();
}

#[test]
fn a_database_from_a_newer_server_is_refused_with_a_clear_message() {
    let dir = tempfile::tempdir().unwrap();
    let config = common::config_in(dir.path());
    drop(SqliteStore::open(&config.db_path()).unwrap());
    rusqlite::Connection::open(config.db_path()).unwrap().execute_batch("PRAGMA user_version = 99;").unwrap();
    let mut out = Vec::new();
    let error = tracer_server::cli::execute(tracer_server::cli::Command::AppList, &config, &mut out, &mut std::io::empty()).unwrap_err().to_string();
    assert!(error.contains("newer"), "{error}");
}

// ---------------------------------------------------------------------------------------------------------
// killing the real server under load

struct Running {
    child: Child,
    port: u16,
}

fn binary() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tracer-server"))
}

fn start(dir: &std::path::Path, port: u16) -> Running {
    let child = binary()
        .env("TRACER_DATA_DIR", dir)
        .env("TRACER_BIND", format!("127.0.0.1:{port}"))
        .env("TRACER_RATE_PER_MIN", "0")
        .env("TRACER_KEY_RATE_PER_MIN", "0")
        .arg("serve")
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let running = Running { child, port };
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let started = Instant::now();
    while !agent.get(format!("http://127.0.0.1:{port}/v1/health")).call().is_ok_and(|r| r.status() == 200) {
        assert!(started.elapsed() < Duration::from_secs(20), "the server did not come up");
        std::thread::sleep(Duration::from_millis(50));
    }
    running
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn stored_ids(dir: &std::path::Path) -> HashSet<String> {
    let conn = rusqlite::Connection::open(dir.join("tracer.db")).unwrap();
    let mut stmt = conn.prepare("SELECT item_id FROM items_seen").unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0)).unwrap().map(Result::unwrap).collect()
}

/// Posts unique items from several threads until the server goes away; returns the ids it acknowledged.
fn hammer(port: u16, key: &str, stop_after: Duration, kill: impl FnOnce() + Send) -> HashSet<String> {
    let acked = Arc::new(Mutex::new(HashSet::new()));
    let counter = Arc::new(AtomicU64::new(0));
    let dead = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let (acked, counter, dead) = (acked.clone(), counter.clone(), dead.clone());
            scope.spawn(move || {
                let agent: ureq::Agent =
                    ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(5))).build().into();
                while !dead.load(Ordering::Relaxed) {
                    let n = counter.fetch_add(1, Ordering::Relaxed);
                    let ids: Vec<String> = (0..10).map(|i| format!("kill-{n}-{i}")).collect();
                    let items = ids.iter().map(|id| event(id)).collect();
                    let body = serde_json::to_vec(&envelope("i", &format!("s{}", n % 16), items)).unwrap();
                    let result = agent.post(format!("http://127.0.0.1:{port}/v1/ingest")).header("x-tracer-key", key).send(&body[..]);
                    match result {
                        Ok(response) if response.status() == 200 => acked.lock().unwrap().extend(ids),
                        Ok(_) => {}
                        Err(_) => {
                            dead.store(true, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
        std::thread::sleep(stop_after);
        kill();
        std::thread::sleep(Duration::from_millis(1500));
        dead.store(true, Ordering::Relaxed);
    });
    Arc::try_unwrap(acked).unwrap().into_inner().unwrap()
}

#[test]
fn killing_the_server_under_load_loses_no_acknowledged_item() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let created = binary().env("TRACER_DATA_DIR", &data).args(["app", "create", "demo", "Demo"]).output().unwrap();
    let key = String::from_utf8_lossy(&created.stdout).lines().find_map(|l| l.strip_prefix("write key: ").map(str::to_string)).unwrap();

    for signal in ["TERM", "KILL"] {
        let port = free_port();
        let mut server = start(&data, port);
        let pid = server.child.id().to_string();
        let acked = hammer(port, &key, Duration::from_millis(1500), || {
            assert!(Command::new("kill").args([&format!("-{signal}"), &pid]).status().unwrap().success());
        });
        // SIGTERM is a graceful stop: the process exits by itself, successfully. SIGKILL cannot be handled.
        let started = Instant::now();
        let status = loop {
            if let Some(status) = server.child.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < Duration::from_secs(15), "the server did not exit after SIG{signal}");
            std::thread::sleep(Duration::from_millis(50));
        };
        if signal == "TERM" {
            assert!(status.success(), "SIGTERM should be a clean shutdown, got {status:?}");
        }
        assert!(acked.len() > 100, "SIG{signal}: only {} items were acknowledged before the kill", acked.len());
        let stored = stored_ids(&data);
        let missing: Vec<&String> = acked.iter().filter(|id| !stored.contains(*id)).collect();
        assert!(missing.is_empty(), "SIG{signal}: {} acknowledged items are missing, for example {:?}", missing.len(), missing.first());
        // The restarted server finds a usable database and takes new data.
        let again = start(&data, free_port());
        let reply = ureq::post(format!("http://127.0.0.1:{}/v1/ingest", again.port))
            .config()
            .http_status_as_error(false)
            .build()
            .header("x-tracer-key", &key)
            .send(&serde_json::to_vec(&envelope("i", "after", vec![event(&format!("after-{signal}"))])).unwrap()[..])
            .unwrap();
        assert_eq!(reply.status(), 200);
        let mut again = again;
        let _ = again.child.kill();
        let _ = again.child.wait();
    }
}
