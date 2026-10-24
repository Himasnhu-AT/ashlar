//! An MCP client over stdio: JSON-RPC 2.0, one message per line, to a child process.
//!
//! [`McpClient::start`] spawns the server with a clean environment, runs the `initialize`
//! handshake and keeps two threads: one reads the server's stdout (answers, notifications and the
//! server's own requests, which get `ping` answered and everything else refused), one forwards its
//! stderr to the log. Requests wait with a timeout and can be cancelled (both send
//! `notifications/cancelled`). Dropping the client closes the server's stdin, gives it a moment to
//! exit and then kills it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::tool::{CallOutput, Tool};

/// The protocol revision the client asks for.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Revisions the client accepts in the server's answer.
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// The longest line read from a server (a tool result with an image can be a few MB).
pub const MAX_LINE: usize = 64 * 1024 * 1024;
/// The longest stderr line forwarded to the log.
const MAX_LOG_LINE: usize = 2000;
/// At most this many pages of `tools/list`.
const MAX_PAGES: usize = 50;

/// How to start a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    /// The whole environment of the child (it inherits nothing).
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    /// The name stderr lines are logged under.
    pub label: String,
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    #[error("couldn't start the server: {0}")]
    Spawn(String),
    #[error("the server stopped{}", .0.as_ref().map(|s| format!(" ({s})")).unwrap_or_default())]
    Exited(Option<String>),
    #[error("the server didn't answer {method} within {secs} s")]
    Timeout { method: String, secs: u64 },
    #[error("cancelled")]
    Cancelled,
    #[error("the server answered {method} with error {code}: {message}")]
    Rpc { method: String, code: i64, message: String },
    #[error("the server's answer to {method} isn't valid: {detail}")]
    Protocol { method: String, detail: String },
    #[error("couldn't write to the server: {0}")]
    Io(String),
}

impl McpError {
    /// The server process is gone (a restart may help).
    pub fn is_exit(&self) -> bool {
        matches!(self, McpError::Exited(_) | McpError::Io(_))
    }
}

/// What the server said about itself in `initialize`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub protocol_version: String,
    pub instructions: Option<String>,
}

type Reply = Result<Value, (i64, String)>;

#[derive(Default)]
struct Shared {
    pending: Mutex<HashMap<u64, Sender<Reply>>>,
    alive: AtomicBool,
    exit_reason: Mutex<Option<String>>,
    tools_changed: AtomicBool,
}

impl Shared {
    fn fail_all(&self, reason: Option<String>) {
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut r) = self.exit_reason.lock()
            && r.is_none()
        {
            *r = reason;
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear(); // dropping the senders wakes every waiter with a disconnect
        }
    }

    fn exit_reason(&self) -> Option<String> {
        self.exit_reason.lock().ok().and_then(|r| r.clone())
    }
}

/// A running MCP server.
pub struct McpClient {
    child: Mutex<Option<Child>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    info: ServerInfo,
    label: String,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient").field("label", &self.label).field("info", &self.info).finish_non_exhaustive()
    }
}

