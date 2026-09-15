//! MCP over streamable HTTP on the loopback interface, for an app that lets local agents drive its
//! open window: `POST /mcp` carries one JSON-RPC message (or a batch) and gets the reply as
//! `application/json` (`202 Accepted` for a notification); `DELETE /mcp` ends a session; there is
//! no server-to-client stream (`GET` is `405`).
//!
//! The transport is app-neutral: each session gets its own handler from a factory (`line in, reply
//! out`, e.g. an MCP server bridged to the app's control port), made and run on one worker thread,
//! so handlers need not be `Send`.
//!
//! Every request must pass, in this order: the path; a loopback `Host` (`127.0.0.1`, `localhost`,
//! `[::1]`, with or without the port) and, when present, a loopback `Origin` (DNS rebinding); the
//! bearer token (`Authorization: Bearer …`, compared in constant time); and the [`Gate`] (the app
//! closes it while it must not be driven, e.g. a sign-in screen is up). The socket binds
//! `127.0.0.1` only. Bodies over [`MAX_BODY`], headers over [`MAX_HEAD`] and more than
//! [`MAX_CONNECTIONS`] connections at once are refused.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::{Value, json};

/// The endpoint's path.
pub const PATH: &str = "/mcp";
/// Largest request body.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
/// Largest request line plus headers.
pub const MAX_HEAD: usize = 16 * 1024;
/// Connections served at once.
pub const MAX_CONNECTIONS: usize = 32;
/// Sessions kept; the least recently used goes when a new one starts past this.
pub const MAX_SESSIONS: usize = 16;
/// The session header.
pub const SESSION_HEADER: &str = "Mcp-Session-Id";

/// One session's handler: a JSON-RPC message in, the reply out (`None` for a notification).
pub type Handler = Box<dyn FnMut(&str) -> Option<String>>;

/// Open or closed for every request. Closed, every request is refused with the reason.
#[derive(Clone, Default)]
pub struct Gate(Arc<Mutex<Option<String>>>);

impl Gate {
    /// Close it (`Some(reason)`) or open it (`None`).
    pub fn set(&self, closed: Option<String>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = closed;
    }

    /// Why it is closed; `None` when open.
    pub fn closed(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// Where and how to serve.
#[derive(Clone)]
pub struct Config {
    /// The preferred port; when taken, the next `tries - 1` ports, then any free one.
    pub port: u16,
    pub tries: u16,
    /// The bearer token every request must carry.
    pub token: String,
    pub gate: Gate,
}

impl Config {
    pub fn new(port: u16, token: impl Into<String>) -> Self {
        Self { port, tries: 10, token: token.into(), gate: Gate::default() }
    }
}

/// A random bearer token: 32 bytes from the operating system, as 64 hex digits.
pub fn new_token() -> io::Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// A running endpoint; dropping it stops it.
pub struct HttpServer {
    port: u16,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl HttpServer {
    /// Bind and serve. `factory` makes each session's handler, on the worker thread.
    pub fn start(config: Config, factory: impl Fn() -> Handler + Send + 'static) -> io::Result<Self> {
        let listener = bind(config.port, config.tries)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let (jobs, rx) = channel::<Job>();
        std::thread::Builder::new().name("mcp-http-worker".into()).spawn(move || worker(rx, factory))?;
        let s = stop.clone();
        let accept = std::thread::Builder::new().name("mcp-http-accept".into()).spawn(move || {
            let live = Arc::new(AtomicUsize::new(0));
            for conn in listener.incoming() {
                if s.load(Ordering::Relaxed) {
                    return;
                }
                let Ok(mut stream) = conn else { continue };
                if live.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    let _ = respond(&mut stream, 503, "Service Unavailable", &[], "application/json", br#"{"error":"too many connections"}"#);
                    continue;
                }
                live.fetch_add(1, Ordering::Relaxed);
                let (cfg, jobs, done, stop) = (config.clone(), jobs.clone(), live.clone(), s.clone());
                let spawned = std::thread::Builder::new().name("mcp-http-conn".into()).spawn(move || {
                    if !stop.load(Ordering::Relaxed) {
                        let _ = serve(stream, port, &cfg, &jobs);
                    }
                    done.fetch_sub(1, Ordering::Relaxed);
                });
                if spawned.is_err() {
                    live.fetch_sub(1, Ordering::Relaxed);
                }
            }
        })?;
        Ok(Self { port, stop, accept: Some(accept) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// `http://127.0.0.1:<port>/mcp`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{PATH}", self.port)
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // wake the accept loop
        let _ = TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, self.port).into(), Duration::from_millis(200));
        if let Some(t) = self.accept.take() {
            let _ = t.join();
        }
    }
}

fn bind(port: u16, tries: u16) -> io::Result<TcpListener> {
    if port != 0 {
        for p in (0..tries.max(1)).filter_map(|i| port.checked_add(i)) {
            if let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, p)) {
                return Ok(l);
            }
        }
    }
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
}

enum Job {
    /// A message for a session (`None`: start a new one); the reply and the session's id.
    Message {
        session: Option<String>,
        body: String,
        reply: Sender<Result<(String, Option<String>), String>>,
    },
    End {
        session: String,
        reply: Sender<bool>,
    },
}

fn worker(rx: Receiver<Job>, factory: impl Fn() -> Handler) {
    let mut sessions: Vec<(String, Handler, u64)> = Vec::new();
    let mut clock = 0u64;
    for job in rx {
        clock += 1;
        match job {
            Job::Message { session, body, reply } => {
                let id = match session {
                    Some(id) => id,
                    None => {
                        let Ok(id) = new_token() else {
                            let _ = reply.send(Err("no random source".into()));
                            continue;
                        };
                        if sessions.len() >= MAX_SESSIONS
                            && let Some(oldest) = sessions.iter().enumerate().min_by_key(|(_, s)| s.2).map(|(i, _)| i)
                        {
                            drop(sessions.swap_remove(oldest));
                        }
                        sessions.push((id.clone(), factory(), clock));
                        id
                    }
                };
                let Some(s) = sessions.iter_mut().find(|s| s.0 == id) else {
                    let _ = reply.send(Err("unknown session".into()));
                    continue;
                };
                s.2 = clock;
                // a handler that panics loses its session, not the endpoint
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (s.1)(&body)));
                match out {
                    Ok(r) => {
                        let _ = reply.send(Ok((id, r)));
                    }
                    Err(_) => {
                        sessions.retain(|x| x.0 != id);
                        let _ = reply.send(Err("the session failed".into()));
                    }
                }
            }
            Job::End { session, reply } => {
                let had = sessions.iter().any(|s| s.0 == session);
                sessions.retain(|s| s.0 != session);
                let _ = reply.send(had);
            }
        }
    }
}

