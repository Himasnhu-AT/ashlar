//! One running instance per user: a second launch forwards its arguments (the files it was asked
//! to open) to the first over a local socket, then exits.
//!
//! ```no_run
//! # use ashlar_native::single_instance::{Launch, SingleInstance};
//! let args: Vec<String> = std::env::args().skip(1).collect();
//! match SingleInstance::new("com.example.editor").claim(&args, |msg| {
//!     // On a listener thread: queue `msg.args` (resolved against `msg.cwd`) and wake the UI.
//! }) {
//!     Ok(Launch::Forwarded) => std::process::exit(0),
//!     Ok(Launch::Primary(guard)) => { /* keep `guard` alive while the app runs */ }
//!     Err(e) => { /* no single-instance support here: just run */ }
//! }
//! ```
//!
//! Transport: a Unix domain socket (macOS, Linux) in a per-user directory, readable only by the
//! user; on Windows a loopback TCP port written to a per-user file with a random token the
//! second launch must present. Messages are one JSON line, at most [`MAX_MESSAGE`] bytes.
//! The handshake has timeouts, so a hung first instance never hangs the second.

// The transport helpers are unused where there is no transport (the web).
#![cfg_attr(not(any(unix, windows)), allow(dead_code))]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::{NativeError, Result};

/// Largest forwarded message.
pub const MAX_MESSAGE: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(3);
const ACK: &str = "ok";

/// What a second launch sent.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Forwarded {
    /// The second launch's working directory (resolve relative arguments against it).
    pub cwd: PathBuf,
    /// Its arguments, without the program name.
    pub args: Vec<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Wire {
    v: u32,
    #[serde(default)]
    token: String,
    cwd: PathBuf,
    args: Vec<String>,
}

/// The outcome of [`SingleInstance::claim`].
pub enum Launch {
    /// This is the first instance; it listens while the guard lives.
    Primary(PrimaryGuard),
    /// Another instance is running and has the arguments: exit now.
    Forwarded,
}

impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Launch::Primary(_) => "Primary",
            Launch::Forwarded => "Forwarded",
        })
    }
}

/// Stops listening (and removes the socket) when dropped.
pub struct PrimaryGuard {
    stop: Arc<AtomicBool>,
    endpoint: PathBuf,
    #[allow(dead_code)]
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        (self.wake)();
        let _ = std::fs::remove_file(&self.endpoint);
    }
}

/// Which app the instance belongs to, and where its endpoint lives.
#[derive(Debug, Clone)]
pub struct SingleInstance {
    app_id: String,
    dir: Option<PathBuf>,
}

/// An app id is a reverse-DNS-like name: ASCII letters, digits, `.`, `-`, `_`; at most 64.
pub fn valid_app_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) && !id.starts_with('.')
}

impl SingleInstance {
    pub fn new(app_id: impl Into<String>) -> Self {
        Self { app_id: app_id.into(), dir: None }
    }

    /// Keep the endpoint in `dir` instead of the per-user default (tests, sandboxes).
    pub fn in_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = Some(dir.into());
        self
    }

    fn default_dir() -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            return Some(PathBuf::from(d));
        }
        // macOS: `$TMPDIR` is per user (`/var/folders/…`); Windows: `%TEMP%` is per user.
        Some(std::env::temp_dir())
    }

    fn endpoint(&self) -> Result<PathBuf> {
        if !valid_app_id(&self.app_id) {
            return Err(NativeError::Invalid(format!("`{}` is not an app id", self.app_id.chars().take(64).collect::<String>())));
        }
        let dir = self.dir.clone().or_else(Self::default_dir).ok_or(NativeError::Unsupported)?;
        #[cfg(unix)]
        let name = {
            // `/tmp` may be shared: keep users apart by name too.
            let user = std::env::var("USER").unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric()).take(32).collect::<String>();
            format!("{}.{}.sock", self.app_id, user)
        };
        #[cfg(not(unix))]
        let name = format!("{}.instance", self.app_id);
        Ok(dir.join(name))
    }

    /// Become the first instance, or forward `args` to it. `on_forward` runs on a listener thread
    /// for each later launch.
    pub fn claim(&self, args: &[String], on_forward: impl Fn(Forwarded) + Send + 'static) -> Result<Launch> {
        let endpoint = self.endpoint()?;
        let cwd = std::env::current_dir().unwrap_or_default();
        let msg = |token: String| Wire { v: 1, token, cwd: cwd.clone(), args: args.iter().take(4096).cloned().collect() };
        imp::claim(&endpoint, msg, Box::new(on_forward))
    }
}

