//! The telemetry (trace) crates' heavier runs, not part of `ci`.

use crate::{cmd, run};

/// The SDK's benchmarks and heavy stress tests (`crates/trace/tests/stress.rs`) in a release build,
/// each in a process of its own so that one cannot disturb another's measurements. Takes several
/// minutes. `TRACER_STRESS_RUNS`, `TRACER_STRESS_SEED`, `TRACER_STRESS_CALLS`,
/// `TRACER_STRESS_IDLE_SECS` and `TRACER_STRESS_MEMORY_SECS` shorten or vary it.
pub fn stress() -> Result<(), String> {
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
    for name in TESTS {
        run(&mut cmd(&[
            "test",
            "--quiet",
            "-p",
            "ashlar-trace",
            "--release",
            "--test",
            "stress",
            "--",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
            "--exact",
            name,
        ]))?;
    }
    Ok(())
}

/// A short standard load profile against a spawned release server: max throughput, then a fixed
/// 500/s. Heavier profiles are `loadgen` commands (docs/trace/self-hosting.md, "Capacity and tuning").
pub fn load() -> Result<(), String> {
    run(&mut cmd(&["build", "--release", "-p", "ashlar-trace-server", "-p", "ashlar-example-trace-loadgen"]))?;
    let base = ["run", "-q", "--release", "-p", "ashlar-example-trace-loadgen", "--", "run", "--spawn", "--concurrency", "64", "--duration", "10"];
    run(&mut cmd(&base))?;
    let mut fixed = base.to_vec();
    fixed.extend(["--rate", "500"]);
    run(&mut cmd(&fixed))
}

/// Checks and lints the windowed demo, which is its own workspace (its GUI toolkit's tree is large).
/// It compiles the demo; it does not run it.
pub fn demo() -> Result<(), String> {
    let manifest = "examples/trace-demo-gui/Cargo.toml";
    run(&mut cmd(&["check", "--manifest-path", manifest, "--locked"]))?;
    run(&mut cmd(&["clippy", "--manifest-path", manifest, "--locked", "--", "-D", "warnings"]))
}
