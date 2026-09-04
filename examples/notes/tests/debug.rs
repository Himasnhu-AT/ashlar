#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `ashlar-debug` driving the notes app: the windowed runtime on a headless event loop (always),
//! and the real binary in a real window (`ASHLAR_DEBUG_REAL_WINDOW=1`, needs a display).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ashlar::app::{Driver, Runtime};
use ashlar::fs::AppDirs;
use ashlar_debug::{Env, Exit, Io};
use notes::Notes;
use serde_json::Value;

fn debug(env: &Env, args: &[String], stdin: &str) -> (Exit, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut input = stdin.as_bytes();
    let code = ashlar_debug::run(args.to_vec(), env, &mut Io { input: &mut input, out: &mut out, err: &mut err });
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

fn env(root: &std::path::Path) -> Env {
    Env { home: root.join("debug-home"), config_roots: vec![root.join("none")], session: "default".into(), key_dir: None }
}

#[test]
fn the_cli_drives_the_running_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = AppDirs::under(dir.path());
    dirs.ensure().unwrap();
    let rt: Runtime<Notes> = notes::app().dirs(dirs.clone()).control_port(0).runtime();
    let mut d = Driver::new(rt);
    d.settle(10);
    let port = d.runtime.control_port().expect("serving");
    let env = env(dir.path());
    let base = ["--port".to_string(), port.to_string(), "--key-dir".into(), dirs.config.to_string_lossy().to_string()];

    let done = Arc::new(AtomicBool::new(false));
    let results = Arc::new(Mutex::new(Vec::new()));
    let (done2, results2) = (Arc::clone(&done), Arc::clone(&results));
    let agent = std::thread::spawn(move || {
        let with = |more: &[&str]| base.iter().cloned().chain(more.iter().map(|s| s.to_string())).collect::<Vec<_>>();
        let script = "exec note.add '{\"title\": \"from a script\"}'\nexec note.list\ncommands note\n";
        let mut out = vec![
            debug(&env, &with(&["--json", "status"]), ""),
            debug(&env, &with(&["batch", "-", "--verbose"]), script),
            debug(&env, &with(&["call", "ui.inspect"]), ""),
        ];
        // Once the app answers the debugger vocabulary, the snapshot and refs work too.
        let methods = serde_json::from_str::<Value>(out[0].1.trim()).ok().map(|v| v["result"]["app"]["methods"].to_string()).unwrap_or_default();
        if methods.contains("ui.snapshot") {
            out.push(debug(&env, &with(&["snapshot", "-i"]), ""));
        }
        *results2.lock().unwrap() = out;
        done2.store(true, Ordering::SeqCst);
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
        d.advance(0.02); // frames run only when the server wakes the loop
        std::thread::sleep(Duration::from_millis(2));
    }
    agent.join().unwrap();
    let r = results.lock().unwrap().clone();
    assert!(r.len() >= 3, "the agent did not finish");
    let status: Value = serde_json::from_str(r[0].1.trim()).unwrap();
    assert_eq!(status["result"]["app"]["name"], "notes", "{status}");
    let (code, out, err) = &r[1];
    assert_eq!(*code, Exit::Ok, "{out}{err}");
    assert!(out.contains("ok      1  exec note.add") && out.contains("\"count\":1") && out.contains("note.add"), "{out}");
    assert!(out.ends_with("batch: 3 steps passed (-)\n"), "{out}");
    assert_eq!(r[2].0, Exit::Ok, "{}", r[2].2);
    assert!(r[2].1.contains("\"document\""), "{}", r[2].1);
    if let Some((code, out, err)) = r.get(3) {
        assert_eq!(*code, Exit::Ok, "{err}");
        assert!(out.contains("[ref=e"), "{out}");
    }
}

#[test]
fn open_and_close_the_real_window() {
    if std::env::var_os("ASHLAR_DEBUG_REAL_WINDOW").is_none() {
        eprintln!("SKIPPED: set ASHLAR_DEBUG_REAL_WINDOW=1 to open the notes window (needs a display)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let env = env(dir.path());
    let s = |a: &[&str]| a.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let (code, out, err) = debug(&env, &s(&["open", env!("CARGO_BIN_EXE_notes")]), "");
    assert_eq!(code, Exit::Ok, "{out}{err}");
    let shot = dir.path().join("window.png");
    let steps = [
        s(&["exec", "note.add", "{\"title\": \"in a window\"}"]),
        s(&["wait", "--idle", "--timeout", "5000"]),
        s(&["screenshot", shot.to_str().unwrap()]),
        s(&["status"]),
    ];
    let mut failures = Vec::new();
    for a in &steps {
        let (code, out, err) = debug(&env, a, "");
        // `wait --idle` needs the debugger vocabulary; older apps say they do not answer it.
        if code != Exit::Ok && !(a[0] == "wait" && err.contains("unknown method")) {
            failures.push(format!("{a:?}: {out}{err}"));
        }
    }
    let (code, out, err) = debug(&env, &s(&["close"]), "");
    assert_eq!(code, Exit::Ok, "{out}{err}");
    assert!(failures.is_empty(), "{failures:#?}");
    assert_eq!(&std::fs::read(&shot).unwrap()[..4], b"\x89PNG");
}
