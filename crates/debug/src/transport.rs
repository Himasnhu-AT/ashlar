//! What a `.ui` script talks to: the TCP control port ([`Client`]) or anything else that answers
//! control methods, such as an in-process app driver on a virtual clock.
//!
//! [`run_script`] runs a script against any [`Transport`]. `sleep N` and `wait N` call
//! [`Transport::pause`], so a driver with virtual time lets that time pass instead of the wall
//! clock, and the same script gives the same steps on every run.

use std::time::Duration;

use ashlar_control::{Client, ControlError};
use serde_json::Value;

use crate::{Ctx, Env, Exit, Fail, Io};

/// Longest pause one `sleep`/`wait N` line asks for.
pub(crate) const MAX_PAUSE_MS: u64 = 600_000;

/// Something that answers control methods.
pub trait Transport {
    /// Call one method and return its result, or the app's error.
    fn call(&mut self, method: &str, params: Value) -> Result<Value, ControlError>;

    /// Let `ms` milliseconds pass (`sleep`, `wait <ms>`). The default waits on the wall clock.
    fn pause(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms.min(MAX_PAUSE_MS)));
    }

    /// Longest wait for one reply, where that means something (a socket).
    fn set_timeout(&mut self, _timeout: Duration) {}

    /// The TCP port, when there is one.
    fn port(&self) -> Option<u16> {
        None
    }
}

impl Transport for Client {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, ControlError> {
        Client::call(self, method, params)
    }

    fn set_timeout(&mut self, timeout: Duration) {
        Client::set_timeout(self, timeout);
    }

    fn port(&self) -> Option<u16> {
        Some(Client::port(self))
    }
}

/// A script that ran to the end.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Steps run (blank and comment lines do not count).
    pub passed: usize,
    /// One `{"line", "command", "ok", "result"}` object per step.
    pub steps: Vec<Value>,
}

/// A script that stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptError {
    /// `file:line: what went wrong`.
    pub error: String,
    /// The failing line, the steps before it and the app's detail, when the script ran.
    pub detail: Option<Value>,
    /// The script could not run as written (a bad command or argument), not a failed step.
    pub usage: bool,
}

impl ScriptError {
    /// The exit code `ashlar-debug batch` gives for this failure.
    pub fn exit(&self) -> Exit {
        if self.usage { Exit::Usage } else { Exit::Failed }
    }
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.error)
    }
}

impl std::error::Error for ScriptError {}

/// Run a `.ui` script (the `batch` language) against `transport`. `name` labels lines in errors.
/// Commands that manage sessions or processes (`open`, `connect`, `close`, `trace`, …) fail here:
/// the transport is the target.
pub fn run_script(transport: &mut dyn Transport, name: &str, text: &str) -> Result<Report, ScriptError> {
    let env = Env::under(std::path::Path::new(""));
    let (mut input, mut out, mut err) = (&b""[..], std::io::sink(), std::io::sink());
    let mut io = Io { input: &mut input, out: &mut out, err: &mut err };
    let mut ctx = Ctx::new(&env, &mut io, true);
    ctx.inproc = Some(transport);
    ctx.in_batch = true;
    match ctx.run_script(name, text, false) {
        Ok(done) => {
            let passed = done.result.get("passed").and_then(Value::as_u64).map_or(0, |n| usize::try_from(n).unwrap_or(usize::MAX));
            let steps = done.result.get("steps").and_then(Value::as_array).cloned().unwrap_or_default();
            Ok(Report { passed, steps })
        }
        Err(Fail::Failed { error, detail, .. }) => Err(ScriptError { error, detail, usage: false }),
        Err(Fail::Usage(error, _)) => Err(ScriptError { error, detail: None, usage: true }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use serde_json::json;

    /// Answers `ui.get` with the virtual time; `pause` moves it.
    #[derive(Default)]
    struct Fake {
        ms: u64,
        calls: Vec<String>,
    }

    impl Transport for Fake {
        fn call(&mut self, method: &str, _params: Value) -> Result<Value, ControlError> {
            self.calls.push(method.to_string());
            match method {
                "ui.get" => Ok(json!({"value": self.ms.to_string()})),
                "app.version" => Ok(json!({"name": "fake", "version": "1"})),
                _ => Err(ControlError::Remote { error: format!("unknown method {method}"), detail: None }),
            }
        }
        fn pause(&mut self, ms: u64) {
            self.ms += ms;
        }
    }

    #[test]
    fn sleep_and_wait_pass_virtual_time() {
        let mut f = Fake::default();
        let r = run_script(&mut f, "t.ui", "# virtual\nsleep 1000\nwait 500\nassert text id=clock 1500\n").unwrap();
        assert_eq!(r.passed, 3);
        assert_eq!(r.steps[2]["result"]["got"], "1500");
        assert_eq!(f.ms, 1500);
    }

    #[test]
    fn failures_name_the_line_and_session_verbs_are_refused() {
        let mut f = Fake::default();
        let e = run_script(&mut f, "t.ui", "sleep 10\nassert text id=clock 0\n").unwrap_err();
        assert!(e.error.starts_with("t.ui:2: expected text of id=clock"), "{e}");
        assert_eq!(e.exit(), Exit::Failed);
        assert_eq!(e.detail.as_ref().unwrap()["passed"], 1);
        f.calls.clear();
        for verb in ["open app", "connect 1", "close", "trace start", "sessions"] {
            let e = run_script(&mut f, "t.ui", verb).unwrap_err();
            assert!(e.error.contains("not available in an in-process script"), "{verb}: {e}");
        }
        assert!(f.calls.is_empty(), "{:?}", f.calls);
    }
}
