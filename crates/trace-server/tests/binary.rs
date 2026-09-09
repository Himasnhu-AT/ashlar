//! The real binary, driven through its command line: `main`, the logger, argument errors and `serve`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::too_many_arguments, clippy::type_complexity)]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn binary(dir: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracer-server"));
    command
        .env("TRACER_DATA_DIR", dir)
        .env_remove("TRACER_BIND")
        .env_remove("TRACER_ADMIN_PASSWORD")
        .env_remove("TRACER_RETENTION_DAYS")
        .env_remove("TRACER_RATE_PER_MIN");
    command
}

#[test]
fn help_and_unknown_commands() {
    let dir = tempfile::tempdir().unwrap();
    let help = binary(dir.path()).arg("help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("usage: tracer-server"));
    let none = binary(dir.path()).output().unwrap();
    assert!(none.status.success(), "no arguments prints the usage");

    let bogus = binary(dir.path()).arg("frobnicate").output().unwrap();
    assert_eq!(bogus.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bogus.stderr).contains("unknown command"));
    let bad_days = binary(dir.path()).args(["rollup", "--days", "0"]).output().unwrap();
    assert_eq!(bad_days.status.code(), Some(2));
}

#[test]
fn bad_settings_stop_the_command_and_name_the_variable() {
    let dir = tempfile::tempdir().unwrap();
    let out = binary(dir.path()).env("TRACER_RETENTION_DAYS", "lots").arg("prune").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("TRACER_RETENTION_DAYS"));
    let out = binary(dir.path()).env("TRACER_BIND", "not an address").arg("serve").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn the_commands_work_end_to_end_through_the_binary() {
    let dir = tempfile::tempdir().unwrap();
    let created = binary(dir.path()).args(["app", "create", "demo", "Demo"]).output().unwrap();
    assert!(created.status.success());
    let key = String::from_utf8_lossy(&created.stdout).lines().find_map(|l| l.strip_prefix("write key: ").map(str::to_string)).unwrap();
    let listed = binary(dir.path()).args(["app", "list"]).output().unwrap();
    assert!(String::from_utf8_lossy(&listed.stdout).contains("demo"));
    assert!(binary(dir.path()).args(["key", "create", "demo"]).output().unwrap().status.success());
    assert!(binary(dir.path()).args(["key", "revoke", &key]).output().unwrap().status.success());
    assert_eq!(binary(dir.path()).args(["key", "revoke", &key]).output().unwrap().status.code(), Some(2), "already revoked");
    // The admin password comes from the environment for this command only.
    let admin = binary(dir.path()).env("TRACER_ADMIN_PASSWORD", "a long enough password").args(["admin", "create", "root"]).output().unwrap();
    assert!(admin.status.success(), "{}", String::from_utf8_lossy(&admin.stderr));
    let mut piped = binary(dir.path()).args(["admin", "create", "second"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    piped.stdin.take().unwrap().write_all(b"another long password\n").unwrap();
    assert!(piped.wait_with_output().unwrap().status.success());
    assert!(binary(dir.path()).arg("prune").output().unwrap().status.success());
    assert!(binary(dir.path()).args(["rollup", "--days", "2"]).output().unwrap().status.success());
}

#[test]
fn serve_logs_a_warning_for_a_public_bind_without_a_proxy_and_stops_on_sigterm() {
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let child = binary(dir.path()).env("TRACER_BIND", format!("0.0.0.0:{port}")).arg("serve").stderr(Stdio::piped()).spawn().unwrap();
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let started = Instant::now();
    while !agent.get(format!("http://127.0.0.1:{port}/v1/health")).call().is_ok_and(|r| r.status() == 200) {
        assert!(started.elapsed() < Duration::from_secs(20), "no health answer");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap().success());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{:?}", out.status);
    let log = String::from_utf8_lossy(&out.stderr);
    assert!(log.contains("listening on"), "{log}");
    assert!(log.contains("TRACER_TRUST_PROXY"), "the public-bind warning is missing: {log}");
    assert!(log.contains("shut down"), "{log}");
}
