#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Agents reach notes three ways: the CLI (`notes-cli`), the real TCP control port of a running
//! window, and MCP over the CLI.

use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ashlar::app::{Driver, Runtime};
use ashlar::cli::Io;
use ashlar::control::Client;
use ashlar::fs::AppDirs;
use ashlar::prelude::*;
use notes::Notes;

fn cli(root: &std::path::Path, args: &[&str], stdin: &str) -> (u8, String) {
    let mut input = Cursor::new(stdin.as_bytes().to_vec());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let exit = notes::app()
        .dirs(AppDirs::under(root))
        .cli()
        .run_with(args.iter().map(|s| s.to_string()).collect(), &mut Io { input: &mut input, out: &mut out, err: &mut err });
    (exit.code(), String::from_utf8(out).unwrap())
}

#[test]
fn notes_cli_runs_the_same_commands() {
    let dir = tempfile::tempdir().unwrap();
    let (code, out) = cli(dir.path(), &["commands", "--json"], "");
    assert_eq!(code, 0, "{out}");
    for id in ["note.add", "file.save_as", "app.undo", "view.theme"] {
        assert!(out.contains(id), "{id} missing: {out}");
    }
    let (code, out) = cli(dir.path(), &["exec", "note.add", r#"{"title":"cli"}"#], "");
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with(r#"{"ok":true"#), "{out}");

    let file = dir.path().join("script.notes");
    let script = format!(
        "{}\n{}\n{}\n{}\n",
        r#"{"command":"note.add","params":{"title":"one"}}"#,
        r#"{"command":"note.add","params":{"title":"two"}}"#,
        r#"{"command":"app.undo"}"#,
        json!({"command": "file.save_as", "params": {"path": file.to_string_lossy()}}),
    );
    let (code, out) = cli(dir.path(), &["run", "-"], &script);
    assert_eq!(code, 0, "{out}");
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(saved.contains("\"one\"") && !saved.contains("\"two\""), "{saved}");

    let (code, out) = cli(dir.path(), &["exec", "note.edit", r#"{"id": 999}"#], "");
    assert_eq!(code, 1, "{out}");
    assert!(out.contains(r#""ok":false"#));
}

#[test]
fn notes_cli_serves_mcp() {
    let dir = tempfile::tempdir().unwrap();
    let msgs = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "execute", "arguments": {"command": "note.add", "params": {"title": "via mcp"}}}}),
    ];
    let stdin: String = msgs.iter().map(|m| format!("{m}\n")).collect();
    let (code, out) = cli(dir.path(), &["mcp"], &stdin);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(r#"\"count\":1"#), "{out}");
    assert!(out.contains(r#""isError":false"#), "{out}");
}

#[test]
fn the_running_window_answers_on_its_tcp_control_port() {
    let dir = tempfile::tempdir().unwrap();
    let dirs = AppDirs::under(dir.path());
    dirs.ensure().unwrap();
    let rt: Runtime<Notes> = notes::app().dirs(dirs.clone()).control_port(0).runtime();
    let mut d = Driver::new(rt);
    d.settle(10);
    let port = d.runtime.control_port().expect("serving");

    let done = Arc::new(AtomicBool::new(false));
    let result = Arc::new(Mutex::new(None));
    let (done2, result2, key_dir) = (Arc::clone(&done), Arc::clone(&result), dirs.config.clone());
    let agent = std::thread::spawn(move || {
        let r = (|| -> Result<(Value, Value), String> {
            let mut c = Client::connect(port, &key_dir).map_err(|e| e.to_string())?;
            c.set_timeout(Duration::from_secs(10));
            let added = c.execute("note.add", json!({"title": "over tcp"})).map_err(|e| e.to_string())?;
            let ins = c.call("ui.inspect", json!({})).map_err(|e| e.to_string())?;
            Ok((added, ins))
        })();
        *result2.lock().unwrap() = Some(r);
        done2.store(true, Ordering::SeqCst);
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
        d.advance(0.02); // frames run only when the server wakes the loop
        std::thread::sleep(Duration::from_millis(2));
    }
    agent.join().unwrap();
    let (added, ins) = result.lock().unwrap().take().expect("finished").unwrap();
    assert_eq!(added["count"], json!(1));
    assert_eq!(ins["document"]["dirty"], json!(true));

    // a wrong key is refused
    assert!(Client::connect_with_key(port, "0".repeat(64)).and_then(|mut c| c.call("app.ping", json!({}))).is_err());
}
