//! `cargo xtask bench`: the performance gate (FRAMEWORK-PLAN §5).
//!
//! Runs every ignored test whose name contains `perf`, in every crate, in release, one at a time
//! (`cargo test --release -- --ignored --nocapture perf`, but each test in its own process so they
//! don't disturb each other), and writes `target/bench/bench.json` and `bench.md`.
//!
//! A perf test reports numbers by printing lines of the form
//!
//! ```text
//! bench: <metric> <value> <unit>        e.g. `bench: edit_100k 812 ns`
//! ```
//!
//! Units ending in `/s` (`ops/s`, `MB/s`) are higher-is-better; every other unit is lower-is-better.
//! A test that prints no `bench:` line is measured by its wall time. `--compare <bench.json>`
//! fails when a metric is more than `--threshold` percent (default 5) worse than in that file.
//!
//! ```text
//! cargo xtask bench [--filter <text>] [--repeat N] [--compare <file>] [--threshold <percent>]
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    pub unit: String,
}

impl Metric {
    pub fn higher_is_better(&self) -> bool {
        self.unit.ends_with("/s")
    }
}

#[derive(Clone, Debug)]
struct TestRun {
    krate: String,
    test: String,
    ok: bool,
    wall_ms: f64,
    metrics: Vec<Metric>,
    output: String,
}

/// The `bench:` lines of a test's output.
pub fn parse_metrics(output: &str) -> Vec<Metric> {
    output
        .lines()
        .filter_map(|l| l.trim().strip_prefix("bench:"))
        .filter_map(|rest| {
            let mut it = rest.split_whitespace();
            let name = it.next()?.to_owned();
            let value: f64 = it.next()?.parse().ok()?;
            let unit = it.next().unwrap_or("").to_owned();
            value.is_finite().then_some(Metric { name, value, unit })
        })
        .collect()
}

struct Options {
    filter: String,
    repeat: usize,
    compare: Option<PathBuf>,
    threshold: f64,
}

fn options(args: &[String]) -> Result<Options, String> {
    let mut o = Options { filter: String::new(), repeat: 1, compare: None, threshold: 5.0 };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or_else(|| format!("bench: {a} needs a value"));
        match a.as_str() {
            "--filter" => o.filter = value()?,
            "--repeat" => o.repeat = value()?.parse::<usize>().map_err(|e| format!("bench: --repeat: {e}"))?.clamp(1, 100),
            "--compare" => o.compare = Some(PathBuf::from(value()?)),
            "--threshold" => o.threshold = value()?.parse::<f64>().map_err(|e| format!("bench: --threshold: {e}"))?.abs(),
            other => return Err(format!("bench: unknown argument {other}")),
        }
    }
    Ok(o)
}

/// The crate name in a cargo package id (`path+file:///…/store#ashlar-store@0.1.0` or `…/ashlar-store#0.1.0`).
fn crate_of(package_id: &str) -> String {
    let (path, frag) = package_id.rsplit_once('#').unwrap_or((package_id, ""));
    match frag.split_once('@') {
        Some((name, _)) => name.to_owned(),
        None => path.rsplit('/').next().unwrap_or(path).to_owned(),
    }
}

/// (crate, crate dir, test executable) for every test target of the workspace, built in release.
fn test_executables(root: &Path) -> Result<Vec<(String, PathBuf, PathBuf)>, String> {
    eprintln!("bench: building tests in release");
    let out = Command::new(crate::cargo())
        .current_dir(root)
        .args(["test", "--workspace", "--release", "--no-run", "--message-format=json-render-diagnostics"])
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("bench: cargo test --no-run: {e}"))?;
    if !out.status.success() {
        return Err("bench: building the tests failed".into());
    }
    let mut exes = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(msg) = serde_json::from_str::<Value>(line) else { continue };
        if msg["reason"] != "compiler-artifact" || msg["profile"]["test"] != true {
            continue;
        }
        let (Some(exe), Some(id), Some(manifest)) = (msg["executable"].as_str(), msg["package_id"].as_str(), msg["manifest_path"].as_str()) else {
            continue;
        };
        let dir = Path::new(manifest).parent().map(Path::to_path_buf).unwrap_or_else(|| root.to_path_buf());
        exes.push((crate_of(id), dir, PathBuf::from(exe)));
    }
    exes.sort();
    exes.dedup();
    Ok(exes)
}

/// The ignored tests of one executable whose names contain `perf` and `filter`.
fn perf_tests(exe: &Path, dir: &Path, filter: &str) -> Vec<String> {
    let Ok(out) = Command::new(exe).current_dir(dir).args(["--ignored", "--list", "perf"]).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_suffix(": test"))
        .filter(|name| name.contains(filter))
        .map(str::to_owned)
        .collect()
}

