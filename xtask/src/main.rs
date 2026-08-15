//! Repo tasks. `cargo xtask ci` is the gate every change must pass.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const MSRV: &str = "1.90";

/// FNV-1a hashes of words that must not appear in this repo (see CLAUDE.md, "Naming"). Kept as hashes
/// so this file does not itself contain them.
const FORBIDDEN: &[u64] = &[
    0xf75016fbb527aa8f,
    0x82757cee3dfc919e,
    0x9c89d87215276ff3,
    0x457dc19057ca90de,
    0x49b095d2fab9b11e,
    0xa3e02073f071feb3,
    0x00392b19133936cf,
    0x8709fcb399eeadec,
    0x6dad38891082a6af,
];

type Step = (&'static str, fn() -> bool);

const SCANNED: &[&str] = &["rs", "md", "toml", "sql", "yml", "yaml", "json", "html", "css", "txt", "example"];

fn main() -> ExitCode {
    let task = std::env::args().nth(1).unwrap_or_default();
    let steps: &[Step] = match task.as_str() {
        "ci" => &[("fmt", fmt), ("names", names), ("clippy", clippy), ("test", test), ("wasm", wasm), ("msrv", msrv)],
        "fmt" => &[("fmt", fmt)],
        "names" => &[("names", names)],
        "clippy" => &[("clippy", clippy)],
        "test" => &[("test", test)],
        "wasm" => &[("wasm", wasm)],
        "msrv" => &[("msrv", msrv)],
        _ => {
            eprintln!("usage: cargo xtask <ci|fmt|names|clippy|test|wasm|msrv>");
            return ExitCode::FAILURE;
        }
    };
    for (name, step) in steps {
        eprintln!("== {name}");
        if !step() {
            eprintln!("xtask: {name} failed");
            return ExitCode::FAILURE;
        }
    }
    eprintln!("xtask: ok");
    ExitCode::SUCCESS
}

fn cargo(args: &[&str]) -> bool {
    Command::new("cargo").args(args).current_dir(root()).status().is_ok_and(|status| status.success())
}

fn fmt() -> bool {
    cargo(&["fmt", "--all", "--check"])
}

fn clippy() -> bool {
    cargo(&["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"])
}

fn test() -> bool {
    cargo(&["test", "--workspace", "--all-features"])
}

/// The wire types and the SDK must keep building for the web target, where the SDK is a no-op.
fn wasm() -> bool {
    cargo(&["check", "--target", "wasm32-unknown-unknown", "-p", "tracer-proto", "-p", "tracer-sdk"])
}

/// Checks the published library crates against the minimum supported Rust version, when that toolchain
/// is installed. Skips with a note otherwise so a fresh checkout is not blocked.
fn msrv() -> bool {
    let toolchain = format!("+{MSRV}");
    let installed = Command::new("cargo").args([&toolchain, "--version"]).output().is_ok_and(|output| output.status.success());
    if !installed {
        eprintln!("   skipped: toolchain {MSRV} is not installed (rustup toolchain install {MSRV})");
        return true;
    }
    cargo(&[&toolchain, "check", "-p", "tracer-proto", "-p", "tracer-sdk", "-p", "tracer-server"])
}

/// Fails when a forbidden word appears in any tracked text file.
fn names() -> bool {
    let mut hits = Vec::new();
    scan(&root(), &mut hits);
    for (path, line, word) in &hits {
        eprintln!("   {}:{line}: forbidden name starting with {:?}", path.display(), word.chars().next().unwrap_or('?'));
    }
    hits.is_empty()
}

fn scan(dir: &Path, hits: &mut Vec<(PathBuf, usize, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | ".git" | ".claude" | "graft" | "node_modules" | "data" | "plan") {
                scan(&path, hits);
            }
            continue;
        }
        let extension = path.extension().and_then(|extension| extension.to_str()).unwrap_or_default();
        if !SCANNED.contains(&extension) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (index, line) in text.lines().enumerate() {
            for word in line.split(|c: char| !c.is_ascii_alphanumeric()) {
                if FORBIDDEN.contains(&fnv1a(&word.to_ascii_lowercase())) {
                    hits.push((path.clone(), index + 1, word.to_owned()));
                }
            }
        }
    }
}

fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default()
}
