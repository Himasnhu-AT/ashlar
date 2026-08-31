//! A client for a running app's control port. It implements [`Dispatch`], so anything that drives
//! a headless session (the MCP server, the CLI) drives a live app the same way.

use std::io::{BufReader, ErrorKind, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ashlar_command::{CommandError, CommandInfo, Dispatch};
use serde_json::{Value, json};

use crate::ControlError;
use crate::key::{key_path, read_key_file};
use crate::lines::{Plain, ReadError, read_bounded_line};
use crate::protocol::Reply;

/// Longest reply line accepted (screenshots travel inline as base64).
pub const MAX_REPLY: usize = 256 * 1024 * 1024;

enum KeySource {
    File(PathBuf),
    Fixed(String),
}

struct Conn {
    reader: BufReader<Plain<TcpStream>>,
    writer: TcpStream,
}

/// A connection to `127.0.0.1:<port>` (the control port is loopback only, so the key never leaves
/// the machine). Reconnects when the app closed the kept connection or restarted (a restart also
/// means a new key, read again from the key file), but only before a request was sent: once any
/// byte of a request reached the socket it is never sent again, so a mutating command cannot run
/// twice when the app dies mid-request.
pub struct Client {
    port: u16,
    key: KeySource,
    conn: Option<Conn>,
    next_id: u64,
    timeout: Duration,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").field("port", &self.port).field("connected", &self.conn.is_some()).finish_non_exhaustive()
    }
}

impl Client {
    /// Connect to the app on `port` whose key file is in `key_dir`; fails fast when nothing
    /// listens or the key is refused.
    pub fn connect(port: u16, key_dir: &Path) -> Result<Self, ControlError> {
        Self::open(port, KeySource::File(key_path(key_dir, port)))
    }

    /// Connect with a key known in-process (tests, a parent that started the app).
    pub fn connect_with_key(port: u16, key: impl Into<String>) -> Result<Self, ControlError> {
        Self::open(port, KeySource::Fixed(key.into()))
    }

