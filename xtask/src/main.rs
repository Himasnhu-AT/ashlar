//! `cargo xtask <task>`: the repo's gates. `cargo xtask ci` is the one every commit passes.
//!
//! - `ci [--no-wait]`: fmt check, names, layers, deny-panics and deps (bans and sources, when
//!   cargo-deny is installed), then clippy, the wasm check and the tests side by side. Clippy and wasm build in
//!   their own target dirs with their output captured (shown only when they fail). Waits while another
//!   checkout is compiling unless `--no-wait`.
//! - `fmt`, `names`, `layers`, `deny-panics`, `clippy`, `test`, `wasm`: one step of `ci`.
//! - `msrv`: `cargo check` on the workspace's `rust-version` toolchain.
//! - `advisories`: cargo-deny on advisories, bans and sources (needs the network).
//! - `bench [--filter T] [--repeat N] [--compare FILE] [--threshold PCT]`: see `bench.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod bench;
mod names;
mod panics;

/// Rank of each workspace crate. A crate may depend only on crates of a lower rank.
/// 0–1 core (headless, wasm-clean) · 2–4 services · 5 ui · 6 runtime · 7 umbrella · 8 examples.
/// Nothing below 5 may depend on a UI crate. Keep docs/architecture.md in sync.
const LAYERS: &[(&str, u8)] = &[
    ("ashlar-command", 0),
    ("ashlar-fs", 0),
    ("ashlar-jobs", 0),
    ("ashlar-log", 0),
    ("ashlar-store", 1),
    ("ashlar-native", 2),
    ("ashlar-control", 2),
    ("ashlar-mcp", 3),
    ("ashlar-cli", 4),
    ("ashlar-debug", 4),
    ("ashlar-print", 2),
    ("ashlar-ui", 5),
    ("ashlar-app", 6),
    ("ashlar", 7),
    ("ashlar-example-gallery", 8),
    ("ashlar-example-notes", 8),
    ("xtask", 9),
];
/// Crates no rank 0–4 crate may depend on (directly).
const UI_ONLY: &[&str] = &["egui", "eframe", "winit", "egui-wgpu", "egui_kittest"];
/// Crates of this rank or lower must build for `wasm32-unknown-unknown`.
const WASM_MAX_RANK: u8 = 1;

fn layer(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

/// The cargo that runs this xtask (so `cargo +toolchain xtask` stays on that toolchain).
pub fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())
}

/// The workspace root (the parent of `xtask/`).
pub fn root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(manifest).to_path_buf()
}

fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root().join("target"))
}

fn metadata() -> Result<serde_json::Value, String> {
    let out = Command::new(cargo())
        .current_dir(root())
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|e| format!("cargo metadata: {e}"))?;
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata json: {e}"))
}

fn layers() -> Result<(), String> {
    let meta = metadata()?;
    let mut errors = Vec::new();
    for pkg in meta["packages"].as_array().into_iter().flatten() {
        let name = pkg["name"].as_str().unwrap_or_default();
        let Some(l) = layer(name) else {
            errors.push(format!("{name}: not assigned a layer (add it to xtask LAYERS and docs/architecture.md)"));
            continue;
        };
        for dep in pkg["dependencies"].as_array().into_iter().flatten() {
            if dep["kind"].as_str() == Some("dev") {
                continue;
            }
            let d = dep["name"].as_str().unwrap_or_default();
            if l < 5 && UI_ONLY.contains(&d) {
                errors.push(format!("{name} (L{l}) depends on UI crate {d}"));
            }
            if let Some(dl) = layer(d)
                && dl >= l
            {
                errors.push(format!("{name} (L{l}) depends on {d} (L{dl}): only lower layers allowed"));
            }
        }
    }
    let doc = std::fs::read_to_string(root().join("docs").join("architecture.md")).unwrap_or_default();
    errors.extend(doc_out_of_sync(&doc).into_iter().map(|(name, l)| format!("docs/architecture.md: the ranks table has no `{name}` at rank {l}")));
    if errors.is_empty() {
        println!("layers: ok");
        Ok(())
    } else {
        Err(format!("layers:\n{}", errors.join("\n")))
    }
}

/// The `LAYERS` entries missing from the ranks table of docs/architecture.md (`| <rank> | `name` …`).
fn doc_out_of_sync(doc: &str) -> Vec<(&'static str, u8)> {
    LAYERS
        .iter()
        .filter(|(name, l)| !doc.lines().any(|line| line.starts_with(&format!("| {l} |")) && line.contains(&format!("`{name}`"))))
        .copied()
        .collect()
}

