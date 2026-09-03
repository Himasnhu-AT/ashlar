#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Logging and the panic hook never write to stdout (a CLI prints results there, an MCP server
//! speaks JSON-RPC on it) and survive a closed stderr. Runs without the test harness, which
//! itself prints to stdout: the binary re-runs itself as the child under test.

use std::io::Read as _;
use std::process::{Command, Stdio};

use ashlar_log::{Filter, Logger, crash, logging};

const CHILD: &str = "ASHLAR_LOG_NO_STDOUT_CHILD";

fn child() {
    let dir = std::env::var_os("ASHLAR_LOG_NO_STDOUT_DIR").map(std::path::PathBuf::from);
    let logger = Logger::new("child", dir.as_deref(), log::LevelFilter::Trace, true).with_filter(Filter::parse("trace"));
    let max = logger.max_level();
    assert!(logging::install(Box::new(logger), max));
    crash::install(dir.as_deref(), "child", "0");
    // give the parent time to close its end of stderr
    std::thread::sleep(std::time::Duration::from_millis(200));
    for i in 0..2000 {
        log::info!("line {i} {}", "y".repeat(200));
    }
    let r = std::panic::catch_unwind(|| panic!("caught in the child"));
    assert!(r.is_err());
    logging::flush();
}

fn run(close_stderr: bool, dir: &std::path::Path) {
    let mut c = Command::new(std::env::current_exe().unwrap())
        .env(CHILD, "1")
        .env("ASHLAR_LOG_NO_STDOUT_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut err = c.stderr.take().unwrap();
    let mut err_text = String::new();
    if close_stderr {
        drop(err);
    } else {
        err.read_to_string(&mut err_text).unwrap();
    }
    let mut out = String::new();
    c.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    let status = c.wait().unwrap();
    assert!(status.success(), "child failed (close_stderr={close_stderr}): {status} {err_text}");
    assert!(out.is_empty(), "stdout must stay empty, got {} bytes: {:?}", out.len(), out.get(..200));
    if !close_stderr {
        assert!(err_text.contains("line 1999") && err_text.contains("caught in the child"), "stderr got the lines");
    }
    let log = std::fs::read_to_string(dir.join("child.log")).unwrap();
    assert!(log.contains("caught in the child"), "the file got the panic line");
}

fn main() {
    if std::env::var_os(CHILD).is_some() {
        child();
        return;
    }
    for close_stderr in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        run(close_stderr, dir.path());
    }
    eprintln!("no_stdout: ok");
}