    fn open(port: u16, key: KeySource) -> Result<Self, ControlError> {
        let mut c = Self { port, key, conn: None, next_id: 1, timeout: Duration::from_secs(90) };
        c.reconnect()?;
        Ok(c)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// How long to wait for one reply (default 90 s; the app itself gives up after its own
    /// reply timeout, 60 s by default).
    pub fn set_timeout(&mut self, t: Duration) {
        self.timeout = t;
        if let Some(c) = &self.conn {
            let _ = c.writer.set_read_timeout(Some(t));
        }
    }

    fn key(&self) -> Result<String, ControlError> {
        match &self.key {
            KeySource::File(p) => read_key_file(p).map_err(ControlError::Key),
            KeySource::Fixed(k) => Ok(k.clone()),
        }
    }

    fn reconnect(&mut self) -> Result<(), ControlError> {
        self.conn = None;
        let key = self.key()?;
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let s = TcpStream::connect_timeout(&addr, Duration::from_millis(800))
            .map_err(|e| ControlError::Io(format!("no app is listening on 127.0.0.1:{} ({e})", self.port)))?;
        let _ = s.set_nodelay(true);
        let _ = s.set_read_timeout(Some(self.timeout));
        let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
        let read = s.try_clone().map_err(|e| ControlError::Io(e.to_string()))?;
        self.conn = Some(Conn { reader: BufReader::new(Plain(read)), writer: s });
        // Authenticate the connection once; later requests need not carry the key.
        match self.roundtrip(&json!({"id": 0, "method": "auth", "key": key}))? {
            Reply::Ok(_) => Ok(()),
            Reply::Err { error, .. } => {
                self.conn = None;
                Err(if error == "unauthorized" { ControlError::Unauthorized } else { ControlError::Protocol(error) })
            }
        }
    }

    fn roundtrip(&mut self, req: &Value) -> Result<Reply, ControlError> {
        match self.send(req) {
            Ok(()) => self.receive(),
            Err((_, e)) => Err(e),
        }
    }

    /// Write one request line. `Err((sent_nothing, error))`: `sent_nothing` is true when no byte
    /// of the request reached the socket, the only case that may be retried.
    fn send(&mut self, req: &Value) -> Result<(), (bool, ControlError)> {
        let port = self.port;
        let Some(c) = self.conn.as_mut() else { return Err((true, ControlError::Io("not connected".into()))) };
        let mut line = req.to_string().into_bytes();
        line.push(b'\n');
        let mut sent = 0usize;
        while let Some(rest) = line.get(sent..).filter(|r| !r.is_empty()) {
            match c.writer.write(rest) {
                Ok(0) => return Err((sent == 0, ControlError::Io(format!("control connection to 127.0.0.1:{port} closed while sending")))),
                Ok(n) => sent = sent.saturating_add(n),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err((sent == 0, ControlError::Io(format!("control connection to 127.0.0.1:{port} failed: {e}")))),
            }
        }
        Ok(())
    }

    fn receive(&mut self) -> Result<Reply, ControlError> {
        let Some(c) = self.conn.as_mut() else { return Err(ControlError::Io("not connected".into())) };
        let mut buf = Vec::new();
        match read_bounded_line(&mut c.reader, &mut buf, MAX_REPLY) {
            Ok(true) => {}
            Ok(false) => return Err(ControlError::Io("the app closed the control connection".into())),
            Err(ReadError::TooLong) => return Err(ControlError::Protocol(format!("reply longer than {MAX_REPLY} bytes"))),
            Err(ReadError::Io) => return Err(ControlError::Io("no reply from the app (timed out or connection lost)".into())),
        }
        let v: Value = serde_json::from_slice(&buf).map_err(|e| ControlError::Protocol(e.to_string()))?;
        Reply::from_json(&v).ok_or_else(|| ControlError::Protocol("reply without a boolean `ok`".into()))
    }

    /// Whether the kept connection is known dead: the app closed it (it quit or restarted) or sent
    /// something unasked. Checked without blocking, before a request is written.
    fn stale(&self) -> bool {
        let Some(c) = self.conn.as_ref() else { return true };
        if !c.reader.buffer().is_empty() || c.writer.set_nonblocking(true).is_err() {
            return true;
        }
        let mut probe = [0u8; 1];
        let alive = matches!(c.writer.peek(&mut probe), Err(e) if e.kind() == ErrorKind::WouldBlock);
        c.writer.set_nonblocking(false).is_err() || !alive
    }

    /// Send one request and return the app's reply; `Err` only for transport failures.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Reply, ControlError> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let req = json!({"id": id, "method": method, "params": params});
        // A kept connection the app closed (it quit or restarted) is replaced before sending.
        if self.stale() {
            self.reconnect()?;
        }
        let r = match self.send(&req) {
            Ok(()) => self.receive(),
            // Nothing was sent: the app cannot have seen the request, so one retry is safe.
            Err((true, _)) => self.reconnect().and_then(|()| self.roundtrip(&req)),
            // Part of the request may have reached the app: never send it again.
            Err((false, e)) => Err(e),
        };
        r.inspect_err(|_| self.conn = None)
    }

    /// Call one method: its `result`, or the app's error as [`ControlError::Remote`].
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, ControlError> {
        match self.request(method, params)? {
            Reply::Ok(v) => Ok(v),
            Reply::Err { error, detail } => Err(ControlError::Remote { error, detail }),
        }
    }
}

impl Dispatch for Client {
    /// The app's commands; empty when it cannot be reached (`Dispatch` has no error channel here;
    /// use [`Client::call`] with `engine.commands` to see why).
    fn commands(&self) -> Vec<CommandInfo> {
        // `commands` takes `&self`: talk on a separate short-lived connection.
        let key = match self.key() {
            Ok(k) => k,
            Err(_) => return Vec::new(),
        };
        let Ok(mut c) = Client::connect_with_key(self.port, key) else { return Vec::new() };
        c.timeout = self.timeout;
        c.call("engine.commands", json!({})).ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
    }

    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        self.call("engine.execute", json!({"command": id, "params": params})).map_err(|e| e.into_command_error(id))
    }
}

impl ControlError {
    /// As a [`CommandError`]: the app's own when it sent one, otherwise [`CommandError::Failed`].
    pub fn into_command_error(self, id: &str) -> CommandError {
        match self {
            ControlError::Remote { detail: Some(d), error } => {
                serde_json::from_value(d).unwrap_or(CommandError::Failed { id: id.to_string(), msg: error })
            }
            e => CommandError::Failed { id: id.to_string(), msg: e.to_string() },
        }
    }
}
