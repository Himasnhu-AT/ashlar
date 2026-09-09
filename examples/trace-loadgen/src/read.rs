//! Read-path measurements on a seeded database: rollup times, every dashboard page and report format, cold and
//! warm; and ingest latency while the rollup and retention jobs run.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ashlar_trace_server::jobs;
use ashlar_trace_server::store::SqliteStore;

use crate::run::{self, Phase, RunArgs};
use crate::spawn;
use crate::synth::Profile;

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1000.0
}

struct Browser {
    base: String,
    agent: ureq::Agent,
    cookie: String,
}

fn set_cookies(response: &ureq::http::Response<ureq::Body>) -> Vec<String> {
    response.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).filter_map(|l| l.split(';').next()).map(str::to_string).collect()
}

impl Browser {
    fn login(base: &str) -> Result<Self, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        let mut page = agent.get(format!("{base}/login")).call().map_err(|e| e.to_string())?;
        let form_cookies = set_cookies(&page).join("; ");
        let html = page.body_mut().read_to_string().map_err(|e| e.to_string())?;
        let marker = "name=\"csrf\" value=\"";
        let from = html.find(marker).ok_or("no csrf field")? + marker.len();
        let csrf = &html[from..from + html[from..].find('"').unwrap_or(0)];
        let response = agent
            .post(format!("{base}/login"))
            .header("cookie", &form_cookies)
            .header("content-type", "application/x-www-form-urlencoded")
            .send(format!("username=admin&password=seeded+admin+password&csrf={csrf}"))
            .map_err(|e| e.to_string())?;
        let cookie = set_cookies(&response).into_iter().find(|c| c.starts_with("tracer_session=")).ok_or("login failed")?;
        Ok(Self { base: base.to_string(), agent, cookie })
    }

    /// Returns (status, body bytes, milliseconds).
    fn get(&self, path: &str) -> (u16, usize, f64) {
        let started = Instant::now();
        match self.agent.get(format!("{}{path}", self.base)).header("cookie", &self.cookie).call() {
            Ok(mut r) => {
                let status = r.status().as_u16();
                let len = r.body_mut().read_to_vec().map_or(0, |b| b.len());
                (status, len, ms(started))
            }
            Err(_) => (0, 0, ms(started)),
        }
    }
}

fn counts(db: &PathBuf) {
    if let Ok(conn) = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        let mut line = Vec::new();
        for table in [
            "installs",
            "sessions",
            "events",
            "perf",
            "crashes",
            "issues",
            "items_seen",
            "daily_sessions",
            "daily_events",
            "daily_perf",
            "daily_installs",
        ] {
            let n: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap_or(-1);
            line.push(format!("{table} {n}"));
        }
        println!("rows: {}", line.join(", "));
    }
    println!("database file: {:.2} GiB", std::fs::metadata(db).map_or(0, |m| m.len()) as f64 / 1_073_741_824.0);
}

pub fn command(opts: &HashMap<String, String>) -> Result<(), String> {
    let dir = PathBuf::from(opts.get("dir").ok_or("--dir DIR is required")?);
    let db = dir.join("tracer.db");
    let mode = opts.get("mode").map_or("pages", String::as_str);
    counts(&db);
    match mode {
        "rollup" => rollups(&db),
        "pages" => pages(&dir),
        "contend" => contend(&dir, opts),
        other => Err(format!("unknown --mode {other} (rollup, pages, contend)")),
    }
}