fn run_test(krate: &str, dir: &Path, exe: &Path, test: &str) -> TestRun {
    let started = Instant::now();
    let out = Command::new(exe).current_dir(dir).args([test, "--exact", "--ignored", "--nocapture", "--test-threads=1"]).output();
    let wall_ms = started.elapsed().as_secs_f64() * 1e3;
    let (ok, output) = match out {
        Ok(o) => (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => (false, e.to_string()),
    };
    TestRun { krate: krate.to_owned(), test: test.to_owned(), ok, wall_ms, metrics: parse_metrics(&output), output }
}

/// The metrics a run is judged by: its own, or its wall time when it reports none.
fn gated(run: &TestRun) -> Vec<Metric> {
    if run.metrics.is_empty() { vec![Metric { name: "wall".into(), value: run.wall_ms, unit: "ms".into() }] } else { run.metrics.clone() }
}

/// Keeps the best value of each metric across repeats (the least disturbed run).
fn best_of(a: &mut TestRun, b: TestRun) {
    a.ok &= b.ok;
    a.wall_ms = a.wall_ms.min(b.wall_ms);
    for m in b.metrics {
        match a.metrics.iter_mut().find(|x| x.name == m.name) {
            Some(x) if m.higher_is_better() => x.value = x.value.max(m.value),
            Some(x) => x.value = x.value.min(m.value),
            None => a.metrics.push(m),
        }
    }
}

fn key(krate: &str, test: &str, metric: &str) -> String {
    format!("{krate}/{test}/{metric}")
}

/// One comparison against a baseline: (key, old, new, change in percent where positive is worse).
#[derive(Debug, PartialEq)]
pub struct Change {
    pub key: String,
    pub unit: String,
    pub old: f64,
    pub new: f64,
    pub worse_pct: f64,
}

/// Each current metric that also appears in the baseline (`bench.json` of an earlier run).
pub fn compare(baseline: &Value, current: &Value) -> Vec<Change> {
    let index = |v: &Value| {
        let mut m = BTreeMap::new();
        for r in v["results"].as_array().into_iter().flatten() {
            let (k, t) = (r["crate"].as_str().unwrap_or_default(), r["test"].as_str().unwrap_or_default());
            for x in r["gated"].as_array().into_iter().flatten() {
                if let (Some(name), Some(value)) = (x["name"].as_str(), x["value"].as_f64()) {
                    m.insert(key(k, t, name), (value, x["unit"].as_str().unwrap_or_default().to_owned()));
                }
            }
        }
        m
    };
    let old = index(baseline);
    let mut out = Vec::new();
    for (k, (new, unit)) in index(current) {
        let Some((old, _)) = old.get(&k).cloned() else { continue };
        if old <= 0.0 {
            continue;
        }
        let higher = Metric { name: String::new(), value: 0.0, unit: unit.clone() }.higher_is_better();
        let change = (new - old) / old * 100.0;
        out.push(Change { key: k, unit, old, new, worse_pct: if higher { -change } else { change } });
    }
    out
}

fn markdown(runs: &[TestRun], changes: &[Change], threshold: f64) -> String {
    let mut md = String::from("# Benchmarks\n\n| crate | test | ok | metric | value |\n|---|---|---|---|---|\n");
    for r in runs {
        for m in gated(r) {
            md.push_str(&format!(
                "| {} | {} | {} | {} | {:.3} {} |\n",
                r.krate,
                r.test,
                if r.ok { "yes" } else { "**no**" },
                m.name,
                m.value,
                m.unit
            ));
        }
    }
    if !changes.is_empty() {
        md.push_str(&format!("\n## Compared with the baseline (regression: more than {threshold}% worse)\n\n| metric | before | after | worse by |\n|---|---|---|---|\n"));
        for c in changes {
            let flag = if c.worse_pct > threshold { " **REGRESSION**" } else { "" };
            md.push_str(&format!("| {} | {:.3} {u} | {:.3} {u} | {:+.1}%{flag} |\n", c.key, c.old, c.new, c.worse_pct, u = c.unit));
        }
    }
    md
}

fn git_commit(root: &Path) -> String {
    Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

pub fn bench(root: &Path, args: &[String]) -> Result<(), String> {
    let o = options(args)?;
    let baseline = match &o.compare {
        Some(p) => Some(
            std::fs::read_to_string(p)
                .map_err(|e| format!("bench: {}: {e}", p.display()))
                .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| format!("bench: {}: {e}", p.display())))?,
        ),
        None => None,
    };
    let mut tests = Vec::new();
    for (krate, dir, exe) in test_executables(root)? {
        for t in perf_tests(&exe, &dir, &o.filter) {
            tests.push((krate.clone(), dir.clone(), exe.clone(), t));
        }
    }
    if tests.is_empty() {
        return Err("bench: no ignored tests named `perf…` found".into());
    }
    let mut runs: Vec<TestRun> = Vec::new();
    for round in 0..o.repeat {
        for (i, (krate, dir, exe, test)) in tests.iter().enumerate() {
            eprintln!(
                "bench: [{}/{}] {krate} {test}{}",
                i + 1,
                tests.len(),
                if o.repeat > 1 { format!(" (round {})", round + 1) } else { String::new() }
            );
            let run = run_test(krate, dir, exe, test);
            if !run.ok {
                eprintln!("{}", run.output);
            }
            match runs.get_mut(i) {
                Some(prev) => best_of(prev, run),
                None => runs.push(run),
            }
        }
    }
    let results: Vec<Value> = runs
        .iter()
        .map(|r| {
            let ms = |v: &[Metric]| v.iter().map(|m| json!({"name": m.name, "value": m.value, "unit": m.unit})).collect::<Vec<_>>();
            json!({"crate": r.krate, "test": r.test, "ok": r.ok, "wall_ms": r.wall_ms, "metrics": ms(&r.metrics), "gated": ms(&gated(r))})
        })
        .collect();
    let report = json!({
        "version": 1,
        "commit": git_commit(root),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "repeat": o.repeat,
        "unix_time": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "results": results,
    });
    let changes = baseline.as_ref().map(|b| compare(b, &report)).unwrap_or_default();
    let out_dir = root.join("target").join("bench");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("bench: {}: {e}", out_dir.display()))?;
    let pretty = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(out_dir.join("bench.json"), pretty).map_err(|e| format!("bench: write bench.json: {e}"))?;
    let md = markdown(&runs, &changes, o.threshold);
    std::fs::write(out_dir.join("bench.md"), &md).map_err(|e| format!("bench: write bench.md: {e}"))?;
    println!("{md}\nbench: wrote {}", out_dir.join("bench.{json,md}").display());

    let failed: Vec<String> = runs.iter().filter(|r| !r.ok).map(|r| format!("{} {}", r.krate, r.test)).collect();
    let regressed: Vec<String> = changes.iter().filter(|c| c.worse_pct > o.threshold).map(|c| format!("{} {:+.1}%", c.key, c.worse_pct)).collect();
    let mut errors = Vec::new();
    if !failed.is_empty() {
        errors.push(format!("bench: {} perf test(s) failed: {}", failed.len(), failed.join(", ")));
    }
    if !regressed.is_empty() {
        errors.push(format!("bench: {} regression(s) over {}%:\n  {}", regressed.len(), o.threshold, regressed.join("\n  ")));
    }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("\n")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_parsed_from_bench_lines() {
        let out = "running 1 test\nbench: edit 812.5 ns\n  bench: rate 1e6 ops/s\nbench: bad x ns\nnot bench: x 1 ns\n";
        let m = parse_metrics(out);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], Metric { name: "edit".into(), value: 812.5, unit: "ns".into() });
        assert!(m[1].higher_is_better() && !m[0].higher_is_better());
    }

    #[test]
    fn crate_names_from_package_ids() {
        assert_eq!(crate_of("path+file:///a/crates/store#ashlar-store@0.1.0"), "ashlar-store");
        assert_eq!(crate_of("path+file:///a/crates/ashlar#0.1.0"), "ashlar");
    }

    fn report(edit_ns: f64, rate: f64) -> Value {
        json!({"results": [{"crate": "c", "test": "perf_a", "gated": [
            {"name": "edit", "value": edit_ns, "unit": "ns"},
            {"name": "rate", "value": rate, "unit": "ops/s"},
        ]}]})
    }

    #[test]
    fn comparison_knows_which_way_is_worse() {
        let changes = compare(&report(100.0, 1000.0), &report(110.0, 900.0));
        assert_eq!(changes.len(), 2);
        assert!((changes[0].worse_pct - 10.0).abs() < 1e-9, "slower is worse");
        assert!((changes[1].worse_pct - 10.0).abs() < 1e-9, "lower throughput is worse");
        let better = compare(&report(100.0, 1000.0), &report(90.0, 1100.0));
        assert!(better.iter().all(|c| c.worse_pct < 0.0));
    }

    #[test]
    fn repeats_keep_the_best_value() {
        let run = |edit: f64, rate: f64| TestRun {
            krate: "c".into(),
            test: "t".into(),
            ok: true,
            wall_ms: edit,
            metrics: vec![
                Metric { name: "edit".into(), value: edit, unit: "ns".into() },
                Metric { name: "rate".into(), value: rate, unit: "ops/s".into() },
            ],
            output: String::new(),
        };
        let mut a = run(100.0, 10.0);
        best_of(&mut a, run(90.0, 5.0));
        assert_eq!((a.metrics[0].value, a.metrics[1].value), (90.0, 10.0));
    }

    #[test]
    fn tests_without_metrics_are_gated_by_wall_time() {
        let r = TestRun { krate: "c".into(), test: "t".into(), ok: true, wall_ms: 12.0, metrics: vec![], output: String::new() };
        assert_eq!(gated(&r), [Metric { name: "wall".into(), value: 12.0, unit: "ms".into() }]);
    }
}
