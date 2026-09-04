#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The CLI end to end against the demo app (a real control port, deterministic), and `open`/`close`
//! against the real binary.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use ashlar_debug::demo::{self, Running};
use ashlar_debug::{Env, Exit, Io, run};
use serde_json::{Value, json};

static N: AtomicU32 = AtomicU32::new(0);

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ashlar-debug-cli-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A demo app whose key sits where an app keeps its settings, and a private state folder.
struct Fixture {
    env: Env,
    root: PathBuf,
    demo: Running,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = temp(tag);
        let key_dir = root.join("config/org.example.Demo");
        let demo = demo::start(0, &key_dir).unwrap();
        let env = Env { home: root.join("home"), config_roots: vec![root.join("config")], session: "default".into(), key_dir: None };
        let f = Self { env, root, demo };
        f.ok(&["connect", &f.demo.port.to_string()]);
        f
    }

    fn run(&self, args: &[&str]) -> (Exit, String, String) {
        cli(&self.env, args, "")
    }

    /// Run and expect success; stdout.
    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.run(args);
        assert_eq!(code, Exit::Ok, "{args:?}\nstdout: {out}\nstderr: {err}");
        out
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        let (_, out, _) = self.run(&a);
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{args:?}: {e}: {out}"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn cli(env: &Env, args: &[&str], stdin: &str) -> (Exit, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut input = stdin.as_bytes();
    let code = run(args.iter().map(|s| s.to_string()).collect(), env, &mut Io { input: &mut input, out: &mut out, err: &mut err });
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

const SNAPSHOT: &str = r#"- window "Demo" [ref=e1]
  - textbox "Title" [ref=e2]
  - button "Save" [ref=e3]
  - checkbox "Wrap lines" [ref=e4]
  - button "Delete" [ref=e5] [disabled]
  - button "Load" [ref=e6]
  - label "Ready" [ref=e7]
  - list "Items" [ref=e8]
    - listitem "Item 1" [ref=e9]
    - listitem "Item 2" [ref=e10]
    - listitem "Item 3" [ref=e11]
"#;

#[test]
fn snapshot_prints_the_outline_and_json_carries_the_elements() {
    let f = Fixture::new("snapshot");
    assert_eq!(f.ok(&["snapshot"]), SNAPSHOT);
    assert_eq!(
        f.ok(&["snapshot", "-i", "--scope", "id=items"]),
        "- listitem \"Item 1\" [ref=e9]\n- listitem \"Item 2\" [ref=e10]\n- listitem \"Item 3\" [ref=e11]\n"
    );
    assert_eq!(f.ok(&["snapshot", "--depth", "0"]), "- window \"Demo\" [ref=e1]\n");
    let v = f.json(&["snapshot", "-i"]);
    assert_eq!(v["ok"], true);
    let els = v["result"]["elements"].as_array().unwrap();
    assert_eq!(els.len(), 8);
    assert_eq!(
        els[1],
        json!({"ref": "e3", "id": "file.save", "role": "button", "name": "Save", "enabled": true, "focused": false, "visible": true, "rect": [224.0, 16.0, 56.0, 24.0], "parent": "e1"})
    );
    assert!(v["result"]["generation"].as_u64().unwrap() >= 3, "generations count up");
}

