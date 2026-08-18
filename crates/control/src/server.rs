//! The loopback TCP control server.
//!
//! Hardening, all on by default and none optional:
//! - binds `127.0.0.1` only;
//! - every request carries the app's random key ([`crate::key`]), written to a 0600 file once the
//!   port is bound and removed when the [`Server`] is dropped;
//! - a line that is not a JSON request object closes the connection (blocks browser cross-origin
//!   POSTs and other junk), lines are bounded ([`Config::max_line`]);
//! - at most [`Config::max_connections`] connections at once;
//! - a connection must send an authorised request within [`Config::first_request`] (one overall
//!   deadline, a slow drip of bytes cannot extend it);
//! - the app answers within [`Config::reply_timeout`] or the client gets a `timeout` error;
//! - hostile input never panics a thread and never reaches the app thread.

use std::io::{BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

use crate::ControlError;
use crate::key::{KeyFile, key_path, random_key, write_key_file};
use crate::lines::{Auth, DEFAULT_MAX_LINE, Source, serve_lines};
use crate::protocol::{Reply, Request};

/// Server settings. [`Config::new`] gives the hardened defaults.
#[derive(Debug, Clone)]
pub struct Config {
    /// TCP port on 127.0.0.1; 0 picks a free one ([`Server::port`] tells which).
    pub port: u16,
    /// Folder for the key file (the app's settings folder).
    pub key_dir: PathBuf,
    /// Longest request line in bytes.
    pub max_line: usize,
    /// Connections served at once; further ones get an error line and are closed.
    pub max_connections: usize,
    /// A connection that sends no authorised request in this time is closed.
    pub first_request: Duration,
    /// How long a connection waits for the app thread to answer one request.
    pub reply_timeout: Duration,
}

impl Config {
    pub fn new(port: u16, key_dir: impl Into<PathBuf>) -> Self {
        Self {
            port,
            key_dir: key_dir.into(),
            max_line: DEFAULT_MAX_LINE,
            max_connections: 16,
            first_request: Duration::from_secs(30),
            reply_timeout: Duration::from_secs(60),
        }
    }
}

/// Repaint callback: called after a request is queued for the app thread.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// A running control server. Dropping it stops accepting connections and removes the key file.
pub struct Server {
    port: u16,
    key: Arc<str>,
    key_file: KeyFile,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("port", &self.port).field("key_file", &self.key_file).finish_non_exhaustive()
    }
}

impl Server {
    /// Bind, write the key file and serve. Requests arrive on the returned receiver; poll it on the
    /// app thread (each frame) and answer with [`crate::handle`] or [`Request::reply`]. `wake` is
    /// called after each request is queued (request a repaint).
    pub fn start(config: Config, wake: impl Fn() + Send + Sync + 'static) -> Result<(Server, Receiver<Request>), ControlError> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))
            .map_err(|e| ControlError::Io(format!("cannot bind 127.0.0.1:{}: {e}", config.port)))?;
        let port = listener.local_addr().map_err(|e| ControlError::Io(e.to_string()))?.port();
        let key: Arc<str> = random_key().map_err(ControlError::Key)?.into();
        let path = key_path(&config.key_dir, port);
        write_key_file(&path, &key).map_err(ControlError::Key)?;
        let key_file = KeyFile::new(path);
        log::info!("control server listening on 127.0.0.1:{port}, key in {}", key_file.path().display());
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Ctx { tx, wake: Arc::new(wake), key: key.clone(), config: Arc::new(config) };
        let stop2 = stop.clone();
        std::thread::Builder::new()
            .name("control-accept".into())
            .spawn(move || accept_loop(listener, ctx, &stop2))
            .map_err(|e| ControlError::Io(format!("cannot start the control thread: {e}")))?;
        Ok((Server { port, key, key_file, stop }, rx))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The key requests must carry (to hand to a trusted in-process client or child).
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn key_file(&self) -> &std::path::Path {
        self.key_file.path()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept so the thread sees the flag and drops the listener.
        let _ = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)), Duration::from_millis(200));
    }
}

#[derive(Clone)]
struct Ctx {
    tx: Sender<Request>,
    wake: Wake,
    key: Arc<str>,
    config: Arc<Config>,
}

fn accept_loop(listener: TcpListener, ctx: Ctx, stop: &AtomicBool) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let Ok(mut stream) = stream else { continue };
        if open.fetch_add(1, Ordering::SeqCst) >= ctx.config.max_connections {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
            let _ = writeln!(stream, "{}", Reply::err("too many control connections").to_json(None));
            continue;
        }
        // The slot is given back when the guard drops: after `serve` returns, if it panics, or if
        // the thread can't be started (the closure, and with it the guard, is dropped).
        let slot = Slot(Arc::clone(&open));
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("control-conn".into()).spawn(move || {
            let _slot = slot;
            serve(stream, &ctx);
        });
        if let Err(e) = spawned {
            log::error!("control server could not start a connection thread: {e}");
        }
    }
}

/// One counted connection; decrements the open-connection count when dropped.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The read side of a connection with an optional overall deadline: each read waits only for the
/// time left, so blank lines, bad JSON or a slow drip of bytes cannot extend it.
struct Timed {
    s: TcpStream,
    deadline: Option<Instant>,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(d) = self.deadline {
            let left = d.checked_duration_since(Instant::now()).filter(|l| !l.is_zero()).ok_or(std::io::ErrorKind::TimedOut)?;
            self.s.set_read_timeout(Some(left))?;
        }
        self.s.read(buf)
    }
}

impl Source for Timed {
    fn set_deadline(&mut self, d: Option<Instant>) {
        self.deadline = d;
        if d.is_none() {
            let _ = self.s.set_read_timeout(None);
        }
    }
}

fn serve(stream: TcpStream, ctx: &Ctx) {
    let Ok(read) = stream.try_clone() else { return };
    let _ = stream.set_nodelay(true);
    // A client that stops reading cannot block this thread forever.
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let deadline = Instant::now().checked_add(ctx.config.first_request);
    let mut reader = BufReader::new(Timed { s: read, deadline });
    serve_lines(&mut reader, stream, ctx.config.max_line, Auth::Key(&ctx.key), |method, params| {
        let (req, rrx) = Request::new(method, params);
        ctx.tx.send(req).ok()?;
        (ctx.wake)();
        Some(match rrx.recv_timeout(ctx.config.reply_timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => Reply::err(format!("timeout: the app did not answer within {} s", ctx.config.reply_timeout.as_secs())),
            Err(RecvTimeoutError::Disconnected) => Reply::err("the app dropped the request"),
        })
    });
}