impl McpClient {
    /// Spawn the server and run the handshake (`initialize`, then `notifications/initialized`).
    pub fn start(cmd: &ServerCommand, client_name: &str, client_version: &str, timeout: Duration) -> Result<McpClient, McpError> {
        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args).env_clear().stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in &cmd.env {
            command.env(k, v);
        }
        if let Some(dir) = &cmd.cwd {
            command.current_dir(dir);
        }
        let mut child = command.spawn().map_err(|e| McpError::Spawn(format!("{}: {e}", cmd.program.display())))?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(McpError::Spawn("no stdio pipes".into()));
        };
        let shared = Arc::new(Shared::default());
        shared.alive.store(true, Ordering::SeqCst);
        let stdin = Arc::new(Mutex::new(Some(stdin)));
        spawn_reader(&cmd.label, stdout, Arc::clone(&shared), Arc::clone(&stdin));
        spawn_stderr(&cmd.label, stderr);
        let mut client = McpClient {
            child: Mutex::new(Some(child)),
            stdin,
            shared,
            next_id: AtomicU64::new(1),
            info: ServerInfo::default(),
            label: cmd.label.clone(),
        };
        let params = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": client_name, "version": client_version},
        });
        let result = client.request("initialize", params, timeout, None)?;
        client.info = parse_initialize(&result)?;
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    pub fn info(&self) -> &ServerInfo {
        &self.info
    }

    /// The process is still running (as far as its stdout tells).
    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Ordering::SeqCst)
    }

    /// Why it stopped, when it did.
    pub fn exit_reason(&self) -> Option<String> {
        self.shared.exit_reason()
    }

    /// True once after the server sent `notifications/tools/list_changed`.
    pub fn take_tools_changed(&self) -> bool {
        self.shared.tools_changed.swap(false, Ordering::SeqCst)
    }

    /// Every tool (following `nextCursor`).
    pub fn list_tools(&self, timeout: Duration) -> Result<Vec<Tool>, McpError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = self.request("tools/list", params, timeout, None)?;
            let Some(tools) = result.get("tools").and_then(Value::as_array) else {
                return Err(McpError::Protocol { method: "tools/list".into(), detail: "no tools array".into() });
            };
            out.extend(tools.iter().filter_map(Tool::from_mcp));
            cursor = result.get("nextCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
            if cursor.is_none() {
                return Ok(out);
            }
        }
        Ok(out)
    }

    /// Call a tool. A tool's own failure is `Ok` with `is_error`; `Err` is the transport or the protocol.
    pub fn call_tool(&self, name: &str, arguments: &Value, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<CallOutput, McpError> {
        let args = if arguments.is_object() { arguments.clone() } else { json!({}) };
        let result = self.request("tools/call", json!({"name": name, "arguments": args}), timeout, cancel)?;
        CallOutput::from_mcp(&result).ok_or_else(|| McpError::Protocol { method: "tools/call".into(), detail: "no content array".into() })
    }

    /// One request; waits for its answer, the timeout, `cancel`, or the server's exit.
    pub fn request(&self, method: &str, params: Value, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<Value, McpError> {
        if !self.is_alive() {
            return Err(McpError::Exited(self.exit_reason()));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        if let Ok(mut pending) = self.shared.pending.lock() {
            pending.insert(id, tx);
        }
        if let Err(e) = self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})) {
            self.forget(id);
            return Err(e);
        }
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                self.forget(id);
                let _ = self.notify("notifications/cancelled", json!({"requestId": id, "reason": "timeout"}));
                return Err(McpError::Timeout { method: method.to_string(), secs: timeout.as_secs() });
            }
            if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                self.forget(id);
                let _ = self.notify("notifications/cancelled", json!({"requestId": id, "reason": "cancelled by the user"}));
                return Err(McpError::Cancelled);
            }
            match rx.recv_timeout((deadline - now).min(Duration::from_millis(50))) {
                Ok(Ok(v)) => return Ok(v),
                Ok(Err((code, message))) => return Err(McpError::Rpc { method: method.to_string(), code, message }),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(McpError::Exited(self.exit_reason())),
            }
        }
    }

    /// A notification (no answer).
    pub fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn forget(&self, id: u64) {
        if let Ok(mut pending) = self.shared.pending.lock() {
            pending.remove(&id);
        }
    }

    fn send(&self, msg: &Value) -> Result<(), McpError> {
        write_line(&self.stdin, msg)
    }

    /// Close stdin, wait up to `grace` for the server to exit, then kill it.
    pub fn shutdown(&self, grace: Duration) {
        if let Ok(mut stdin) = self.stdin.lock() {
            stdin.take();
        }
        let Ok(mut slot) = self.child.lock() else { return };
        let Some(mut child) = slot.take() else { return };
        let until = Instant::now() + grace;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(20)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        self.shared.fail_all(Some("stopped".into()));
    }

    /// The child's process id while it runs.
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().ok().and_then(|c| c.as_ref().map(Child::id))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.shutdown(Duration::from_millis(500));
    }
}

fn write_line(stdin: &Mutex<Option<ChildStdin>>, msg: &Value) -> Result<(), McpError> {
    let mut line = serde_json::to_vec(msg).map_err(|e| McpError::Io(e.to_string()))?;
    line.push(b'\n');
    let mut guard = stdin.lock().map_err(|_| McpError::Io("stdin lock poisoned".into()))?;
    let Some(pipe) = guard.as_mut() else { return Err(McpError::Exited(None)) };
    pipe.write_all(&line).and_then(|()| pipe.flush()).map_err(|e| McpError::Io(e.to_string()))
}

fn parse_initialize(result: &Value) -> Result<ServerInfo, McpError> {
    let bad = |detail: &str| McpError::Protocol { method: "initialize".into(), detail: detail.to_string() };
    let version = result.get("protocolVersion").and_then(Value::as_str).ok_or_else(|| bad("no protocolVersion"))?;
    if !SUPPORTED_VERSIONS.contains(&version) {
        return Err(bad(&format!("unsupported protocol version {version:?}")));
    }
    let server = result.get("serverInfo");
    let field = |k: &str| server.and_then(|s| s.get(k)).and_then(Value::as_str).unwrap_or_default().chars().take(200).collect::<String>();
    Ok(ServerInfo {
        name: field("name"),
        version: field("version"),
        protocol_version: version.to_string(),
        instructions: result.get("instructions").and_then(Value::as_str).map(|s| s.chars().take(20_000).collect()),
    })
}