/// Why forwarding failed.
enum SendError {
    /// The other end closed the connection without answering: no instance runs there (its socket
    /// outlived it, for example held by a child process that inherited it).
    Closed,
    Failed(NativeError),
}

impl From<SendError> for NativeError {
    fn from(e: SendError) -> Self {
        match e {
            SendError::Closed => NativeError::Os("the running instance closed the connection without accepting the files".into()),
            SendError::Failed(e) => e,
        }
    }
}

/// Send one message on `stream` and wait for the ack.
fn send<S: Read + Write>(stream: &mut S, wire: &Wire) -> std::result::Result<(), SendError> {
    let mut line = serde_json::to_vec(wire).map_err(|e| SendError::Failed(NativeError::Os(crate::os_text(e))))?;
    if line.len() > MAX_MESSAGE {
        return Err(SendError::Failed(NativeError::Invalid("too many arguments to forward".into())));
    }
    line.push(b'\n');
    let closed = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::NotConnected
        | std::io::ErrorKind::UnexpectedEof => SendError::Closed,
        _ => SendError::Failed(io(e)),
    };
    stream.write_all(&line).map_err(closed)?;
    stream.flush().map_err(closed)?;
    let mut reply = String::new();
    BufReader::new(stream.take(16)).read_line(&mut reply).map_err(closed)?;
    match reply.trim() {
        r if r == ACK => Ok(()),
        "" => Err(SendError::Closed),
        _ => Err(SendError::Failed(NativeError::Os("the running instance did not accept the files".into()))),
    }
}

/// Read one message from `stream`, check it, ack it.
fn receive<S: Read + Write>(stream: &mut S, token: &str) -> Option<Forwarded> {
    let mut line = Vec::new();
    let mut reader = BufReader::new(Read::by_ref(stream).take(MAX_MESSAGE as u64 + 1));
    reader.read_until(b'\n', &mut line).ok()?;
    if line.len() > MAX_MESSAGE || line.last() != Some(&b'\n') {
        return None;
    }
    let wire: Wire = serde_json::from_slice(&line).ok()?;
    if wire.v != 1 || wire.token != token || wire.args.len() > 4096 {
        return None;
    }
    let _ = stream.write_all(format!("{ACK}\n").as_bytes());
    Some(Forwarded { cwd: wire.cwd, args: wire.args })
}

fn io(e: std::io::Error) -> NativeError {
    NativeError::Os(crate::os_text(e))
}

type OnForward = Box<dyn Fn(Forwarded) + Send>;

