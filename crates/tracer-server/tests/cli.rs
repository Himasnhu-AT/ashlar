#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]
mod common;

use common::{TestServer, config_in, envelope, item};
use tracer_proto::{ItemBody, SessionStart};
use tracer_server::cli::{Command, execute, parse};
use tracer_server::config::Config;

fn run(config: &Config, words: &[&str], stdin: &str) -> Result<String, String> {
    let args: Vec<String> = words.iter().map(|word| word.to_string()).collect();
    let command = parse(&args).map_err(|error| error.to_string())?;
    let mut out = Vec::new();
    let mut input = std::io::Cursor::new(stdin.as_bytes().to_vec());
    execute(command, config, &mut out, &mut input).map_err(|error| error.to_string())?;
    Ok(String::from_utf8(out).unwrap())
}

fn printed_key(output: &str) -> String {
    output.lines().find_map(|line| line.strip_prefix("write key: ")).expect("a key line").to_string()
}

#[test]
fn app_create_then_ingest_with_the_printed_key_works() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path());
    let key = printed_key(&run(&config, &["app", "create", "demo", "Demo App"], "").unwrap());
    assert!(key.len() >= 32);
    assert!(run(&config, &["app", "list"], "").unwrap().contains("demo\tDemo App"));
    assert!(run(&config, &["app", "create", "demo", "Again"], "").unwrap_err().contains("already exists"));

    let server = TestServer::start(config.clone());
    let body = serde_json::to_vec(&envelope("i1", "s1", vec![item("a", ItemBody::SessionStart(SessionStart::default()))])).unwrap();
    assert_eq!(server.post_raw("/v1/ingest", Some(&key), false, &body).status, 200);

    // A second key works, and a revoked one stops working while the server is running.
    let second = printed_key(&run(&config, &["key", "create", "demo"], "").unwrap());
    assert_ne!(second, key);
    assert_eq!(server.post_raw("/v1/ingest", Some(&second), false, &body).status, 200);
    assert_eq!(run(&config, &["key", "revoke", &key], "").unwrap().trim(), "revoked");
    assert_eq!(server.post_raw("/v1/ingest", Some(&key), false, &body).status, 401);
    assert_eq!(server.post_raw("/v1/ingest", Some(&second), false, &body).status, 200);
    assert!(run(&config, &["key", "revoke", &key], "").is_err());
    assert!(run(&config, &["key", "create", "nope"], "").is_err());
}

#[test]
fn admin_create_reads_the_password_from_settings_or_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = config_in(dir.path());
    assert!(run(&config, &["admin", "create", "alice"], "short\n").unwrap_err().contains("at least 8"));
    run(&config, &["admin", "create", "alice"], "a long enough password\n").unwrap();
    config.admin_password = Some("another good password".into());
    run(&config, &["admin", "create", "bob"], "").unwrap();
    assert!(run(&config, &["admin", "create", "bob"], "").unwrap_err().contains("taken"));

    let store = tracer_server::store::SqliteStore::open(&config.db_path()).unwrap();
    use tracer_server::store::Store;
    let (_, hash) = store.admin_password_hash("alice").unwrap().unwrap();
    assert!(hash.starts_with("$argon2"));
    assert!(tracer_server::cli::verify_password("a long enough password", &hash));
    assert!(!tracer_server::cli::verify_password("a long enough passwore", &hash));
    assert!(!hash.contains("long enough"));
}

#[test]
fn prune_and_rollup_run_from_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path());
    assert!(run(&config, &["prune"], "").unwrap().starts_with("removed 0 events"));
    assert!(run(&config, &["rollup", "--days", "5"], "").unwrap().contains("5 days"));
    assert_eq!(parse(&["serve".to_string()]).unwrap(), Command::Serve);
}
