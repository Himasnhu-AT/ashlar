//! Where tool calls end up: a control-protocol method call, answered in-process, by a running
//! app, or by a child process.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::time::Duration;

use ashlar_command::Dispatch;
use ashlar_control::{AppInfo, Client, MAX_REPLY, NoUi, Reply};
use serde_json::{Value, json};

/// What answers the MCP server's method calls.
pub enum Backend {
    /// An in-process session (no window): standard methods only.
    Headless(Box<dyn Dispatch>),
    /// A running app through its control port: standard and `ui.*` methods.
    Remote(Client),
    /// A running app reached on first use (and again after it restarts), so the MCP server can
    /// start before the app: until then each call says how to start it.
    App(AppPort),
    /// A child process speaking the control protocol on stdin/stdout (`<tool> serve`): a crash
    /// in a command kills the child, not the MCP server; the next call starts a fresh one.
    Subprocess(Subprocess),
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.describe())
    }
}

impl Backend {
    /// Call one control method. Transport failures become error replies.
    pub fn call(&mut self, method: &str, params: Value, app: &AppInfo) -> Reply {
        match self {
            Backend::Headless(d) => ashlar_control::answer(method, params, d.as_mut(), &mut NoUi, app),
            Backend::Remote(c) => c.request(method, params).unwrap_or_else(|e| Reply::err(format!("the app is not reachable: {e}"))),
            Backend::App(a) => a.call(method, params),
            Backend::Subprocess(s) => s.call(method, params),
        }
    }

    /// A short description for the server instructions.
    pub fn describe(&self) -> String {
        match self {
            Backend::Headless(_) => "headless session (no window)".into(),
            Backend::Remote(c) => format!("the running app on 127.0.0.1:{}", c.port()),
            Backend::App(a) => format!("the running app on 127.0.0.1:{}", a.port),
            Backend::Subprocess(s) => format!("a worker process ({})", s.program.display()),
        }
    }
}

/// A running app's control port, connected on first use.
pub struct AppPort {
    port: u16,
    key_dir: PathBuf,
    /// How to start the app with its control port, for the error when nothing answers.
    hint: String,
    client: Option<Client>,
}

impl AppPort {
    /// The app on `127.0.0.1:port` whose control key is in `key_dir`; `hint` tells a user how to
    /// start it (`start the app with \`notes --control 9876\``).
    pub fn new(port: u16, key_dir: impl Into<PathBuf>, hint: impl Into<String>) -> Self {
        Self { port, key_dir: key_dir.into(), hint: hint.into(), client: None }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    fn call(&mut self, method: &str, params: Value) -> Reply {
        let client = match self.client.as_mut() {
            Some(c) => c,
            None => match Client::connect(self.port, &self.key_dir) {
                Ok(c) => self.client.insert(c),
                Err(e) => return Reply::err(format!("cannot reach the app on 127.0.0.1:{} ({e}); {}", self.port, self.hint)),
            },
        };
        match client.request(method, params) {
            Ok(r) => r,
            Err(e) => {
                // the app went away: connect again on the next call
                self.client = None;
                Reply::err(format!("the app is not reachable: {e}; {}", self.hint))
            }
        }
    }
}

/// A child process that serves the control protocol on its stdin/stdout.
pub struct Subprocess {
    program: PathBuf,
    args: Vec<String>,
    timeout: Duration,
    running: Option<Running>,
    next_id: u64,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    /// Reply lines read by a thread (bounded, so a chatty child cannot grow memory).
    lines: Receiver<Vec<u8>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Subprocess {
    /// `program args…` must serve the control protocol on stdio (`ashlar_control::serve_stream`).
    /// Started on the first call.
    pub fn new(program: impl Into<PathBuf>, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            timeout: Duration::from_secs(300),
            running: None,
            next_id: 1,
        }
    }

    /// Longest one call may take before the child is killed (default 300 s).
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    fn start(&mut self) -> Result<&mut Running, String> {
        let alive = self.running.as_mut().is_some_and(|r| matches!(r.child.try_wait(), Ok(None)));
        if alive {
            return self.running.as_mut().ok_or_else(|| "the worker vanished".to_string());
        }
        self.running = None;
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // The child's logs go where ours go (stderr), never into the protocol stream.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("could not start {}: {e}", self.program.display()))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err("the worker has no stdin/stdout".into());
        };
        let (tx, rx) = sync_channel(4);
        let spawned = std::thread::Builder::new().name("mcp-worker-read".into()).spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut buf = Vec::new();
                let limit = u64::try_from(MAX_REPLY).unwrap_or(u64::MAX).saturating_add(1);
                match r.by_ref().take(limit).read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) if buf.len() > MAX_REPLY => return,
                    Ok(_) => {
                        if tx.send(buf).is_err() {
                            return;
                        }
                    }
                }
            }
        });
        if let Err(e) = spawned {
            let _ = child.kill();
            return Err(format!("could not start a reader thread: {e}"));
        }
        Ok(self.running.insert(Running { child, stdin, lines: rx }))
    }

    /// One request and its reply. A child that dies, hangs past the timeout or talks nonsense is
    /// killed; the next call starts a new one.
    pub fn call(&mut self, method: &str, params: Value) -> Reply {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let timeout = self.timeout;
        let program = self.program.display().to_string();
        let r = match self.start() {
            Ok(r) => r,
            Err(e) => return Reply::err(e),
        };
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        let sent = r.stdin.write_all(format!("{line}\n").as_bytes()).and_then(|()| r.stdin.flush());
        let outcome = match sent {
            Err(e) => Err(format!("the worker {program} is gone ({e})")),
            Ok(()) => loop {
                match r.lines.recv_timeout(timeout) {
                    Ok(buf) => {
                        // Stray output (a library printing to stdout) or a reply to an earlier
                        // request: skip it; the timeout bounds a child that only prints noise.
                        let Ok(v) = serde_json::from_slice::<Value>(&buf) else {
                            log::debug!("worker output that is not a reply: {}", String::from_utf8_lossy(&buf).trim());
                            continue;
                        };
                        if v.get("id") != Some(&json!(id)) {
                            continue;
                        }
                        break Reply::from_json(&v).ok_or_else(|| format!("the worker {program} sent a reply without `ok`"));
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        break Err(format!("`{method}` did not finish within {} s; the worker was stopped", timeout.as_secs()));
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        let status = r.child.wait().map(|s| s.to_string()).unwrap_or_else(|e| e.to_string());
                        break Err(format!("the worker stopped without an answer while running `{method}` ({status}); it will be restarted"));
                    }
                }
            },
        };
        match outcome {
            Ok(reply) => reply,
            Err(e) => {
                self.running = None;
                Reply::err(e)
            }
        }
    }
}
