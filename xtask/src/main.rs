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
        "demo" => &[("demo", demo)],
        "stress" => &[("stress", stress)],
        "load" => &[("load", load)],
        _ => {
            eprintln!("usage: cargo xtask <ci|fmt|names|clippy|test|wasm|msrv|demo|stress|load>");
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

/// Runs the SDK's benchmarks and heavy stress tests (`crates/tracer-sdk/tests/stress.rs`) in a release
/// build and prints their tables. Each runs in a process of its own, so that one cannot disturb another's
/// measurements. Not part of `ci`: it takes several minutes. `TRACER_STRESS_RUNS`, `TRACER_STRESS_SEED`,
/// `TRACER_STRESS_CALLS`, `TRACER_STRESS_IDLE_SECS` and `TRACER_STRESS_MEMORY_SECS` shorten or vary it.
fn stress() -> bool {
    const TESTS: &[&str] = &[
        "bench_host_thread_calls",
        "bench_worst_single_call",
        "bench_init_and_shutdown",
        "bench_worker_throughput",
        "bench_worker_throughput_with_many_adopted_folders",
        "bench_gzip_levels",
        "bench_idle_and_steady_cpu",
        "bench_memory_stays_flat_with_the_server_unreachable",
        "stress_flood_with_exact_accounting",
        "stress_flood_with_consent_and_forget",
        "stress_many_processes_on_one_data_directory",
        "stress_a_thousand_starts_and_stops_leak_nothing",
    ];
    TESTS.iter().all(|name| {
        cargo(&[
            "test",
            "--quiet",
            "-p",
            "tracer-sdk",
            "--release",
            "--test",
            "stress",
            "--",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
            "--exact",
            name,
        ])
    })
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

/// Checks and lints the windowed demo, which lives outside the workspace because its GUI toolkit has a
/// very large dependency tree. Not part of `ci`. It compiles the demo; it does not run it.
/// A short standard load profile against a spawned release server: max throughput, then a fixed 500/s.
/// Heavier profiles (soak, overload, the large read-path database) are `loadgen` commands; see
/// `docs/self-hosting.md`, "Capacity and tuning".
fn load() -> bool {
    let run = |extra: &[&str]| {
        let mut args = vec!["run", "-q", "--release", "-p", "loadgen", "--", "run", "--spawn", "--concurrency", "64", "--duration", "10"];
        args.extend_from_slice(extra);
        cargo(&args)
    };
    cargo(&["build", "--release", "-p", "tracer-server", "-p", "loadgen"]) && run(&[]) && run(&["--rate", "500"])
}

fn demo() -> bool {
    cargo(&["check", "--manifest-path", "examples/demo-gui/Cargo.toml", "--locked"])
        && cargo(&["clippy", "--manifest-path", "examples/demo-gui/Cargo.toml", "--locked", "--", "-D", "warnings"])
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
