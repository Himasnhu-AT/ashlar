#![allow(dead_code)]
//! Shared test harness: a real server on an ephemeral port with a temp data directory.

use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;

use ashlar_trace_proto::{AppInfo, Envelope, Item, ItemBody, ProcessKind};
use ashlar_trace_server::config::Config;
use ashlar_trace_server::http::{self, AppState};
use ashlar_trace_server::store::{SqliteStore, Store};

pub struct Reply {
    pub status: u16,
    pub body: String,
    pub retry_after: Option<String>,
    /// The `Connection` header, lowercase.
    pub connection: Option<String>,
}

impl Reply {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }

    /// The `accepted`, `duplicates`, `rejected` counts and `need_catalog` flag of an ingest reply.
    pub fn counts(&self) -> (u64, u64, u64, bool) {
        let value = self.json();
        (
            value["accepted"].as_u64().unwrap(),
            value["duplicates"].as_u64().unwrap(),
            value["rejected"].as_u64().unwrap(),
            value["need_catalog"].as_bool().unwrap(),
        )
    }
}

pub struct TestServer {
    pub base: String,
    pub db_path: std::path::PathBuf,
    pub store: Arc<SqliteStore>,
    pub key: String,
    pub app_id: i64,
    pub state: AppState,
    agent: ureq::Agent,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    _dir: Option<tempfile::TempDir>,
}

pub fn config_in(dir: &Path) -> Config {
    Config { data_dir: dir.to_path_buf(), rate_per_min: 0, key_rate_per_min: 0, secure_cookies: false, ..Config::default() }
}

impl TestServer {
    /// A server with one app named "demo" and its write key, rate limiting off.
    pub fn demo() -> Self {
        Self::demo_with(|_| {})
    }

    pub fn demo_with(adjust: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        adjust(&mut config);
        let mut server = Self::start(config);
        server.key = "tk_test_key_demo".to_string();
        let app = server.store.create_app("demo", "Demo", &server.key, ashlar_trace_server::now_ms()).unwrap();
        server.app_id = app.id;
        server._dir = Some(dir);
        server
    }

    /// Starts a server on `config.data_dir`. No app is created.
    pub fn start(config: Config) -> Self {
        Self::start_with_limits(config, http::ServerLimits::default())
    }

    pub fn start_with_limits(config: Config, limits: http::ServerLimits) -> Self {
        let store = Arc::new(SqliteStore::open(&config.db_path()).unwrap());
        let db_path = config.db_path();
        let state = AppState::new(store.clone() as Arc<dyn Store>, config);
        let served = state.clone();
        let (addr_tx, addr_rx) = std::sync::mpsc::channel::<SocketAddr>();
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                addr_tx.send(listener.local_addr().unwrap()).unwrap();
                http::serve_with(listener, http::router(served), limits, async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
            });
        });
        let addr = addr_rx.recv().unwrap();
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        Self {
            base: format!("http://{addr}"),
            db_path,
            store,
            key: String::new(),
            app_id: 0,
            state,
            agent,
            shutdown: Some(shutdown),
            thread: Some(thread),
            _dir: None,
        }
    }

    pub fn post_raw(&self, path: &str, key: Option<&str>, gzip: bool, body: &[u8]) -> Reply {
        self.post_with(path, key, gzip, body, &[])
    }

    /// Like [`TestServer::post_raw`] with extra request headers (each appended as its own header line).
    pub fn post_with(&self, path: &str, key: Option<&str>, gzip: bool, body: &[u8], headers: &[(&str, &str)]) -> Reply {
        let mut request = self.agent.post(format!("{}{path}", self.base));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(key) = key {
            request = request.header("x-tracer-key", key);
        }
        if gzip {
            request = request.header("content-encoding", "gzip");
        }
        let mut response = request.send(body).unwrap();
        Reply {
            status: response.status().as_u16(),
            retry_after: response.headers().get("retry-after").and_then(|value| value.to_str().ok()).map(str::to_string),
            connection: response.headers().get("connection").and_then(|value| value.to_str().ok()).map(str::to_ascii_lowercase),
            body: response.body_mut().read_to_string().unwrap_or_default(),
        }
    }

    pub fn get(&self, path: &str) -> Reply {
        let mut response = self.agent.get(format!("{}{path}", self.base)).call().unwrap();
        Reply {
            status: response.status().as_u16(),
            retry_after: None,
            connection: None,
            body: response.body_mut().read_to_string().unwrap_or_default(),
        }
    }

    /// Posts an envelope with the server's key, gzip-compressed like the SDK does.
    pub fn ingest(&self, envelope: &Envelope) -> Reply {
        self.ingest_json(&serde_json::to_vec(envelope).unwrap())
    }

    pub fn ingest_json(&self, json: &[u8]) -> Reply {
        self.post_raw("/v1/ingest", Some(&self.key), true, &gzip(json))
    }

    pub fn forget(&self, install_id: &str) -> Reply {
        let body = serde_json::to_vec(&ashlar_trace_proto::ForgetRequest { schema: 1, install_id: install_id.to_string() }).unwrap();
        self.post_raw("/v1/forget", Some(&self.key), false, &body)
    }

    /// Runs a read-only query against the database file.
    pub fn count(&self, sql: &str) -> i64 {
        let conn = rusqlite::Connection::open(&self.db_path).unwrap();
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    pub fn text(&self, sql: &str) -> String {
        let conn = rusqlite::Connection::open(&self.db_path).unwrap();
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

pub fn now() -> u64 {
    ashlar_trace_server::now_ms() as u64
}

pub fn envelope(install: &str, session: &str, items: Vec<Item>) -> Envelope {
    Envelope {
        schema: 1,
        app: AppInfo { name: "demo".into(), version: "1.0.0".into(), build: None, process: ProcessKind::Gui },
        install_id: install.into(),
        session_id: session.into(),
        device: None,
        sent_at: now(),
        items,
    }
}

pub fn item(id: &str, body: ItemBody) -> Item {
    Item { id: id.into(), ts: now() - 1000, body }
}