fn cmd(args: &[&str]) -> Command {
    let mut c = Command::new(cargo());
    c.current_dir(root()).args(args);
    c
}

fn run(c: &mut Command) -> Result<(), String> {
    eprintln!("$ {}", show(c));
    let ok = c.status().map_err(|e| format!("{}: {e}", show(c)))?.success();
    if ok { Ok(()) } else { Err(format!("failed: {}", show(c))) }
}

fn show(c: &Command) -> String {
    let args: Vec<_> = c.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    format!("{} {}", Path::new(c.get_program()).file_name().map(|p| p.to_string_lossy()).unwrap_or_default(), args.join(" "))
}

/// Run a step with its output captured: one summary line when it passes, the whole output when it fails.
fn run_captured(name: &str, c: &mut Command) -> Result<String, String> {
    let line = format!("$ {}", show(c));
    let started = std::time::Instant::now();
    let out = c.output().map_err(|e| format!("{name}: {e}"))?;
    let secs = started.elapsed().as_secs();
    if out.status.success() {
        Ok(format!("{name}: ok in {secs} s"))
    } else {
        Err(format!("{line}\n{}{}\n{name}: FAILED after {secs} s", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }
}

fn fmt() -> Result<(), String> {
    run(&mut cmd(&["fmt", "--all", "--check"]))
}

fn clippy_cmd() -> Command {
    cmd(&["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])
}

fn test_cmd() -> Command {
    cmd(&["test", "--workspace"])
}

/// `cargo check --target wasm32-unknown-unknown` on every crate of rank 0–1, and how many there are.
fn wasm_cmd() -> Result<(Command, usize), String> {
    let meta = metadata()?;
    let mut c = cmd(&["check", "--target", "wasm32-unknown-unknown"]);
    let mut n = 0;
    for pkg in meta["packages"].as_array().into_iter().flatten() {
        let name = pkg["name"].as_str().unwrap_or_default();
        if layer(name).is_some_and(|l| l <= WASM_MAX_RANK) {
            c.args(["-p", name]);
            n += 1;
        }
    }
    Ok((c, n))
}

/// `cargo deny check` when cargo-deny is installed; `None` (with a note) when it isn't.
fn deny_cmd(checks: &[&str]) -> Option<Command> {
    let installed = Command::new(cargo()).args(["deny", "--version"]).output().is_ok_and(|o| o.status.success());
    if !installed {
        eprintln!("deps: skipped, cargo-deny isn't installed (cargo install cargo-deny --locked)");
        return None;
    }
    let mut c = cmd(&["deny", "--log-level", "error", "check"]);
    c.args(checks);
    Some(c)
}

fn ci(args: &[String]) -> Result<(), String> {
    let started = std::time::Instant::now();
    fmt()?;
    let root = root();
    let mut failures: Vec<String> = Vec::new();
    // Quick source checks: shown now, before the long builds, and repeated in the final result.
    for r in [names::check(&root), layers(), panics::check(&root)] {
        if let Err(e) = r {
            failures.push(e);
        }
    }
    // Offline checks only; advisories need the network and run in `advisories` and on CI.
    if let Some(mut c) = deny_cmd(&["bans", "sources"]) {
        match run_captured("deps", &mut c) {
            Ok(line) => println!("{line}"),
            Err(e) => failures.push(e),
        }
    }
    for e in &failures {
        eprintln!("{e}\n");
    }
    if !args.iter().any(|a| a == "--no-wait") {
        wait_for_other_builds(&root);
    }
    let target = target_dir();
    let (clippy_dir, wasm_dir) = (target.join("ci-clippy"), target.join("ci-wasm"));
    std::thread::scope(|scope| {
        let clippy = scope.spawn(|| run_captured("clippy", clippy_cmd().env("CARGO_TARGET_DIR", &clippy_dir)));
        let wasm = scope.spawn(|| {
            let (mut c, n) = wasm_cmd()?;
            run_captured(&format!("wasm ({n} crates)"), c.env("CARGO_TARGET_DIR", &wasm_dir))
        });
        let t = std::time::Instant::now();
        let tests = run(&mut test_cmd()).map(|()| format!("tests: ok in {} s", t.elapsed().as_secs()));
        let joined = |h: std::thread::ScopedJoinHandle<'_, Result<String, String>>| h.join().unwrap_or_else(|_| Err("a gate step panicked".into()));
        for r in [tests, joined(clippy), joined(wasm)] {
            match r {
                Ok(line) => println!("{line}"),
                Err(e) => failures.push(e),
            }
        }
    });
    println!("ci: {} in {} s", if failures.is_empty() { "ok" } else { "FAILED" }, started.elapsed().as_secs());
    if failures.is_empty() { Ok(()) } else { Err(failures.join("\n\n")) }
}

/// Wait (at most half an hour) while another checkout is compiling, so two gates take turns instead of
/// competing for cores. `--no-wait` skips it.
fn wait_for_other_builds(root: &Path) {
    let started = std::time::Instant::now();
    let mut told = false;
    loop {
        let others = other_builds(root);
        if others.is_empty() || started.elapsed() > std::time::Duration::from_secs(1800) {
            if told {
                eprintln!("ci: waited {} s for other builds", started.elapsed().as_secs());
            }
            return;
        }
        if !told {
            eprintln!(
                "ci: waiting for {} compiler process(es) of another checkout to finish (--no-wait to skip), e.g. {}",
                others.len(),
                others.first().map(String::as_str).unwrap_or("")
            );
            told = true;
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

/// rustc processes that are not building this checkout (none on Windows, where `ps` is missing).
fn other_builds(root: &Path) -> Vec<String> {
    let Ok(out) = Command::new("ps").args(["-axo", "pid=,command="]).output() else { return Vec::new() };
    let root = root.to_string_lossy();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains("rustc") && l.contains("--crate-name") && !l.contains(root.as_ref()))
        .map(|l| l.chars().take(120).collect())
        .collect()
}

/// The workspace `rust-version`.
fn msrv_version() -> Result<String, String> {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml")).map_err(|e| format!("Cargo.toml: {e}"))?;
    manifest
        .lines()
        .find_map(|l| l.trim().strip_prefix("rust-version"))
        .and_then(|rest| rest.split('"').nth(1))
        .map(str::to_owned)
        .ok_or_else(|| "Cargo.toml: no workspace rust-version".into())
}

fn msrv() -> Result<(), String> {
    let v = msrv_version()?;
    let installed = Command::new("rustup").args(["run", &v, "cargo", "--version"]).output().is_ok_and(|o| o.status.success());
    if !installed {
        return Err(format!("msrv: the {v} toolchain isn't installed (rustup toolchain install {v} --profile minimal)"));
    }
    let mut c = Command::new("rustup");
    c.current_dir(root())
        .args(["run", &v, "cargo", "check", "--workspace", "--all-targets", "--locked"])
        .env("CARGO_TARGET_DIR", target_dir().join("msrv"))
        .env_remove("CARGO")
        .env_remove("RUSTC");
    run(&mut c)?;
    println!("msrv: ok on {v}");
    Ok(())
}

fn advisories() -> Result<(), String> {
    if let Some(mut c) = deny_cmd(&["advisories", "bans", "sources"]) {
        run(&mut c)?;
        println!("advisories: ok");
    }
    Ok(())
}

const USAGE: &str = "usage: cargo xtask <ci [--no-wait] | fmt | names | layers | deny-panics | clippy | test | wasm | msrv | advisories | bench [--filter T] [--repeat N] [--compare FILE] [--threshold PCT]>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (task, rest) = args.split_first().map(|(t, r)| (t.as_str(), r)).unwrap_or(("", &[]));
    let r = match task {
        "ci" => ci(rest),
        "fmt" => fmt(),
        "names" => names::check(&root()),
        "layers" => layers(),
        "deny-panics" => panics::check(&root()),
        "clippy" => run(&mut clippy_cmd()),
        "test" => run(&mut test_cmd()),
        "wasm" => wasm_cmd().and_then(|(mut c, n)| run(&mut c).map(|()| println!("wasm: ok ({n} crates)"))),
        "msrv" => msrv(),
        "advisories" => advisories(),
        "bench" => bench::bench(&root(), rest),
        _ => Err(USAGE.into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn architecture_doc_lists_every_crate_at_its_rank() {
        let doc = std::fs::read_to_string(root().join("docs").join("architecture.md")).unwrap_or_default();
        assert_eq!(doc_out_of_sync(&doc), []);
        assert!(doc_out_of_sync("| 0 | `ashlar-store` |").contains(&("ashlar-store", 1)));
    }
}
