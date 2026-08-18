//! `cargo xtask <ci|layers>`: the repo's gates.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::process::{Command, ExitCode};

/// Layer of each workspace crate. A crate may depend only on crates of a lower layer.
/// 0–1 core (headless, wasm-clean) · 2–4 services · 5 ui · 6 runtime · 7 umbrella · 8 examples.
/// Nothing below 5 may depend on a UI crate.
const LAYERS: &[(&str, u8)] = &[
    ("ashlar-command", 0),
    ("ashlar-fs", 0),
    ("ashlar-jobs", 0),
    ("ashlar-store", 1),
    ("ashlar-native", 2),
    ("ashlar-control", 2),
    ("ashlar-mcp", 3),
    ("ashlar-cli", 4),
    ("ashlar-print", 2),
    ("ashlar-ui", 5),
    ("ashlar-app", 6),
    ("ashlar", 7),
    ("ashlar-example-gallery", 8),
    ("ashlar-example-notes", 8),
    ("xtask", 9),
];
/// Crates no L0/L1 crate may depend on (directly).
const UI_ONLY: &[&str] = &["egui", "eframe", "winit", "egui-wgpu", "egui_kittest"];

fn layer(name: &str) -> Option<u8> {
    LAYERS.iter().find(|(n, _)| *n == name).map(|(_, l)| *l)
}

fn layers() -> Result<(), String> {
    let out = Command::new("cargo").args(["metadata", "--format-version", "1", "--no-deps"]).output().map_err(|e| format!("cargo metadata: {e}"))?;
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata json: {e}"))?;
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
    if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
}

fn run(args: &[&str]) -> Result<(), String> {
    eprintln!("ci: cargo {}", args.join(" "));
    let ok = Command::new("cargo").args(args).status().map_err(|e| e.to_string())?.success();
    if ok { Ok(()) } else { Err(format!("cargo {} failed", args.join(" "))) }
}

fn ci() -> Result<(), String> {
    run(&["fmt", "--all", "--check"])?;
    layers()?;
    run(&["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])?;
    run(&["test", "--workspace"])?;
    Ok(())
}

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let r = match task.as_str() {
        "layers" => layers(),
        "ci" => ci(),
        _ => Err("usage: cargo xtask <ci|layers>".into()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
