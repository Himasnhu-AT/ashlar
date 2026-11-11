//! `cargo xtask launch`: launch `examples/hello` (release) in a real window and measure it. Manual
//! only: it opens windows, so CI never runs it.
//!
//! - `cargo xtask launch [--runs N]` launches N times (default 9) and prints
//!   `bench: launch_to_present_p50/p95 <ms> ms` (the app's own figure, from the first line of
//!   `run()` to the first presented frame) and `bench: spawn_to_ready_p50/p95` (from spawning the
//!   process, so dyld and `main` count too).
//! - `cargo xtask launch --memory [--runs N] [--settle SECS]` launches N times (default 5), lets
//!   each idle (default 10 s) and prints the lowest and highest physical footprint, its peak
//!   (`vmmap --summary`, macOS) and the RSS (`ps`). On macOS the numbers fall in two groups
//!   (IMP-2.19): a window that was never covered stays at about 40 MB / 94 MB peak, while one whose
//!   occlusion state changed once (covered by another window, minimized, a Space switch, even right
//!   after launch) keeps two swapchain drawables mapped for the window server (~19 MB) after a
//!   ~95 MB startup spike: about 66 MB / 193 MB. The `_max` figures are what users see; the run
//!   count per group is printed too.
//!
//! The app writes `{pid, ready_ms}` to `HELLO_READY_FILE` after its first present. Every child is
//! killed before the next starts.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Options {
    runs: Option<usize>,
    memory: bool,
    settle: u64,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut o = Options { runs: None, memory: false, settle: 10 };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("launch: {a} needs a value"));
        match a.as_str() {
            "--runs" => o.runs = Some(value()?.parse::<usize>().map_err(|e| format!("launch: --runs: {e}"))?.clamp(1, 100)),
            "--memory" => o.memory = true,
            "--settle" => o.settle = value()?.parse::<u64>().map_err(|e| format!("launch: --settle: {e}"))?.min(600),
            other => return Err(format!("launch: unknown argument {other}")),
        }
    }
    Ok(o)
}

/// Build hello in release and return its executable.
fn build_hello(root: &Path) -> Result<PathBuf, String> {
    eprintln!("launch: building examples/hello in release");
    let out = Command::new(crate::cargo())
        .current_dir(root)
        .args(["build", "--release", "-p", "ashlar-example-hello", "--message-format=json-render-diagnostics"])
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("launch: cargo build hello: {e}"))?;
    if !out.status.success() {
        return Err("launch: building examples/hello failed".into());
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| m["reason"] == "compiler-artifact" && m["target"]["name"] == "ashlar-example-hello")
        .find_map(|m| m["executable"].as_str().map(PathBuf::from))
        .ok_or_else(|| "launch: cargo reported no hello executable".into())
}

