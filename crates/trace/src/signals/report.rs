//! The plain-text report a user can attach to a bug report.

use std::fmt::Write as _;
use std::path::Path;

use tracer_proto::{Breadcrumb, Crash, Device, Perf, Signal};

use super::{Shared, now_ms, sys};

/// Writes `bytes` to `path` through a temporary file in the same directory, then renames it into place.
pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = std::path::PathBuf::from(temp);
    let result = std::fs::write(&temp, bytes).and_then(|()| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Everything the report shows.
pub(super) struct Report<'a> {
    pub app_name: &'a str,
    pub app_version: &'a str,
    pub build: Option<&'a str>,
    pub signals: Vec<&'static str>,
    pub generated_ms: u64,
    /// `key: value` lines about the install and session, from the pipeline.
    pub session: &'a str,
    pub device: &'a Device,
    pub perf: Option<&'a Perf>,
    pub crash: Option<&'a Crash>,
    pub breadcrumbs: &'a [Breadcrumb],
}

pub(super) fn write(shared: &Shared, path: &Path) -> std::io::Result<()> {
    let device = sys::device_info();
    let perf = shared.last_perf();
    let crash = super::lock(&shared.last_crash, true).and_then(|slot| slot.clone());
    let breadcrumbs = if shared.allowed(Signal::Logs) { shared.ring.snapshot(true) } else { Vec::new() };
    let session = shared.sink.session_summary();
    let report = Report {
        app_name: &shared.settings.app_name,
        app_version: &shared.settings.app_version,
        build: shared.settings.build.as_deref(),
        signals: shared.settings.signals.iter().map(|signal| signal.as_str()).collect(),
        generated_ms: now_ms(),
        session: &session,
        device: &device,
        perf: perf.as_ref(),
        crash: crash.as_ref(),
        breadcrumbs: &breadcrumbs,
    };
    write_atomic(path, render(&report).as_bytes())
}