fn rollups(db: &PathBuf) -> Result<(), String> {
    let store = SqliteStore::open(db).map_err(|e| e.to_string())?;
    let now = ashlar_trace_server::now_ms();
    let t = Instant::now();
    let closed = jobs::close_stale_sessions(&store, now).map_err(|e| e.to_string())?;
    println!("close stale sessions: {:.0} ms ({closed} closed)", ms(t));
    let t = Instant::now();
    jobs::rollup_recent(&store, now, 90, 90).map_err(|e| e.to_string())?;
    println!("rollup, full 90 days: {:.0} ms", ms(t));
    for n in 1..=3 {
        let t = Instant::now();
        jobs::rollup_recent(&store, now, 3, 90).map_err(|e| e.to_string())?;
        println!("rollup, 3 days (run {n}): {:.0} ms", ms(t));
    }
    drop(store);
    // Leave a self-contained file behind (the log folded in), so it can be copied.
    if let Ok(conn) = rusqlite::Connection::open(db) {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
    Ok(())
}

fn pages(dir: &std::path::Path) -> Result<(), String> {
    // A fresh server process: its caches are empty for the first request of each kind.
    let mut server = spawn::spawn(Some(dir.to_path_buf()), &[])?;
    // The server rolls up the last three days as soon as it starts; let that finish so it does not disturb
    // the first requests.
    std::thread::sleep(Duration::from_secs(12));
    let browser = Browser::login(&server.url)?;
    println!("{:<34} {:>5} {:>10} {:>10} {:>10}", "page", "days", "cold ms", "warm ms", "bytes");
    let mut worst_cold: f64 = 0.0;
    let mut worst_warm: f64 = 0.0;
    let slug = "loadgen";
    let version = "1.7.0";
    let paths: Vec<(String, String)> = vec![
        ("overview".into(), format!("/apps/{slug}")),
        ("crashes".into(), format!("/apps/{slug}/crashes")),
        ("issue".into(), format!("/apps/{slug}/crashes/1")),
        ("performance".into(), format!("/apps/{slug}/performance")),
        ("usage".into(), format!("/apps/{slug}/usage")),
        ("reports index".into(), format!("/apps/{slug}/reports")),
        ("settings".into(), format!("/apps/{slug}/settings")),
        ("report page".into(), format!("/apps/{slug}/reports?version={version}")),
        ("report html".into(), format!("/apps/{slug}/reports?version={version}&format=html")),
        ("report markdown".into(), format!("/apps/{slug}/reports?version={version}&format=md")),
        ("report json".into(), format!("/apps/{slug}/reports?version={version}&format=json")),
    ];
    for days in [7, 30, 90] {
        for (name, path) in &paths {
            let sep = if path.contains('?') { '&' } else { '?' };
            let url = format!("{path}{sep}days={days}");
            let (status, bytes, cold) = browser.get(&url);
            let mut warm: Vec<f64> = (0..5).map(|_| browser.get(&url).2).collect();
            warm.sort_by(f64::total_cmp);
            let median = warm[warm.len() / 2];
            let flag = if status != 200 { format!("  <- HTTP {status}") } else { String::new() };
            println!("{name:<34} {days:>5} {cold:>10.0} {median:>10.0} {bytes:>10}{flag}");
            worst_cold = worst_cold.max(cold);
            worst_warm = worst_warm.max(median);
        }
    }
    println!("slowest cold {worst_cold:.0} ms, slowest warm {worst_warm:.0} ms");
    server.stop();
    Ok(())
}

/// Ingest at a fixed rate against the seeded database while the rollup and retention jobs run as separate
/// processes (the worst case for the writer: a second process contending for the file's write lock).
fn contend(dir: &std::path::Path, opts: &HashMap<String, String>) -> Result<(), String> {
    let rate: f64 = opts.get("rate").map_or(Ok(300.0), |r| r.parse().map_err(|_| "bad --rate"))?;
    let secs: f64 = opts.get("secs").map_or(Ok(20.0), |r| r.parse().map_err(|_| "bad --secs"))?;
    let mut server = spawn::spawn(Some(dir.to_path_buf()), &[])?;
    let jobs_dir = dir.to_path_buf();
    let which = opts.get("jobs").cloned().unwrap_or_else(|| "rollup prune".to_string());
    let binary = std::env::var_os("LOADGEN_SERVER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_exe().unwrap_or_default().with_file_name("ashlar-trace-server"));
    // Three phases of the same load: quiet, jobs running, quiet again.
    let jobs_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs_f64(secs));
        let mut timings = Vec::new();
        for (label, args) in [("rollup --days 3", vec!["rollup", "--days", "3"]), ("prune (retention 45 days)", vec!["prune"])] {
            if !which.contains(&label[..5]) {
                continue;
            }
            let t = Instant::now();
            let mut command = std::process::Command::new(&binary);
            command.env("TRACER_DATA_DIR", &jobs_dir).args(&args);
            if label.starts_with("prune") {
                command.env("TRACER_RETENTION_DAYS", "45");
            }
            let ok = command.output().is_ok_and(|o| o.status.success());
            timings.push(format!("{label}: {:.1} s{}", t.elapsed().as_secs_f64(), if ok { "" } else { " (failed)" }));
            std::thread::sleep(Duration::from_secs(2));
        }
        timings
    });
    let args = RunArgs {
        url: server.url.clone(),
        key: server.key.clone(),
        concurrency: 64,
        phases: vec![Phase { rate: Some(rate), secs }, Phase { rate: Some(rate), secs: secs * 2.0 }, Phase { rate: Some(rate), secs: 10.0 }],
        max_requests: None,
        profile: Profile { seed: 9, installs: 20_000, versions: 5, crash_permille: 50 },
        db: Some(server.db.clone()),
        server_pid: Some(server.pid()),
        sample_secs: 10.0,
        probe: true,
    };
    println!("phase 1: quiet; phase 2: rollup then prune run as separate processes; phase 3: quiet");
    run::run(&args);
    for line in jobs_thread.join().unwrap_or_default() {
        println!("{line}");
    }
    server.stop();
    Ok(())
}