fn spawn_reader(label: &str, stdout: impl Read + Send + 'static, shared: Arc<Shared>, stdin: Arc<Mutex<Option<ChildStdin>>>) {
    let label = label.to_string();
    let spawned = thread::Builder::new().name(format!("mcp-{label}-out")).spawn(move || {
        let mut reader = BufReader::new(stdout);
        let reason = loop {
            match read_line(&mut reader) {
                Ok(None) => break None,
                Ok(Some(line)) => {
                    if line.iter().all(u8::is_ascii_whitespace) {
                        continue;
                    }
                    match serde_json::from_slice::<Value>(&line) {
                        Ok(msg) => handle(&label, &msg, &shared, &stdin),
                        Err(e) => log::warn!(target: "integrations", "[{label}] not JSON on stdout ({e}); ignored"),
                    }
                }
                Err(e) => break Some(e),
            }
        };
        shared.fail_all(reason);
    });
    if let Err(e) = spawned {
        log::error!(target: "integrations", "[{}] couldn't start the reader thread: {e}", label_of(&e));
    }
}

fn label_of(e: &std::io::Error) -> String {
    e.kind().to_string()
}

/// One line without its newline; `None` at the end of the stream. A line over [`MAX_LINE`] ends the stream.
fn read_line(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let mut line = Vec::new();
    let n = reader.take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line).map_err(|e| e.to_string())?;
    if n == 0 {
        return Ok(None);
    }
    if line.len() > MAX_LINE {
        return Err(format!("a message over {} MB", MAX_LINE / (1024 * 1024)));
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    Ok(Some(line))
}

fn handle(label: &str, msg: &Value, shared: &Shared, stdin: &Mutex<Option<ChildStdin>>) {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id");
    match (method, id) {
        // An answer to one of ours.
        (None, Some(id)) => {
            let Some(id) = id.as_u64() else { return };
            let sender = shared.pending.lock().ok().and_then(|mut p| p.remove(&id));
            let Some(sender) = sender else { return }; // timed out or cancelled already
            let reply = if let Some(err) = msg.get("error") {
                let code = err.get("code").and_then(Value::as_i64).unwrap_or(-32603);
                let message = err.get("message").and_then(Value::as_str).unwrap_or("error").chars().take(2000).collect();
                Err((code, message))
            } else {
                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
            };
            let _ = sender.send(reply);
        }
        // The server asks something of us.
        (Some(method), Some(id)) => {
            let answer = if method == "ping" {
                json!({"jsonrpc": "2.0", "id": id, "result": {}})
            } else {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("the client doesn't support {method}")}})
            };
            let _ = write_line(stdin, &answer);
        }
        (Some("notifications/tools/list_changed"), None) => shared.tools_changed.store(true, Ordering::SeqCst),
        (Some("notifications/message"), None) => {
            let params = msg.get("params");
            let level = params.and_then(|p| p.get("level")).and_then(Value::as_str).unwrap_or("info");
            let data = params.and_then(|p| p.get("data")).map(Value::to_string).unwrap_or_default();
            let data: String = data.chars().take(MAX_LOG_LINE).collect();
            match level {
                "debug" => log::debug!(target: "integrations", "[{label}] {data}"),
                "info" | "notice" => log::info!(target: "integrations", "[{label}] {data}"),
                "warning" => log::warn!(target: "integrations", "[{label}] {data}"),
                _ => log::error!(target: "integrations", "[{label}] {data}"),
            }
        }
        _ => {}
    }
}

fn spawn_stderr(label: &str, stderr: impl Read + Send + 'static) {
    let label = label.to_string();
    let _ = thread::Builder::new().name(format!("mcp-{label}-err")).spawn(move || {
        let mut reader = BufReader::new(stderr);
        while let Ok(Some(line)) = read_line(&mut reader) {
            let text = String::from_utf8_lossy(&line);
            let text = text.trim_end();
            if !text.is_empty() {
                let text: String = text.chars().take(MAX_LOG_LINE).collect();
                log::info!(target: "integrations", "[{label}] {text}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_answers_are_checked() {
        let ok = json!({"protocolVersion": "2025-06-18", "serverInfo": {"name": "x", "version": "1"}, "instructions": "hi"});
        let info = parse_initialize(&ok).unwrap();
        assert_eq!((info.name.as_str(), info.version.as_str(), info.instructions.as_deref()), ("x", "1", Some("hi")));
        assert!(parse_initialize(&json!({"protocolVersion": "1999-01-01"})).is_err());
        assert!(parse_initialize(&json!({})).is_err());
    }

    #[test]
    fn lines_are_read_without_newlines() {
        let mut r = BufReader::new(&b"{\"a\":1}\n{\"b\":2}"[..]);
        assert_eq!(read_line(&mut r).unwrap().unwrap(), b"{\"a\":1}");
        assert_eq!(read_line(&mut r).unwrap().unwrap(), b"{\"b\":2}");
        assert_eq!(read_line(&mut r).unwrap(), None);
    }
}