/// Accept connections until `stop`, handing each message to `on_forward`.
fn serve<L, S>(listener: L, accept: impl Fn(&L) -> std::io::Result<S> + Send + 'static, token: String, stop: Arc<AtomicBool>, on_forward: OnForward)
where
    L: Send + 'static,
    S: Read + Write + Send,
{
    let spawned = std::thread::Builder::new().name("single-instance".into()).spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            let Ok(mut stream) = accept(&listener) else {
                // Out of descriptors or similar: don't spin.
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(msg) = receive(&mut stream, &token) {
                    on_forward(msg);
                }
            }));
            if r.is_err() {
                log::warn!("single-instance: a forwarded launch's handler panicked");
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("single-instance: no listener thread ({e})");
    }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    use super::*;

    /// macOS limits socket paths to 104 bytes, Linux to 108.
    const MAX_SOCKET_PATH: usize = 100;

    fn connect(endpoint: &Path) -> std::io::Result<UnixStream> {
        let s = UnixStream::connect(endpoint)?;
        s.set_read_timeout(Some(TIMEOUT))?;
        s.set_write_timeout(Some(TIMEOUT))?;
        Ok(s)
    }

    pub fn claim(endpoint: &Path, msg: impl Fn(String) -> Wire, on_forward: OnForward) -> Result<Launch> {
        if endpoint.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(NativeError::Invalid("the socket path is too long".into()));
        }
        for _ in 0..2 {
            match UnixListener::bind(endpoint) {
                Ok(listener) => {
                    let _ = std::fs::set_permissions(endpoint, std::fs::Permissions::from_mode(0o600));
                    let stop = Arc::new(AtomicBool::new(false));
                    let wake_path = endpoint.to_path_buf();
                    serve(
                        listener,
                        |l: &UnixListener| {
                            let (s, _) = l.accept()?;
                            s.set_read_timeout(Some(TIMEOUT))?;
                            s.set_write_timeout(Some(TIMEOUT))?;
                            Ok(s)
                        },
                        String::new(),
                        stop.clone(),
                        on_forward,
                    );
                    let wake = Box::new(move || {
                        let _ = UnixStream::connect(&wake_path);
                    });
                    return Ok(Launch::Primary(PrimaryGuard { stop, endpoint: endpoint.to_path_buf(), wake }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    match connect(endpoint).map_err(|_| SendError::Closed).and_then(|mut s| send(&mut s, &msg(String::new()))) {
                        Ok(()) => return Ok(Launch::Forwarded),
                        // Nobody listens, or something holds the socket but closes without answering
                        // (a crashed instance's socket, inherited by a child it spawned): no instance
                        // runs there. Remove it and retry.
                        Err(SendError::Closed) => {
                            let _ = std::fs::remove_file(endpoint);
                        }
                        Err(SendError::Failed(e)) => return Err(e),
                    }
                }
                Err(e) => return Err(io(e)),
            }
        }
        Err(NativeError::Os("could not claim the single-instance socket".into()))
    }
}

