#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The control server over real loopback sockets: hostile input, keys, limits, and a client round
//! trip on an ephemeral port.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use ashlar_command::{CommandError, CommandInfo, Dispatch};
use ashlar_control::{AppInfo, Client, Config, ControlError, NoUi, Request, Server, handle};
use serde_json::{Value, json};

/// A counter app: `count.add {by?}`, `count.get`, `count.fail`.
#[derive(Default)]
struct Counter(i64);

impl Dispatch for Counter {
    fn commands(&self) -> Vec<CommandInfo> {
        ["count.add", "count.get", "count.fail"]
            .iter()
            .map(|id| CommandInfo {
                id: id.to_string(),
                label: id.to_string(),
                menu: vec![],
                shortcut: None,
                params_schema: Value::Null,
                params_text: None,
                enabled: true,
                disabled_reason: None,
                mutates: *id == "count.add",
            })
            .collect()
    }
    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        match id {
            "count.add" => {
                self.0 += params.get("by").and_then(Value::as_i64).unwrap_or(1);
                Ok(json!(self.0))
            }
            "count.get" => Ok(json!(self.0)),
            "count.fail" => Err(CommandError::Disabled { id: id.into(), reason: "never".into() }),
            _ => Err(CommandError::Unknown { id: id.into() }),
        }
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ashlar-control-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The app thread: answers every request from a counter until the server is gone.
fn app_thread(rx: Receiver<Request>) {
    std::thread::spawn(move || {
        let mut c = Counter::default();
        let info = AppInfo::new("counter", "0.1.0");
        while let Ok(req) = rx.recv() {
            handle(req, &mut c, &mut NoUi, &info);
        }
    });
}

fn boot(name: &str, tweak: impl FnOnce(&mut Config)) -> (Server, Arc<AtomicUsize>) {
    let mut config = Config::new(0, temp_dir(name));
    tweak(&mut config);
    let wakes = Arc::new(AtomicUsize::new(0));
    let w = wakes.clone();
    let (server, rx) = Server::start(config, move || {
        w.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();
    app_thread(rx);
    (server, wakes)
}

/// One raw connection, several lines.
struct Conn(BufReader<TcpStream>, TcpStream);

impl Conn {
    fn open(port: u16) -> Self {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Self(BufReader::new(s.try_clone().unwrap()), s)
    }
    fn send(&mut self, line: &str) {
        writeln!(self.1, "{line}").unwrap();
    }
    fn recv(&mut self) -> Value {
        let mut r = String::new();
        let _ = self.0.read_line(&mut r);
        serde_json::from_str(&r).unwrap_or_default()
    }
    fn call(&mut self, line: &str) -> Value {
        self.send(line);
        self.recv()
    }
    /// The server closed the connection.
    fn closed(&mut self) -> bool {
        let mut r = String::new();
        matches!(self.0.read_line(&mut r), Ok(0) | Err(_))
    }
}

#[test]
fn client_round_trip_on_an_ephemeral_port() {
    let (server, wakes) = boot("roundtrip", |_| {});
    let dir = server.key_file().parent().unwrap().to_path_buf();
    let mut c = Client::connect(server.port(), &dir).unwrap();
    assert_eq!(c.call("app.ping", json!({})).unwrap(), json!("pong"));
    let v = c.call("app.version", json!({})).unwrap();
    assert_eq!((v["name"].clone(), v["protocol"].clone()), (json!("counter"), json!(1)));
    assert_eq!(c.execute("count.add", json!({"by": 41})).unwrap(), json!(41));
    assert_eq!(c.execute("count.add", json!({})).unwrap(), json!(42));
    // Command errors keep their kind across the wire.
    assert_eq!(c.execute("count.fail", json!({})), Err(CommandError::Disabled { id: "count.fail".into(), reason: "never".into() }));
    assert_eq!(c.execute("nope", json!({})), Err(CommandError::Unknown { id: "nope".into() }));
    assert_eq!(c.commands().len(), 3);
    let batch = c.call("engine.batch", json!({"commands": [{"command": "count.add"}, {"command": "count.get"}]})).unwrap();
    assert_eq!(batch[1]["result"], 43);
    let ui = c.call("ui.click", json!({"x": 1, "y": 2})).unwrap_err();
    assert!(matches!(&ui, ControlError::Remote { error, .. } if error.contains("headless")), "{ui:?}");
    assert!(wakes.load(Ordering::SeqCst) >= 8);
}

#[test]
fn the_key_file_is_private_and_removed_with_the_server() {
    let (server, _) = boot("keyfile", |_| {});
    let p = server.key_file().to_path_buf();
    assert_eq!(p.file_name().unwrap().to_str().unwrap(), format!("control-key.local-{}", server.port()));
    let key = std::fs::read_to_string(&p).unwrap();
    assert_eq!(key, server.key());
    assert_eq!(key.len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    }
    let port = server.port();
    drop(server);
    assert!(!p.exists());
    // The port stops accepting.
    std::thread::sleep(Duration::from_millis(100));
    assert!(Client::connect_with_key(port, key).is_err());
}

#[test]
fn requests_without_the_right_key_are_refused_and_closed() {
    let (server, wakes) = boot("auth", |_| {});
    let key = server.key().to_string();
    for k in [String::new(), "nope".into(), key[..63].to_string(), format!("{key}0"), key.to_uppercase()] {
        let mut c = Conn::open(server.port());
        let r = c.call(&format!(r#"{{"id":1,"key":"{k}","method":"engine.execute","params":{{"command":"count.add"}}}}"#));
        assert_eq!((r["ok"].clone(), r["error"].clone(), r["id"].clone()), (json!(false), json!("unauthorized"), json!(1)));
        assert!(c.closed());
    }
    let mut c = Conn::open(server.port());
    assert_eq!(c.call(r#"{"method":"app.ping"}"#)["error"], "unauthorized");
    assert!(c.closed());
    assert_eq!(wakes.load(Ordering::SeqCst), 0, "nothing reached the app");
    // The client says so too.
    assert_eq!(Client::connect_with_key(server.port(), "wrong").unwrap_err(), ControlError::Unauthorized);
    let mut c = Client::connect_with_key(server.port(), key).unwrap();
    assert_eq!(c.call("engine.execute", json!({"command": "count.get"})).unwrap(), json!(0));
}

#[test]
fn http_post_cannot_smuggle_a_command() {
    // What a web page's `fetch("http://127.0.0.1:<port>/", {method: "POST", body})` sends, with the
    // key in the body (the worst case).
    let (server, wakes) = boot("http", |_| {});
    let body = format!("\n{{\"key\":\"{}\",\"method\":\"engine.execute\",\"params\":{{\"command\":\"count.add\"}}}}\n", server.key());
    let req = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: https://attacker.example\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
        server.port(),
        body.len()
    );
    let mut c = Conn::open(server.port());
    c.1.write_all(req.as_bytes()).unwrap();
    let r = c.recv();
    assert!(r["error"].as_str().unwrap().contains("closing the connection"), "{r}");
    assert!(c.closed());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
}

#[test]
fn hostile_lines_close_the_connection_without_reaching_the_app() {
    let (server, wakes) = boot("hostile", |c| c.max_line = 1024);
    let key = server.key().to_string();
    let lines: Vec<Vec<u8>> = vec![
        b"\x00\x01\x02\xff\xfe binary\n".to_vec(),
        b"[1,2,3]\n".to_vec(),
        b"\"just a string\"\n".to_vec(),
        b"12345\n".to_vec(),
        b"{\"id\":1}\n".to_vec(),
        b"{\"method\":[\"x\"]}\n".to_vec(),
        b"{{{{{{{{{{{{{{{{{{{{{{{{{{{{{{\n".to_vec(),
        format!("{}\n", "[".repeat(1000)).into_bytes(),
        format!("{{\"key\":\"{key}\",\"method\":\"app.ping\",\"pad\":\"{}\"}}\n", "x".repeat(2000)).into_bytes(),
    ];
    for line in lines {
        let mut c = Conn::open(server.port());
        c.1.write_all(&line).unwrap();
        let r = c.recv();
        assert_eq!(r["ok"], false, "{}", String::from_utf8_lossy(&line));
        assert!(c.closed());
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
    // The server is still fine.
    let mut c = Client::connect_with_key(server.port(), key).unwrap();
    assert_eq!(c.call("app.ping", json!({})).unwrap(), "pong");
}

#[test]
fn a_huge_line_is_refused_without_buffering_it() {
    let (server, wakes) = boot("huge", |c| c.max_line = 64 * 1024);
    let mut c = Conn::open(server.port());
    let w = c.1.try_clone().unwrap();
    // 8 MiB without a newline: the server answers after 64 KiB and drops the rest.
    let writer = std::thread::spawn(move || {
        let mut w = w;
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..128 {
            if w.write_all(&chunk).is_err() {
                return;
            }
        }
    });
    let r = c.recv();
    assert!(r["error"].as_str().unwrap().contains("longer than 65536"), "{r}");
    assert!(c.closed());
    let _ = writer.join();
    assert_eq!(wakes.load(Ordering::SeqCst), 0);
}

#[test]
fn a_connection_flood_is_capped() {
    let (server, _) = boot("flood", |c| c.max_connections = 4);
    let held: Vec<TcpStream> = (0..4).map(|_| TcpStream::connect(("127.0.0.1", server.port())).unwrap()).collect();
    std::thread::sleep(Duration::from_millis(100));
    for _ in 0..20 {
        let mut extra = Conn::open(server.port());
        assert_eq!(extra.recv()["error"], "too many control connections");
        assert!(extra.closed());
    }
    drop(held);
    // Slots come back once the held connections end.
    let t0 = Instant::now();
    loop {
        match Client::connect_with_key(server.port(), server.key()) {
            Ok(mut c) => {
                assert_eq!(c.call("app.ping", json!({})).unwrap(), "pong");
                break;
            }
            Err(e) => assert!(t0.elapsed() < Duration::from_secs(5), "{e}"),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_first_request_deadline_is_one_overall_deadline() {
    let (server, _) = boot("deadline", |c| c.first_request = Duration::from_millis(300));
    // A silent connection is closed.
    let mut quiet = Conn::open(server.port());
    let t0 = Instant::now();
    assert!(quiet.closed());
    assert!(t0.elapsed() < Duration::from_secs(3));
    // Blank lines dripped slowly do not extend it.
    let s = TcpStream::connect(("127.0.0.1", server.port())).unwrap();
    let mut w = s.try_clone().unwrap();
    let feeder = std::thread::spawn(move || {
        for _ in 0..40 {
            if w.write_all(b"\n").is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
    let t0 = Instant::now();
    let mut r = s;
    r.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut sink = Vec::new();
    let _ = r.read_to_end(&mut sink);
    assert!(t0.elapsed() < Duration::from_millis(1500), "{:?}", t0.elapsed());
    let _ = feeder.join();
    // After an authorised request the connection may stay idle.
    let mut busy = Client::connect_with_key(server.port(), server.key()).unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(busy.call("app.ping", json!({})).unwrap(), "pong");
}

#[test]
fn an_app_that_does_not_answer_times_out_and_a_dropped_request_errors() {
    let dir = temp_dir("timeout");
    let mut config = Config::new(0, &dir);
    config.reply_timeout = Duration::from_millis(200);
    let (server, rx) = Server::start(config, || {}).unwrap();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        while let Ok(req) = rx.recv() {
            if req.method() == "slow" {
                kept.push(req); // never answered
            } else {
                drop(req);
            }
        }
    });
    let mut c = Client::connect_with_key(server.port(), server.key()).unwrap();
    let e = c.call("slow", json!({})).unwrap_err();
    assert!(matches!(&e, ControlError::Remote { error, .. } if error.starts_with("timeout")), "{e:?}");
    let e = c.call("dropped", json!({})).unwrap_err();
    assert!(matches!(&e, ControlError::Remote { error, .. } if error.contains("without an answer")), "{e:?}");
}

#[test]
fn the_client_follows_an_app_restart_on_the_same_port() {
    let dir = temp_dir("restart");
    let (a, rx) = Server::start(Config::new(0, &dir), || {}).unwrap();
    app_thread(rx);
    let port = a.port();
    let mut c = Client::connect(port, &dir).unwrap();
    assert_eq!(c.call("app.ping", json!({})).unwrap(), "pong");
    let old_key = a.key().to_string();
    drop(a);
    std::thread::sleep(Duration::from_millis(100));
    let (b, rx) = Server::start(Config::new(port, &dir), || {}).unwrap();
    app_thread(rx);
    assert_ne!(b.key(), old_key);
    // The kept connection is dead; the client reconnects once and reads the new key.
    assert_eq!(c.call("app.ping", json!({})).unwrap(), "pong");
}

#[test]
fn no_app_listening_fails_fast() {
    let dir = temp_dir("none");
    let (s, _rx) = Server::start(Config::new(0, &dir), || {}).unwrap();
    let port = s.port();
    drop(s);
    std::thread::sleep(Duration::from_millis(100));
    let t0 = Instant::now();
    let e = Client::connect(port, &dir).unwrap_err();
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert!(matches!(e, ControlError::Key(_) | ControlError::Io(_)), "{e:?}");
}

#[test]
fn dropping_the_server_closes_open_connections() {
    let (server, _) = boot("close", |_| {});
    let mut c = Client::connect_with_key(server.port(), server.key()).unwrap();
    assert_eq!(c.call("app.ping", json!({})).unwrap(), "pong");
    let mut raw = Conn::open(server.port());
    drop(server);
    assert!(raw.closed());
    assert!(c.call("app.ping", json!({})).is_err());
}

#[test]
fn a_request_the_app_read_before_dying_is_never_sent_again() {
    // A fake app: authorises every connection, then reads one request and drops the connection
    // without answering, as an app that crashed while running the command would.
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            writeln!(writer, r#"{{"id":0,"ok":true,"result":null}}"#).unwrap();
            line.clear();
            if reader.read_line(&mut line).unwrap_or(0) > 0 {
                seen.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    let mut c = Client::connect_with_key(port, "key").unwrap();
    let e = c.execute("count.add", json!({})).unwrap_err();
    assert!(matches!(&e, CommandError::Failed { msg, .. } if msg.contains("closed")), "{e:?}");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(requests.load(Ordering::SeqCst), 1, "the request was sent again");
    // The next call reconnects and is sent (once) as usual.
    assert!(c.call("app.ping", json!({})).is_err());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}
