use std::io::{Read, Write};
use std::net::TcpStream;

use super::*;

/// A handler that answers each request with its method and a count of messages in its session.
fn echo() -> Handler {
    let mut n = 0;
    Box::new(move |line: &str| {
        let v: Value = serde_json::from_str(line).ok()?;
        n += 1;
        v.get("id").map(|id| json!({"jsonrpc": "2.0", "id": id, "result": {"method": v["method"], "n": n}}).to_string())
    })
}

fn server(token: &str) -> HttpServer {
    HttpServer::start(Config::new(0, token), echo).unwrap()
}

/// A raw request; `(status, headers, body)`.
fn send(port: u16, method: &str, headers: &[(&str, &str)], body: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut req = format!("{method} /mcp HTTP/1.1\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    let code = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    (code, head.to_string(), body.to_string())
}

fn session_of(head: &str) -> String {
    head.lines().find_map(|l| l.strip_prefix("Mcp-Session-Id: ")).unwrap().to_string()
}

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;

#[test]
fn a_session_starts_with_initialize_and_carries_its_id() {
    let s = server("tok");
    let host = format!("127.0.0.1:{}", s.port());
    let auth = [("Host", host.as_str()), ("Authorization", "Bearer tok")];
    let (code, head, body) = send(s.port(), "POST", &auth, INIT);
    assert_eq!(code, 200, "{head}");
    assert!(body.contains("\"n\":1"));
    let id = session_of(&head);
    let with = [auth[0], auth[1], ("Mcp-Session-Id", id.as_str())];
    let (code, _, body) = send(s.port(), "POST", &with, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
    assert_eq!(code, 200);
    assert!(body.contains("\"n\":2") && body.contains("tools/list"), "{body}");
    // a notification is accepted with no body
    let (code, _, body) = send(s.port(), "POST", &with, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    assert_eq!((code, body.as_str()), (202, ""));
    // without a session, or with an unknown one
    assert_eq!(send(s.port(), "POST", &auth, r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#).0, 400);
    assert_eq!(send(s.port(), "POST", &[auth[0], auth[1], ("Mcp-Session-Id", "nope")], r#"{"jsonrpc":"2.0","id":3,"method":"x"}"#).0, 404);
    // ended
    assert_eq!(send(s.port(), "DELETE", &with, "").0, 200);
    assert_eq!(send(s.port(), "POST", &with, r#"{"jsonrpc":"2.0","id":4,"method":"x"}"#).0, 404);
    assert_eq!(send(s.port(), "GET", &auth, "").0, 405);
}

#[test]
fn the_token_is_required() {
    let s = server("secret-token");
    let host = format!("localhost:{}", s.port());
    for auth in [None, Some("Bearer wrong-token!"), Some("Bearer secret-toke"), Some("Basic c2VjcmV0"), Some("secret-token")] {
        let mut h = vec![("Host", host.as_str())];
        if let Some(a) = auth {
            h.push(("Authorization", a));
        }
        let (code, head, _) = send(s.port(), "POST", &h, INIT);
        assert_eq!(code, 401, "{auth:?}");
        assert!(head.contains("WWW-Authenticate: Bearer"));
    }
    assert_eq!(send(s.port(), "POST", &[("Host", host.as_str()), ("Authorization", "Bearer secret-token")], INIT).0, 200);
    assert!(!token_ok(Some("Bearer "), ""), "an empty token never matches");
}

#[test]
fn foreign_hosts_and_origins_are_refused() {
    let s = server("t");
    let p = s.port();
    let ok_host = format!("127.0.0.1:{p}");
    for host in ["evil.example", "evil.example:80", "127.0.0.1.evil.example", "192.168.1.5", &format!("127.0.0.1:{}", p.wrapping_add(1))] {
        assert_eq!(send(p, "POST", &[("Host", host), ("Authorization", "Bearer t")], INIT).0, 403, "{host}");
    }
    assert_eq!(send(p, "POST", &[("Authorization", "Bearer t")], INIT).0, 403, "no Host");
    for origin in ["http://evil.example", "https://localhost.evil.example", "null", "file://"] {
        assert_eq!(send(p, "POST", &[("Host", ok_host.as_str()), ("Origin", origin), ("Authorization", "Bearer t")], INIT).0, 403, "{origin}");
    }
    assert_eq!(send(p, "POST", &[("Host", ok_host.as_str()), ("Origin", "http://localhost:3000"), ("Authorization", "Bearer t")], INIT).0, 200);
    assert!(host_ok("localhost", p) && host_ok(&format!("[::1]:{p}"), p) && host_ok("LOCALHOST", p));
    assert!(origin_ok("http://[::1]:8080") && !origin_ok("http://[::2]"));
}

#[test]
fn a_closed_gate_refuses_every_method() {
    let cfg = Config::new(0, "t");
    let gate = cfg.gate.clone();
    let s = HttpServer::start(cfg, echo).unwrap();
    let host = format!("127.0.0.1:{}", s.port());
    let h = [("Host", host.as_str()), ("Authorization", "Bearer t")];
    gate.set(Some("Sign in first.".into()));
    let (code, _, body) = send(s.port(), "POST", &h, INIT);
    assert_eq!(code, 403);
    assert!(body.contains("Sign in first.") && body.contains("\"id\":1"), "{body}");
    gate.set(None);
    assert_eq!(send(s.port(), "POST", &h, INIT).0, 200);
}

#[test]
fn ports_fall_back_and_limits_hold() {
    let a = HttpServer::start(Config::new(0, "t"), echo).unwrap();
    let b = HttpServer::start(Config::new(a.port(), "t"), echo).unwrap();
    assert_ne!(a.port(), b.port(), "a taken port falls back");
    assert!(b.url().starts_with("http://127.0.0.1:") && b.url().ends_with("/mcp"));
    let host = format!("127.0.0.1:{}", a.port());
    let mut s = TcpStream::connect(("127.0.0.1", a.port())).unwrap();
    write!(s, "POST /mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer t\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 413"), "{out}");
    assert_eq!(send(a.port(), "POST", &[("Host", "x")], "").0, 403);
    let t = new_token().unwrap();
    assert_eq!(t.len(), 64);
    assert_ne!(t, new_token().unwrap());
    drop(a);
}