#[test]
fn acting_on_refs_and_reading_back() {
    let f = Fixture::new("act");
    assert_eq!(f.ok(&["fill", "@e2", "Groceries"]), "filled @e2\n");
    assert_eq!(f.ok(&["click", "button=Save"]), "clicked button=Save\n");
    assert_eq!(f.ok(&["get", "text", "id=status"]), "Saved: Groceries\n");
    assert_eq!(f.ok(&["get", "value", "e2"]), "Groceries\n");
    assert_eq!(f.ok(&["get", "enabled", "@e5"]), "true\n");
    assert_eq!(f.json(&["get", "rect", "@e3"]), json!({"ok": true, "result": [224.0, 16.0, 56.0, 24.0]}));
    f.ok(&["click", "@e4"]);
    assert_eq!(f.ok(&["get", "checked", "text=Wrap lines"]), "true\n");
    assert_eq!(f.ok(&["dblclick", "@e4"]), "double-clicked @e4\n");
    assert_eq!(f.ok(&["get", "checked", "@e4"]), "true\n");
    f.ok(&["focus", "@e2"]);
    f.ok(&["type", " list"]);
    f.ok(&["press", "Mod+S"]);
    assert_eq!(f.ok(&["get", "text", "id=status"]), "Saved: Groceries list\n");
    f.ok(&["hover", "@e3"]);
    f.ok(&["scroll", "down", "300"]);
    f.ok(&["scroll", "@e8", "up"]);
    f.ok(&["scroll", "--dx", "5", "--dy", "-5"]);
    f.ok(&["scrollintoview", "@e11"]);
    f.ok(&["drag", "@e9", "@e11", "--steps", "4"]);
    f.ok(&["highlight", "@e3", "--ms", "200"]);
    f.ok(&["click", "120,60", "--button", "right", "--mods", "Shift+Mod"]);
    assert_eq!(f.ok(&["exec", "file.save"]), "{\"status\":\"Saved: Groceries list\"}\n");
    assert!(f.ok(&["commands", "file"]).starts_with("file.save  Save  [Mod+S]\nfile.load  Load\n"));
    assert_eq!(f.ok(&["menu", "view.wrap"]), "invoked view.wrap\n");
    assert!(f.ok(&["logs", "-n", "3"]).contains("INFO  demo:"));
    assert!(f.ok(&["stats"]).contains("\"requests\""));
    assert_eq!(f.ok(&["devtools", "open"]), "devtools open\n");
    assert_eq!(f.ok(&["devtools", "state"]), "devtools open\n");
    assert_eq!(f.ok(&["devtools"]), "devtools closed\n");
    assert_eq!(f.ok(&["call", "app.ping"]), "\"pong\"\n");
    assert_eq!(f.ok(&["call", "ui.get", r#"{"target":{"id":"title"},"what":"value"}"#]), "{\n  \"value\": \"Groceries list\"\n}\n");
}

#[test]
fn failures_say_why_with_exit_codes() {
    let f = Fixture::new("fail");
    let (code, out, err) = f.run(&["click", "@e99"]);
    assert_eq!((code, out.as_str()), (Exit::Failed, ""));
    assert!(err.starts_with("error: stale_ref: no element e99"), "{err}");
    assert!(err.contains("hint: refs come from the latest snapshot"), "{err}");
    let v = f.json(&["click", "@e99"]);
    assert_eq!(v["ok"], false);
    assert_eq!(v["detail"]["kind"], "stale_ref");
    // The app refuses: a disabled button.
    let (code, _, err) = f.run(&["click", "button=Delete"]);
    assert_eq!(code, Exit::Failed);
    assert!(err.contains("`Delete` is disabled"), "{err}");
    // Usage: exit 2, the usage line on stderr.
    for args in [
        &["click"][..],
        &["click", "Save"],
        &["get", "colour", "@e1"],
        &["nope"],
        &["--port", "x", "status"],
        &["wait"],
        &["wait", "--idle", "--text", "x"],
    ] {
        let (code, _, err) = f.run(args);
        assert_eq!(code, Exit::Usage, "{args:?}: {err}");
        assert!(err.contains("usage: ashlar-debug"), "{args:?}: {err}");
    }
    let v = f.json(&["click", "Save"]);
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().unwrap().contains("not a target"));
    assert_eq!(f.run(&[]).0, Exit::Usage);
    assert!(f.ok(&["--help"]).contains("snapshot [-i]"));
    assert_eq!(f.ok(&["click", "--help"]), "usage: ashlar-debug click <T> [--button primary|secondary|middle] [--count N] [--mods Shift+Mod]\n");
    // Another session that does not exist.
    let (code, _, err) = f.run(&["--session", "other", "snapshot"]);
    assert_eq!(code, Exit::Failed);
    assert!(err.contains("no session `other`") && err.contains("ashlar-debug open"), "{err}");
    assert_eq!(f.run(&["--session", "../x", "snapshot"]).0, Exit::Usage);
}

#[test]
fn waits_for_text_elements_and_idle() {
    let f = Fixture::new("wait");
    f.ok(&["click", "button=Load"]);
    assert_eq!(f.ok(&["get", "text", "id=status"]), "Loading…\n");
    let (code, _, err) = f.run(&["wait", "--text", "Loaded", "--timeout", "50"]);
    assert_eq!(code, Exit::Failed);
    assert!(err.contains("timeout"), "{err}");
    let out = f.ok(&["wait", "--text", "Loaded", "--timeout", "3000"]);
    assert!(out.starts_with("waited "), "{out}");
    f.ok(&["wait", "--idle"]);
    f.ok(&["wait", "text=Item 4"]);
    f.ok(&["wait", "--ref", "@e12"]);
    f.ok(&["wait", "--gone", "text=Item 9", "--timeout", "100"]);
    f.ok(&["fill", "@e2", "x"]);
    f.ok(&["click", "@e3"]);
    f.ok(&["wait", "--enabled", "button=Delete"]);
    assert_eq!(f.ok(&["wait", "20"]), "waited 20 ms\n");
}

#[test]
fn screenshots_are_written_as_png_files() {
    let f = Fixture::new("shot");
    let path = f.root.join("shots/full.png");
    let out = f.ok(&["screenshot", path.to_str().unwrap()]);
    assert_eq!(out, format!("{}\n", path.display()));
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    // Annotated: the refs drawn are listed.
    let out = f.ok(&["screenshot", path.to_str().unwrap(), "--annotate"]);
    assert!(out.contains("\n@e3 [224.0,16.0,56.0,24.0]\n"), "{out}");
    // One element, padded; the default path is in the state folder.
    let v = f.json(&["screenshot", "--target", "@e3", "--pad", "4", "--highlight", "@e3"]);
    assert_eq!((v["result"]["width"].clone(), v["result"]["height"].clone()), (json!(64), json!(32)));
    let p = PathBuf::from(v["result"]["path"].as_str().unwrap());
    assert!(p.starts_with(&f.env.home) && p.exists(), "{p:?}");
}

#[test]
fn batch_runs_a_ui_test_and_stops_at_the_first_failure() {
    let f = Fixture::new("batch");
    let script = f.root.join("test.txt");
    std::fs::write(
        &script,
        r#"# a small UI test
snapshot -i
fill id=title "Groceries"
click button=Save            # saves
wait --text "Saved"
assert text id=status "Saved: Groceries"
expect contains id=status "Groceries"
assert enabled button=Delete
assert disabled button=Nope  # not there: fails below if reached
"#,
    )
    .unwrap();
    let (code, out, err) = f.run(&["batch", script.to_str().unwrap()]);
    assert_eq!(code, Exit::Failed, "{out}{err}");
    assert!(err.contains(":9: assert disabled button=Nope\n     expected button=Nope to be disabled, but it is not there"), "{err}");
    assert!(err.contains("hint: 7 step(s) passed before line 9"), "{err}");

    std::fs::write(
        &script,
        "click button=Load\nwait --text Loaded\nassert count role=listitem >= 4\nassert count role=listitem == 4\nassert exists role=button name~=Sav\n\
         assert missing role=button name=Nope\nassert hidden text=Gone\nassert visible @e3\nassert unchecked @e4\nclick @e4\nassert checked @e4\n\
         assert focused @e3\n",
    )
    .unwrap();
    let (code, _, err) = f.run(&["batch", script.to_str().unwrap()]);
    assert_eq!(code, Exit::Failed);
    assert!(err.contains(":12: assert focused @e3\n     expected @e3 to be focused, but focused is false"), "{err}");
    std::fs::write(&script, "echo a\nassert exists role=nothing\nclick @e3\n").unwrap();
    let v = f.json(&["batch", script.to_str().unwrap()]);
    assert_eq!(v["ok"], false);
    assert_eq!(v["detail"]["line"], 2);
    assert_eq!(v["detail"]["passed"], 1);
    assert_eq!(v["detail"]["steps"].as_array().unwrap().len(), 2);
    assert_eq!(
        v["detail"]["steps"][1],
        json!({"line": 2, "command": "assert exists role=nothing", "ok": false, "error": "expected an element matching role=nothing, found none"})
    );

    // From stdin, all passing, verbose.
    let (code, out, err) = cli(&f.env, &["batch", "-", "--verbose"], "echo hello\nfocus @e2\nassert focused @e2\nsleep 1\nget text id=status\n");
    assert_eq!(code, Exit::Ok, "{err}");
    assert!(out.starts_with("hello\nok      1  echo hello\nok      2  focus @e2\n            focused @e2\n"), "{out}");
    assert!(out.ends_with("batch: 5 steps passed (-)\n"), "{out}");
    // Bad lines are failures with their line number.
    for (script, want) in [
        ("assert text @e3\n", "assert text needs the expected text"),
        ("assert count role=button 3\n", "assert count <selector…> <op> <n>"),
        ("assert sparkles @e3\n", "unknown assertion `sparkles`"),
        ("fill @e2 \"open\n", "unterminated"),
        ("batch -\n", "cannot run another batch"),
        ("click Save\n", "not a target"),
    ] {
        let (code, _, err) = cli(&f.env, &["batch", "-"], script);
        assert_eq!(code, Exit::Failed, "{script}");
        assert!(err.contains(":1:") && err.contains(want), "{script}: {err}");
    }
}

#[test]
fn a_trace_records_every_action() {
    let f = Fixture::new("trace");
    let dir = f.root.join("trace");
    assert!(f.ok(&["trace", "start", dir.to_str().unwrap()]).starts_with("tracing to "));
    f.ok(&["fill", "@e2", "a b"]);
    f.ok(&["snapshot"]); // not an action: not recorded
    let _ = f.run(&["click", "@e99"]); // failures are recorded too
    f.ok(&["click", "@e3"]);
    assert!(f.ok(&["status"]).contains("tracing to"));
    assert!(f.ok(&["trace", "stop"]).starts_with("3 steps in "));
    f.ok(&["click", "@e3"]);
    let mut files: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    files.sort();
    assert_eq!(files, ["001-fill.png", "001-fill.txt", "002-click.png", "002-click.txt", "003-click.png", "003-click.txt", "steps.jsonl"]);
    let steps: Vec<Value> = std::fs::read_to_string(dir.join("steps.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(steps[0]["command"], "fill @e2 \"a b\"");
    assert_eq!((steps[1]["ok"].clone(), steps[2]["ok"].clone()), (json!(false), json!(true)));
    assert!(steps[1]["error"].as_str().unwrap().contains("stale_ref"));
    assert!(std::fs::read_to_string(dir.join("003-click.txt")).unwrap().contains("label \"Saved: a b\""));
    assert_eq!(f.run(&["trace", "stop"]).0, Exit::Failed);
}

#[test]
fn port_and_app_name_reach_an_app_without_a_session() {
    let f = Fixture::new("direct");
    let port = f.demo.port.to_string();
    let none = Env { session: "none".into(), ..f.env.clone() };
    // The key is found in the settings folders by port, or by app name.
    let (code, out, err) = cli(&none, &["--port", &port, "get", "text", "@e3"], "");
    assert_eq!((code, out.as_str()), (Exit::Ok, "Save\n"), "{err}");
    let (code, out, _) = cli(&none, &["--app", "demo", "get", "text", "@e6"], "");
    assert_eq!((code, out.as_str()), (Exit::Ok, "Load\n"));
    let (code, _, err) = cli(&none, &["--app", "paint", "snapshot"], "");
    assert_eq!(code, Exit::Failed);
    assert!(err.contains("no running app matching `paint`"), "{err}");
    let key_dir = f.root.join("config/org.example.Demo");
    let (code, _, _) = cli(&none, &["--port", &port, "--key-dir", key_dir.to_str().unwrap(), "snapshot"], "");
    assert_eq!(code, Exit::Ok);
    let (code, _, err) = cli(&none, &["--port", &port, "--key-dir", "/nonexistent", "snapshot"], "");
    assert_eq!(code, Exit::Failed);
    assert!(err.contains("no control key"), "{err}");
    let status = f.ok(&["status"]);
    assert!(status.starts_with(&format!("demo {} on 127.0.0.1:{port}\nsession `default`\nmethods: app.ping", env!("CARGO_PKG_VERSION"))), "{status}");
    assert!(f.ok(&["sessions"]).contains("running"));
    // close on a connected (not launched) app forgets it and leaves the app running.
    assert_eq!(f.ok(&["close"]), "forgot session `default`\n");
    assert!(!f.demo.finished());
    assert_eq!(f.ok(&["sessions"]), "no sessions\n");
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_ashlar-debug")
}

fn wait_gone(pid_alive: impl Fn() -> bool) -> bool {
    for _ in 0..100 {
        if !pid_alive() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

#[test]
fn open_launches_an_app_and_close_quits_it() {
    let root = temp("open");
    let env = Env { home: root.join("home"), config_roots: vec![root.join("config")], session: "default".into(), key_dir: None };
    let (code, out, err) = cli(&env, &["--session", "s1", "open", binary(), "--", "demo"], "");
    assert_eq!(code, Exit::Ok, "{err}");
    assert!(out.starts_with(&format!("opened {} (pid ", binary())) && out.ends_with("as session `s1`\n"), "{out}");
    let rec: Value = serde_json::from_slice(&std::fs::read(env.session_file("s1")).unwrap()).unwrap();
    let port = rec["port"].as_u64().unwrap();
    assert_eq!(rec["launched"], true);
    assert!(Path::new(rec["key_dir"].as_str().unwrap()).starts_with(env.session_dir("s1")), "the key goes to the session folder");
    let (_, out, _) = cli(&env, &["--session", "s1", "get", "text", "@e3"], "");
    assert_eq!(out, "Save\n");
    // A second open of the same session refuses.
    let (code, _, err) = cli(&env, &["--session", "s1", "open", binary(), "--", "demo"], "");
    assert_eq!(code, Exit::Failed);
    assert!(err.contains("session `s1` is open"), "{err}");
    // The launched app's output was captured.
    assert!(std::fs::read_to_string(rec["log"].as_str().unwrap()).unwrap().contains(&format!("127.0.0.1:{port}")));
    let (code, out, _) = cli(&env, &["--session", "s1", "close"], "");
    assert_eq!((code, out.as_str()), (Exit::Ok, "closed session `s1` (the app quit)\n"));
    let addr = format!("127.0.0.1:{port}");
    assert!(wait_gone(|| std::net::TcpStream::connect(&addr).is_ok()), "the app stopped listening");
    assert!(!env.session_file("s1").exists() && !env.session_dir("s1").exists());
    // An app that exits at once, or never answers: a clear error, the output included.
    let (code, _, err) = cli(&env, &["open", binary(), "--", "nonsense-verb"], "");
    assert_eq!(code, Exit::Failed);
    assert!(err.contains("exited") && err.contains("unknown command `nonsense-verb`"), "{err}");
    let (code, _, err) = cli(&env, &["open", "/nonexistent/app"], "");
    assert_eq!(code, Exit::Failed);
    assert!(err.contains("cannot start /nonexistent/app"), "{err}");
    let _ = std::fs::remove_dir_all(&root);
}