/// A parsed request head.
struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn get(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_head(r: &mut impl BufRead) -> io::Result<Option<Head>> {
    let mut lines = Vec::new();
    let mut total = 0usize;
    loop {
        let mut line = String::new();
        let n = r.take((MAX_HEAD.saturating_sub(total) + 1) as u64).read_line(&mut line)?;
        if n == 0 {
            return Ok(None);
        }
        total = total.saturating_add(n);
        if total > MAX_HEAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "headers too large"));
        }
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            break;
        }
        lines.push(line);
    }
    let mut it = lines.into_iter();
    let first = it.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad request line"));
    };
    let path = target.split('?').next().unwrap_or("").to_string();
    let headers = it.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
    Ok(Some(Head { method: method.to_string(), path, headers }))
}

/// `host` (a `Host` header) names this machine's loopback interface.
pub fn host_ok(host: &str, port: u16) -> bool {
    let host = host.trim().to_ascii_lowercase();
    let (name, p) = match host.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((n, tail)) => (format!("[{n}]"), tail.strip_prefix(':')),
            None => return false,
        },
        None => match host.rsplit_once(':') {
            Some((n, p)) => (n.to_string(), Some(p)),
            None => (host.clone(), None),
        },
    };
    let port_ok = p.is_none_or(|p| p.parse::<u16>().ok() == Some(port));
    port_ok && matches!(name.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

/// `origin` (an `Origin` header) is a page on this machine's loopback interface.
pub fn origin_ok(origin: &str) -> bool {
    let o = origin.trim().to_ascii_lowercase();
    let Some(rest) = o.strip_prefix("http://").or_else(|| o.strip_prefix("https://")) else { return false };
    let host = rest.split('/').next().unwrap_or("");
    let name = if host.starts_with('[') {
        host.split(']').next().map(|n| format!("{n}]")).unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or("").to_string()
    };
    matches!(name.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

/// `header` (an `Authorization` header) carries `token`, compared in constant time.
pub fn token_ok(header: Option<&str>, token: &str) -> bool {
    let Some(given) = header.and_then(|h| h.trim().strip_prefix("Bearer ").or_else(|| h.trim().strip_prefix("bearer "))) else { return false };
    let (a, b) = (given.trim().as_bytes(), token.as_bytes());
    if token.is_empty() || a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn respond(s: &mut TcpStream, code: u16, reason: &str, extra: &[(&str, String)], ctype: &str, body: &[u8]) -> io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n",
        body.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

/// A JSON-RPC error for `body`'s request id (null when it has none).
fn rpc_error(body: Option<&str>, code: i64, message: &str) -> Vec<u8> {
    let id = body.and_then(|b| serde_json::from_str::<Value>(b).ok()).and_then(|v| v.get("id").cloned()).unwrap_or(Value::Null);
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string().into_bytes()
}

fn serve(stream: TcpStream, port: u16, cfg: &Config, jobs: &Sender<Job>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut out = stream.try_clone()?;
    let mut r = BufReader::new(stream);
    let head = match read_head(&mut r) {
        Ok(Some(h)) => h,
        Ok(None) => return Ok(()),
        Err(_) => return respond(&mut out, 400, "Bad Request", &[], "application/json", br#"{"error":"bad request"}"#),
    };
    let reply = handle(&head, &mut r, port, cfg, jobs);
    let (code, reason, extra, body) = reply;
    let ctype = "application/json";
    respond(&mut out, code, reason, &extra, ctype, &body)?;
    let _ = out.shutdown(Shutdown::Both);
    Ok(())
}

type Reply = (u16, &'static str, Vec<(&'static str, String)>, Vec<u8>);

fn handle(head: &Head, r: &mut impl Read, port: u16, cfg: &Config, jobs: &Sender<Job>) -> Reply {
    let err = |code, reason, msg: &str| (code, reason, Vec::new(), json!({"error": msg}).to_string().into_bytes());
    if head.path != PATH {
        return err(404, "Not Found", "the MCP endpoint is /mcp");
    }
    if !head.get("host").is_some_and(|h| host_ok(h, port)) {
        return err(403, "Forbidden", "requests must be addressed to 127.0.0.1 or localhost");
    }
    if head.get("origin").is_some_and(|o| !origin_ok(o)) {
        return err(403, "Forbidden", "requests from web pages on other hosts are refused");
    }
    if !token_ok(head.get("authorization"), &cfg.token) {
        return (
            401,
            "Unauthorized",
            vec![("WWW-Authenticate", "Bearer".to_string())],
            json!({"error": "a valid bearer token is required"}).to_string().into_bytes(),
        );
    }
    let session = head.get(SESSION_HEADER).map(str::to_string);
    match head.method.as_str() {
        "POST" => {}
        "DELETE" => {
            let Some(id) = session else { return err(400, "Bad Request", "no session") };
            let (tx, rx) = channel();
            if jobs.send(Job::End { session: id, reply: tx }).is_err() {
                return err(503, "Service Unavailable", "stopped");
            }
            return if rx.recv().unwrap_or(false) { (200, "OK", Vec::new(), b"{}".to_vec()) } else { err(404, "Not Found", "unknown session") };
        }
        _ => {
            return (
                405,
                "Method Not Allowed",
                vec![("Allow", "POST, DELETE".to_string())],
                json!({"error": "POST JSON-RPC messages to /mcp"}).to_string().into_bytes(),
            );
        }
    }
    if head.get("transfer-encoding").is_some() {
        return err(411, "Length Required", "send a Content-Length body");
    }
    let Some(len) = head.get("content-length").and_then(|l| l.parse::<usize>().ok()) else {
        return err(411, "Length Required", "send a Content-Length body");
    };
    if len > MAX_BODY {
        return err(413, "Payload Too Large", "the message is too large");
    }
    let mut body = vec![0u8; len];
    if r.read_exact(&mut body).is_err() {
        return err(400, "Bad Request", "the body ended early");
    }
    let Ok(body) = String::from_utf8(body) else { return err(400, "Bad Request", "the body is not UTF-8") };
    if let Some(reason) = cfg.gate.closed() {
        return (403, "Forbidden", Vec::new(), rpc_error(Some(&body), -32001, &reason));
    }
    let initialize = serde_json::from_str::<Value>(&body).ok().is_some_and(|v| {
        let is = |m: &Value| m.get("method").and_then(Value::as_str) == Some("initialize");
        is(&v) || v.as_array().is_some_and(|a| a.iter().any(is))
    });
    let session = match (initialize, session) {
        (true, _) => None,
        (false, Some(s)) => Some(s),
        (false, None) => {
            return (400, "Bad Request", Vec::new(), rpc_error(Some(&body), -32600, "send `initialize` first, then the Mcp-Session-Id it returns"));
        }
    };
    let (tx, rx) = channel();
    if jobs.send(Job::Message { session, body: body.clone(), reply: tx }).is_err() {
        return err(503, "Service Unavailable", "stopped");
    }
    match rx.recv() {
        Ok(Ok((id, Some(text)))) => (200, "OK", vec![(SESSION_HEADER, id)], text.into_bytes()),
        Ok(Ok((id, None))) => (202, "Accepted", vec![(SESSION_HEADER, id)], Vec::new()),
        Ok(Err(e)) if e == "unknown session" => (404, "Not Found", Vec::new(), rpc_error(Some(&body), -32001, "unknown session: initialize again")),
        Ok(Err(e)) => (500, "Internal Server Error", Vec::new(), rpc_error(Some(&body), -32603, &e)),
        Err(_) => err(503, "Service Unavailable", "stopped"),
    }
}

#[cfg(test)]
mod tests;