/// Kills the child when dropped, whatever happens in between.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Launch once and wait for the ready file: (the app's ready_ms, spawn to ready file).
fn launch_once(exe: &Path, ready: &Path) -> Result<(Running, f64, f64), String> {
    let _ = std::fs::remove_file(ready);
    let started = Instant::now();
    let child = Command::new(exe)
        .env("HELLO_READY_FILE", ready)
        // keep the logs out of the measurement
        .env("HELLO_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("launch: {}: {e}", exe.display()))?;
    let mut child = Running(child);
    let deadline = started + Duration::from_secs(20);
    loop {
        if let Ok(text) = std::fs::read_to_string(ready)
            && let Ok(v) = serde_json::from_str::<Value>(&text)
        {
            let spawn_ms = started.elapsed().as_secs_f64() * 1000.0;
            let ready_ms = v["ready_ms"].as_f64().ok_or("launch: the ready file has no ready_ms")?;
            return Ok((child, ready_ms, spawn_ms));
        }
        if let Ok(Some(status)) = child.0.try_wait() {
            return Err(format!("launch: hello exited before its first frame ({status})"));
        }
        if Instant::now() > deadline {
            return Err("launch: no ready file after 20 s".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// The value at quantile `q` (nearest rank) of sorted samples.
fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted.get(rank - 1).copied().unwrap_or(0.0)
}

fn print_quantiles(name: &str, mut samples: Vec<f64>) {
    samples.sort_by(f64::total_cmp);
    println!("bench: {name}_p50 {:.1} ms", quantile(&samples, 0.5));
    println!("bench: {name}_p95 {:.1} ms", quantile(&samples, 0.95));
}

/// `vmmap --summary` lines like `Physical footprint:         36.7M`, in MB.
fn vmmap_mb(summary: &str, label: &str) -> Option<f64> {
    let line = summary.lines().find(|l| l.trim_start().starts_with(label))?;
    let value = line.trim_start().strip_prefix(label)?.trim();
    let (num, unit) = value.split_at(value.find(|c: char| !(c.is_ascii_digit() || c == '.'))?);
    let n: f64 = num.parse().ok()?;
    Some(match unit.trim().chars().next()? {
        'K' => n / 1024.0,
        'M' => n,
        'G' => n * 1024.0,
        _ => n / (1024.0 * 1024.0),
    })
}

/// One idle reading, in MB.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Idle {
    rss: Option<f64>,
    footprint: f64,
    peak: f64,
}

fn memory_once(exe: &Path, ready: &Path, settle: u64) -> Result<Idle, String> {
    let (child, _, _) = launch_once(exe, ready)?;
    std::thread::sleep(Duration::from_secs(settle));
    let pid = child.0.id().to_string();
    let rss = Command::new("ps").args(["-o", "rss=", "-p", &pid]).output().map_err(|e| format!("launch: ps: {e}"))?;
    let rss = String::from_utf8_lossy(&rss.stdout).trim().parse::<f64>().ok().map(|kb| kb / 1024.0);
    let out = Command::new("vmmap").args(["--summary", &pid]).output().map_err(|e| format!("launch: vmmap: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    drop(child);
    match (vmmap_mb(&text, "Physical footprint:"), vmmap_mb(&text, "Physical footprint (peak):")) {
        (Some(footprint), Some(peak)) => Ok(Idle { rss, footprint, peak }),
        _ => Err("launch: vmmap printed no footprint (macOS only)".into()),
    }
}

/// How many runs fall in the high group: a peak over 1.5x the lowest peak (IMP-2.19).
fn high_group(runs: &[Idle]) -> usize {
    let low = runs.iter().map(|r| r.peak).fold(f64::INFINITY, f64::min);
    runs.iter().filter(|r| r.peak > low * 1.5).count()
}

fn memory(exe: &Path, ready: &Path, runs: usize, settle: u64) -> Result<(), String> {
    let mut all = Vec::new();
    for i in 0..runs {
        eprintln!("launch: run {}/{runs}, idling {settle} s", i + 1);
        let r = memory_once(exe, ready, settle)?;
        eprintln!("launch: footprint {:.1} MB, peak {:.1} MB", r.footprint, r.peak);
        all.push(r);
        std::thread::sleep(Duration::from_millis(500));
    }
    let span = |f: fn(&Idle) -> f64| {
        let v: Vec<f64> = all.iter().map(f).collect();
        (v.iter().copied().fold(f64::INFINITY, f64::min), v.iter().copied().fold(0.0, f64::max))
    };
    let (f_min, f_max) = span(|r| r.footprint);
    let (p_min, p_max) = span(|r| r.peak);
    println!("bench: hello_idle_footprint_min {f_min:.1} MB");
    println!("bench: hello_idle_footprint_max {f_max:.1} MB");
    println!("bench: hello_peak_footprint_min {p_min:.1} MB");
    println!("bench: hello_peak_footprint_max {p_max:.1} MB");
    if let Some(rss) = all.iter().filter_map(|r| r.rss).reduce(f64::max) {
        println!("bench: hello_idle_rss_max {rss:.1} MB");
    }
    println!("launch: {} of {} runs in the high group (window covered at least once)", high_group(&all), all.len());
    Ok(())
}

pub fn launch(root: &Path, args: &[String]) -> Result<(), String> {
    let o = options(args)?;
    let exe = build_hello(root)?;
    let dir = root.join("target").join("bench");
    std::fs::create_dir_all(&dir).map_err(|e| format!("launch: {}: {e}", dir.display()))?;
    let ready = dir.join("hello-ready.json");
    if o.memory {
        return memory(&exe, &ready, o.runs.unwrap_or(5), o.settle);
    }
    // one unmeasured launch warms the disk cache and the shader caches
    drop(launch_once(&exe, &ready)?);
    let (mut app, mut spawn) = (Vec::new(), Vec::new());
    for _ in 0..o.runs.unwrap_or(9) {
        let (child, ready_ms, spawn_ms) = launch_once(&exe, &ready)?;
        drop(child);
        app.push(ready_ms);
        spawn.push(spawn_ms);
        std::thread::sleep(Duration::from_millis(300));
    }
    print_quantiles("launch_to_present", app);
    print_quantiles("spawn_to_ready", spawn);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_use_nearest_rank() {
        let s = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0];
        assert_eq!(quantile(&s, 0.5), 5.0);
        assert_eq!(quantile(&s, 0.95), 9.0);
        assert_eq!(quantile(&[], 0.5), 0.0);
    }

    #[test]
    fn the_high_group_is_counted_from_the_lowest_peak() {
        let r = |footprint, peak| Idle { rss: None, footprint, peak };
        assert_eq!(high_group(&[r(40.4, 93.5), r(66.0, 192.9), r(65.8, 192.7), r(40.1, 93.4)]), 2);
        assert_eq!(high_group(&[r(40.0, 93.0), r(41.0, 95.0)]), 0);
        assert_eq!(high_group(&[]), 0);
    }

    #[test]
    fn vmmap_sizes_read_in_megabytes() {
        let t = "Physical footprint:         36.7M\nPhysical footprint (peak):  1.5G\n";
        assert_eq!(vmmap_mb(t, "Physical footprint:"), Some(36.7));
        assert_eq!(vmmap_mb(t, "Physical footprint (peak):"), Some(1536.0));
        assert_eq!(vmmap_mb(t, "Missing:"), None);
    }
}
