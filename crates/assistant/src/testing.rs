//! Test helpers (feature `testing`): a loopback HTTP server that answers with a handler, and
//! Anthropic-format streams to answer with. Apps use them to test their account-backend and
//! key-based setups without the network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

/// A request the server received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    /// Names lowercase.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// What to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpReply {
    pub fn json(status: u16, v: &Value) -> Self {
        Self { status, headers: vec![("content-type".into(), "application/json".into())], body: v.to_string().into_bytes() }
    }

    pub fn sse(text: impl Into<String>) -> Self {
        Self { status: 200, headers: vec![("content-type".into(), "text/event-stream".into())], body: text.into().into_bytes() }
    }

    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

type Handler = Arc<dyn Fn(&HttpRequest) -> HttpReply + Send + Sync>;

/// A loopback HTTP/1.1 server; stops when dropped.
pub struct MockServer {
    port: u16,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl MockServer {
    pub fn start(handler: impl Fn(&HttpRequest) -> HttpReply + Send + Sync + 'static) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let requests: Arc<Mutex<Vec<HttpRequest>>> = Arc::default();
        let handler: Handler = Arc::new(handler);
        let (s, r) = (stop.clone(), requests.clone());
        std::thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (h, r) = (handler.clone(), r.clone());
                        std::thread::spawn(move || {
                            let _ = serve(stream, &h, &r);
                        });
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Ok(Self { port, stop, requests })
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn serve(stream: TcpStream, handler: &Handler, log: &Mutex<Vec<HttpRequest>>) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let len = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0).min(64 * 1024 * 1024);
    let mut body = vec![0; len];
    reader.read_exact(&mut body)?;
    let req = HttpRequest { method, path, headers, body };
    log.lock().unwrap_or_else(PoisonError::into_inner).push(req.clone());
    let reply = handler(&req);
    let mut out = stream;
    let mut head = format!("HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n", reply.status, reply.body.len());
    for (k, v) in &reply.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    out.write_all(&reply.body)?;
    out.flush()
}

fn frame(out: &mut String, event: &str, data: &Value) {
    out.push_str(&format!("event: {event}\ndata: {data}\n\n"));
}

/// An Anthropic stream of `blocks` (`{"type":"text","text":…}`, `{"type":"tool_use","id","name","input"}`,
/// `{"type":"thinking","thinking","signature"}`) ending with `stop_reason`.
pub fn anthropic_stream(model: &str, blocks: &[Value], stop_reason: &str) -> String {
    let mut s = String::new();
    frame(
        &mut s,
        "message_start",
        &json!({"type": "message_start", "message": {"id": "msg_test", "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": null,
            "usage": {"input_tokens": 120, "output_tokens": 1, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}}),
    );
    for (i, b) in blocks.iter().enumerate() {
        match b["type"].as_str().unwrap_or_default() {
            "text" => {
                frame(
                    &mut s,
                    "content_block_start",
                    &json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}}),
                );
                frame(
                    &mut s,
                    "content_block_delta",
                    &json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": b["text"]}}),
                );
            }
            "thinking" => {
                frame(
                    &mut s,
                    "content_block_start",
                    &json!({"type": "content_block_start", "index": i, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
                );
                frame(
                    &mut s,
                    "content_block_delta",
                    &json!({"type": "content_block_delta", "index": i, "delta": {"type": "thinking_delta", "thinking": b["thinking"]}}),
                );
                frame(
                    &mut s,
                    "content_block_delta",
                    &json!({"type": "content_block_delta", "index": i, "delta": {"type": "signature_delta", "signature": b["signature"]}}),
                );
            }
            "tool_use" => {
                frame(
                    &mut s,
                    "content_block_start",
                    &json!({"type": "content_block_start", "index": i, "content_block": {"type": "tool_use", "id": b["id"], "name": b["name"], "input": {}}}),
                );
                let text = b["input"].to_string();
                let mid = text.len() / 2;
                let (a, c) = if text.is_char_boundary(mid) { text.split_at(mid) } else { (text.as_str(), "") };
                for part in [a, c] {
                    frame(
                        &mut s,
                        "content_block_delta",
                        &json!({"type": "content_block_delta", "index": i, "delta": {"type": "input_json_delta", "partial_json": part}}),
                    );
                }
            }
            _ => frame(&mut s, "content_block_start", &json!({"type": "content_block_start", "index": i, "content_block": b})),
        }
        frame(&mut s, "content_block_stop", &json!({"type": "content_block_stop", "index": i}));
    }
    frame(
        &mut s,
        "message_delta",
        &json!({"type": "message_delta", "delta": {"stop_reason": stop_reason, "stop_sequence": null}, "usage": {"output_tokens": 42}}),
    );
    frame(&mut s, "message_stop", &json!({"type": "message_stop"}));
    s
}

/// A text-only Anthropic stream.
pub fn anthropic_text(model: &str, text: &str) -> String {
    anthropic_stream(model, &[json!({"type": "text", "text": text})], "end_turn")
}