pub(super) fn render(report: &Report<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Diagnostic report");
    let _ = writeln!(out, "=================");
    let _ = writeln!(out, "Generated: {}", format_utc(report.generated_ms));
    let _ = writeln!(out);

    let _ = writeln!(out, "App");
    let _ = writeln!(out, "  Name:    {}", report.app_name);
    let _ = writeln!(out, "  Version: {}", report.app_version);
    if let Some(build) = report.build {
        let _ = writeln!(out, "  Build:   {build}");
    }
    let _ = writeln!(out, "  Signals: {}", if report.signals.is_empty() { "none".to_owned() } else { report.signals.join(", ") });
    let _ = writeln!(out);

    if !report.session.trim().is_empty() {
        let _ = writeln!(out, "Session");
        for line in report.session.lines().filter(|line| !line.trim().is_empty()) {
            let _ = writeln!(out, "  {line}");
        }
        let _ = writeln!(out);
    }

    let device = report.device;
    let _ = writeln!(out, "Device");
    let os = match &device.os_version {
        Some(version) => format!("{} {version}", device.os_name),
        None => device.os_name.clone(),
    };
    let _ = writeln!(out, "  OS:  {os} ({})", device.arch);
    if let Some(brand) = &device.cpu_brand {
        let cores = device.cpu_cores.map(|cores| format!(", {cores} cores")).unwrap_or_default();
        let _ = writeln!(out, "  CPU: {brand}{cores}");
    }
    if let Some(ram) = device.ram_mb {
        let _ = writeln!(out, "  RAM: {ram} MB");
    }
    if let Some(gpu) = &device.gpu {
        let _ = writeln!(out, "  GPU: {}", gpu.name);
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Last performance window");
    match report.perf {
        Some(perf) => write_perf(&mut out, perf),
        None => {
            let _ = writeln!(out, "  none recorded");
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Last crash or error");
    match report.crash {
        Some(crash) => write_crash(&mut out, crash),
        None => {
            let _ = writeln!(out, "  none recorded");
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "Recent log lines ({})", report.breadcrumbs.len());
    for crumb in report.breadcrumbs {
        let _ = writeln!(out, "  {} {:<5} {}: {}", format_utc(crumb.ts), crumb.level.to_ascii_uppercase(), crumb.target, crumb.message);
    }
    out
}

fn write_perf(out: &mut String, perf: &Perf) {
    let _ = writeln!(out, "  Window: {} s", perf.window_s);
    if let Some(frames) = &perf.frames {
        let avg = frames.sum_us.checked_div(frames.count).unwrap_or(0);
        let p95 = frames.quantile_us(0.95).unwrap_or(0);
        let _ = writeln!(
            out,
            "  Frames: {} (average {:.1} ms, 95th percentile {:.1} ms, worst {:.1} ms, {} slow, {} frozen)",
            frames.count,
            avg as f64 / 1000.0,
            f64::from(p95) / 1000.0,
            f64::from(frames.max_us) / 1000.0,
            frames.slow,
            frames.frozen
        );
    }
    if let Some(cpu) = perf.cpu_pct {
        let _ = writeln!(out, "  CPU:    {cpu:.1}%");
    }
    if let Some(rss) = perf.rss_mb {
        let peak = perf.rss_peak_mb.map(|peak| format!(" (peak {peak} MB)")).unwrap_or_default();
        let _ = writeln!(out, "  Memory: {rss} MB{peak}");
    }
    if let Some(startup) = perf.startup_ms {
        let _ = writeln!(out, "  Startup: {startup} ms");
    }
    for (name, value) in &perf.counters {
        let _ = writeln!(out, "  Counter {name}: {value}");
    }
    for (name, value) in &perf.gauges {
        let _ = writeln!(out, "  Gauge {name}: {value}");
    }
}

fn write_crash(out: &mut String, crash: &Crash) {
    let _ = writeln!(out, "  Kind:     {}", crash.kind.as_str());
    if let Some(message) = &crash.message {
        let _ = writeln!(out, "  Message:  {message}");
    }
    if let Some(location) = &crash.location {
        let _ = writeln!(out, "  Location: {location}");
    }
    if let Some(thread) = &crash.thread {
        let _ = writeln!(out, "  Thread:   {thread}");
    }
    if let Some(blocked) = crash.blocked_ms {
        let _ = writeln!(out, "  Blocked:  {blocked} ms");
    }
    if let Some(code) = crash.exit_code {
        let _ = writeln!(out, "  Exit code: {code}");
    }
    if let Some(signal) = crash.signal {
        let _ = writeln!(out, "  Signal:   {signal}");
    }
    if let Some(context) = &crash.context {
        let _ = writeln!(out, "  Context:  {context}");
    }
    if let Some(backtrace) = &crash.backtrace {
        let _ = writeln!(out, "  Backtrace:");
        for line in backtrace.lines() {
            let _ = writeln!(out, "    {line}");
        }
    }
}

/// `2026-08-15 09:05:07Z` for a Unix time in milliseconds.
pub(super) fn format_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rest = secs % 86_400;
    // Civil-from-days, after Howard Hinnant's date algorithms.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use log::Log;
    use tracer_proto::{CrashKind, Signal};

    use super::*;
    use crate::signals::testutil::{Fixture, TempDir};

    #[test]
    fn utc_formatting() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00Z");
        assert_eq!(format_utc(1_000_000_000_000), "2001-09-09 01:46:40Z");
        assert_eq!(format_utc(1_709_164_800_000), "2024-02-29 00:00:00Z");
        assert_eq!(format_utc(4_102_444_799_000), "2099-12-31 23:59:59Z");
    }

    #[test]
    fn render_lists_everything_readably() {
        let device = Device {
            os_name: "macos".into(),
            os_version: Some("26.0".into()),
            arch: "aarch64".into(),
            cpu_brand: Some("Example CPU".into()),
            cpu_cores: Some(10),
            ram_mb: Some(32768),
            ..Device::default()
        };
        let mut frames = tracer_proto::FrameStats::empty();
        frames.record(4_000);
        frames.record(20_000);
        let perf = Perf {
            window_s: 60,
            frames: Some(frames),
            cpu_pct: Some(12.5),
            rss_mb: Some(300),
            rss_peak_mb: Some(350),
            startup_ms: Some(800),
            ..Perf::default()
        };
        let crash = Crash {
            kind: CrashKind::Panic,
            message: Some("boom".into()),
            location: Some("src/x.rs:1:2".into()),
            thread: Some("main".into()),
            backtrace: Some("a\nb".into()),
            ..Crash::default()
        };
        let crumbs = vec![Breadcrumb { ts: 0, level: "warn".into(), target: "net".into(), message: "retrying".into() }];
        let text = render(&Report {
            app_name: "demo",
            app_version: "1.2.3",
            build: Some("abc"),
            signals: vec!["crash", "perf"],
            generated_ms: 0,
            session: "install_id: abc\nsession_id: def\n",
            device: &device,
            perf: Some(&perf),
            crash: Some(&crash),
            breadcrumbs: &crumbs,
        });
        for needle in [
            "demo",
            "  install_id: abc\n  session_id: def\n",
            "1.2.3",
            "abc",
            "crash, perf",
            "macos 26.0 (aarch64)",
            "Example CPU, 10 cores",
            "32768 MB",
            "Frames: 2",
            "300 MB (peak 350 MB)",
            "Startup: 800 ms",
            "Kind:     panic",
            "boom",
            "src/x.rs:1:2",
            "    a",
            "WARN  net: retrying",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn render_handles_a_fresh_session() {
        let device = Device { os_name: "linux".into(), arch: "x86_64".into(), ..Device::default() };
        let text = render(&Report {
            app_name: "demo",
            app_version: "1",
            build: None,
            signals: vec![],
            generated_ms: 0,
            session: "install_id: abc\nsession_id: def\n",
            device: &device,
            perf: None,
            crash: None,
            breadcrumbs: &[],
        });
        assert!(text.contains("none recorded"));
        assert!(text.contains("Signals: none"));
        assert!(text.contains("Recent log lines (0)"));
    }

    #[test]
    fn write_report_creates_the_file_without_leaving_a_temp_file() {
        let fixture = Fixture::new(|_| {});
        let logger = fixture.signals.logger(None);
        logger.log(&log::Record::builder().level(log::Level::Error).target("t").args(format_args!("saw /Users/zed/x")).build());
        fixture.signals.report_handled("it failed", Some("export"));
        let target = TempDir::new();
        let path = target.path().join("nested").join("report.txt");
        fixture.signals.write_report(&path).expect("write");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("demo") && text.contains("1.2.3"), "{text}");
        assert!(text.contains("it failed") && text.contains("export"), "{text}");
        assert!(text.contains("saw "), "{text}");
        assert!(!text.contains("zed"), "{text}");
        assert!(!path.with_extension("txt.tmp").exists());
    }

    #[test]
    fn report_includes_the_last_perf_window() {
        let fixture = Fixture::new(|_| {});
        fixture.signals.frame(Duration::from_millis(5));
        fixture.signals.tick_at(fixture.at(Duration::from_secs(60)));
        let target = TempDir::new();
        let path = target.path().join("r.txt");
        fixture.signals.write_report(&path).expect("write");
        assert!(std::fs::read_to_string(&path).expect("read").contains("Frames: 1"));
    }

    #[test]
    fn report_to_an_unwritable_path_is_an_error_not_a_panic() {
        let fixture = Fixture::new(|_| {});
        let target = TempDir::new();
        // The target is an existing directory, so renaming a file over it fails.
        assert!(fixture.signals.write_report(target.path()).is_err());
    }

    #[test]
    fn denied_logs_are_left_out_of_the_report() {
        let fixture = Fixture::new(|_| {});
        let logger = fixture.signals.logger(None);
        logger.log(&log::Record::builder().level(log::Level::Error).target("t").args(format_args!("secret line")).build());
        fixture.deny(Signal::Logs);
        let target = TempDir::new();
        let path = target.path().join("r.txt");
        fixture.signals.write_report(&path).expect("write");
        assert!(!std::fs::read_to_string(&path).expect("read").contains("secret line"));
    }
}