#[cfg(windows)]
mod imp {
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};

    use super::*;

    /// A random token: the time, the process id and the address of a fresh allocation, hashed.
    fn token() -> String {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        let a = std::collections::hash_map::RandomState::new().build_hasher().finish();
        format!("{:016x}{:016x}", h.finish(), a)
    }

    fn read_endpoint(endpoint: &Path) -> Option<(u16, String)> {
        let text = std::fs::read_to_string(endpoint).ok()?;
        let (port, token) = text.trim().split_once(' ')?;
        Some((port.parse().ok()?, token.to_string()))
    }

    pub fn claim(endpoint: &Path, msg: impl Fn(String) -> Wire, on_forward: OnForward) -> Result<Launch> {
        if let Some((port, token)) = read_endpoint(endpoint) {
            let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
            if let Ok(mut s) = TcpStream::connect_timeout(&addr, TIMEOUT) {
                let _ = s.set_read_timeout(Some(TIMEOUT));
                let _ = s.set_write_timeout(Some(TIMEOUT));
                if send(&mut s, &msg(token)).is_ok() {
                    return Ok(Launch::Forwarded);
                }
            }
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(io)?;
        let port = listener.local_addr().map_err(io)?.port();
        let token = token();
        let tmp = endpoint.with_extension("tmp");
        std::fs::write(&tmp, format!("{port} {token}")).map_err(io)?;
        std::fs::rename(&tmp, endpoint).map_err(io)?;
        let stop = Arc::new(AtomicBool::new(false));
        serve(
            listener,
            |l: &TcpListener| {
                let (s, _) = l.accept()?;
                s.set_read_timeout(Some(TIMEOUT))?;
                s.set_write_timeout(Some(TIMEOUT))?;
                Ok(s)
            },
            token,
            stop.clone(),
            on_forward,
        );
        let wake = Box::new(move || {
            let _ = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), Duration::from_millis(200));
        });
        Ok(Launch::Primary(PrimaryGuard { stop, endpoint: endpoint.to_path_buf(), wake }))
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::*;

    pub fn claim(_endpoint: &Path, _msg: impl Fn(String) -> Wire, _on_forward: OnForward) -> Result<Launch> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use super::*;

    #[test]
    fn app_ids_are_checked() {
        assert!(valid_app_id("com.example.editor"));
        for bad in ["", ".hidden", "a/b", "a b", "../x", &"x".repeat(65)] {
            assert!(!valid_app_id(bad), "{bad:?}");
        }
        assert!(matches!(SingleInstance::new("a/b").claim(&[], |_| {}), Err(NativeError::Invalid(_))));
    }

    /// Socket paths are capped near 100 bytes: short folder and app names keep the tests passing
    /// under a long `$TMPDIR` too.
    fn short_dir() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("").rand_bytes(4).tempdir().unwrap()
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_second_launch_forwards_its_arguments_to_the_first() {
        let dir = short_dir();
        let instance = SingleInstance::new("t.fwd").in_dir(dir.path());
        let (tx, rx) = channel();
        let first = instance.claim(&["first.txt".into()], move |m| tx.send(m).unwrap()).unwrap();
        assert!(matches!(first, Launch::Primary(_)));
        let second = instance.claim(&["a.txt".into(), "/abs/b.txt".into()], |_| panic!("not the primary")).unwrap();
        assert!(matches!(second, Launch::Forwarded));
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got.args, vec!["a.txt".to_string(), "/abs/b.txt".into()]);
        assert_eq!(got.cwd, std::env::current_dir().unwrap());
        // Garbage on the socket is ignored and the listener keeps going.
        #[cfg(unix)]
        {
            let ep = instance.endpoint().unwrap();
            let mut s = std::os::unix::net::UnixStream::connect(&ep).unwrap();
            s.write_all(b"{not json}\n").unwrap();
            drop(s);
        }
        assert!(matches!(instance.claim(&["c.txt".into()], |_| {}).unwrap(), Launch::Forwarded));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().args, vec!["c.txt".to_string()]);
        // When the first quits, the next launch becomes the first.
        drop(first);
        let third = instance.claim(&[], |_| {}).unwrap();
        assert!(matches!(third, Launch::Primary(_)));
    }

    /// A crashed instance's socket can outlive it in a child process that inherited it (a helper it
    /// spawned, or any process started while the socket was being made): connecting works, but the
    /// connection closes without an answer. That is no running instance either: take it over.
    #[cfg(unix)]
    #[test]
    fn a_socket_that_closes_without_answering_is_taken_over() {
        let dir = short_dir();
        let instance = SingleInstance::new("t.held").in_dir(dir.path());
        let ep = instance.endpoint().unwrap();
        let holder = std::os::unix::net::UnixListener::bind(&ep).unwrap();
        let closer = std::thread::spawn(move || {
            // accept one connection and close it unanswered, like a process that holds the socket
            // but runs no listener
            if let Ok((s, _)) = holder.accept() {
                drop(s);
            }
            holder
        });
        assert!(matches!(instance.claim(&[], |_| {}).unwrap(), Launch::Primary(_)));
        drop(closer.join());
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_socket_from_a_crash_is_taken_over() {
        let dir = short_dir();
        let instance = SingleInstance::new("t.stale").in_dir(dir.path());
        let ep = instance.endpoint().unwrap();
        // A socket file nobody listens on.
        drop(std::os::unix::net::UnixListener::bind(&ep).unwrap());
        assert!(ep.exists());
        assert!(matches!(instance.claim(&[], |_| {}).unwrap(), Launch::Primary(_)));
    }

    #[test]
    fn oversized_or_wrong_token_messages_are_refused() {
        struct Duplex(std::io::Cursor<Vec<u8>>, Vec<u8>);
        impl Read for Duplex {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                self.0.read(b)
            }
        }
        impl Write for Duplex {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.1.write(b)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let line = |token: &str| format!("{{\"v\":1,\"token\":\"{token}\",\"cwd\":\"/\",\"args\":[\"x\"]}}\n").into_bytes();
        let mut ok = Duplex(std::io::Cursor::new(line("t")), vec![]);
        assert!(receive(&mut ok, "t").is_some());
        assert_eq!(ok.1, b"ok\n");
        let mut wrong = Duplex(std::io::Cursor::new(line("nope")), vec![]);
        assert!(receive(&mut wrong, "t").is_none());
        assert!(wrong.1.is_empty());
        let mut huge = Duplex(std::io::Cursor::new(vec![b'x'; MAX_MESSAGE + 10]), vec![]);
        assert!(receive(&mut huge, "").is_none());
        let mut unterminated = Duplex(std::io::Cursor::new(b"{\"v\":1".to_vec()), vec![]);
        assert!(receive(&mut unterminated, "").is_none());
    }
}
