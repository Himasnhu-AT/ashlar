//! The ingest load runner: open-loop (fixed rate) and closed-loop (max throughput) phases over real HTTP.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::synth::{self, Profile, Rng};

#[derive(Clone, Copy, Debug)]
pub struct Phase {
    /// Requests per second, scheduled on a clock; `None` sends as fast as the connections allow.
    pub rate: Option<f64>,
    pub secs: f64,
}

#[derive(Clone)]
pub struct RunArgs {
    pub url: String,
    pub key: String,
    pub concurrency: usize,
    pub phases: Vec<Phase>,
    pub max_requests: Option<u64>,
    pub profile: Profile,
    pub db: Option<PathBuf>,
    pub server_pid: Option<u32>,
    pub sample_secs: f64,
    pub probe: bool,
}

#[derive(Clone, Copy)]
struct Rec {
    phase: u8,
    status: u16,
    lat_us: u32,
    at_s: u32,
}

#[derive(Default, Clone, Copy)]
struct Counts {
    accepted: u64,
    duplicates: u64,
    rejected: u64,
}

pub fn rss_kb(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("ps").args(["-o", "rss=", "-p", &pid.to_string()]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

pub fn percentile(sorted: &[u32], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
    f64::from(sorted[index]) / 1000.0
}

fn db_items(path: &PathBuf) -> Option<i64> {
    let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    conn.busy_timeout(Duration::from_secs(30)).ok()?;
    // Ids this tool made are 24 characters long, so a seeded or pruned dedup table does not disturb the count.
    conn.query_row("SELECT count(*) FROM items_seen WHERE length(item_id) = 24", [], |row| row.get(0)).ok()
}

fn db_bytes(path: &PathBuf) -> u64 {
    let size = |p: &PathBuf| std::fs::metadata(p).map_or(0, |m| m.len());
    let mut wal = path.clone().into_os_string();
    wal.push("-wal");
    size(path) + size(&PathBuf::from(wal))
}

pub fn agent(concurrency: usize) -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_idle_connections_per_host(concurrency.max(1))
        .max_idle_connections(concurrency.max(1))
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into()
}

pub struct Report {
    pub ok: bool,
}

pub fn run(args: &RunArgs) -> Report {
    let total_secs: f64 = args.phases.iter().map(|p| p.secs).sum();
    let agent = agent(args.concurrency + 2);
    let endpoint = format!("{}/v1/ingest", args.url);
    let before = args.db.as_ref().and_then(db_items);
    let started = Instant::now();
    let seq = Arc::new(AtomicU64::new(0));
    let slots: Arc<Vec<AtomicU64>> = Arc::new(args.phases.iter().map(|_| AtomicU64::new(0)).collect());
    let stop = Arc::new(AtomicBool::new(false));
    let bytes_sent = Arc::new(AtomicU64::new(0));
    let items_sent = Arc::new(AtomicU64::new(0));
    let counts = Arc::new(Mutex::new(Counts::default()));
    let records: Arc<Mutex<Vec<Rec>>> = Arc::new(Mutex::new(Vec::new()));
    // Phase boundaries from the start of the run.
    let mut bounds = Vec::new();
    let mut acc = 0.0;
    for p in &args.phases {
        bounds.push((acc, acc + p.secs));
        acc += p.secs;
    }
    let bounds = Arc::new(bounds);
    let phases = Arc::new(args.phases.clone());

    // RSS sampler.
    let rss = Arc::new(Mutex::new(Vec::<(f64, u64)>::new()));
    let sampler = args.server_pid.map(|pid| {
        let (stop, rss, every) = (stop.clone(), rss.clone(), args.sample_secs);
        std::thread::spawn(move || {
            let mut next = 0.0;
            while !stop.load(Ordering::Relaxed) {
                let t = started.elapsed().as_secs_f64();
                if t >= next {
                    if let Some(kb) = rss_kb(pid) {
                        rss.lock().map(|mut v| v.push((t, kb))).ok();
                    }
                    next += every;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    });
    // Health and dashboard probes: they must stay responsive under load.
    let probes = Arc::new(Mutex::new(Vec::<(String, u16, u32)>::new()));
    let prober = args.probe.then(|| {
        let (stop, probes, base, agent) = (stop.clone(), probes.clone(), args.url.clone(), agent.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                for path in ["/v1/health", "/login"] {
                    let t = Instant::now();
                    let status = agent.get(format!("{base}{path}")).call().map_or(0, |r| r.status().as_u16());
                    probes.lock().map(|mut v| v.push((path.to_string(), status, t.elapsed().as_micros() as u32))).ok();
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        })
    });

    let mut workers = Vec::new();
    for worker in 0..args.concurrency {
        let (agent, endpoint, key, profile) = (agent.clone(), endpoint.clone(), args.key.clone(), args.profile.clone());
        let (seq, slots, stop, bytes_sent, items_sent, counts, records, bounds, phases) = (
            seq.clone(),
            slots.clone(),
            stop.clone(),
            bytes_sent.clone(),
            items_sent.clone(),
            counts.clone(),
            records.clone(),
            bounds.clone(),
            phases.clone(),
        );
        let max_requests = args.max_requests;
        workers.push(std::thread::spawn(move || {
            let mut rng = Rng::new(profile.seed ^ ((worker as u64 + 1) << 32));
            let mut local = Vec::new();
            let mut mine = Counts::default();
            loop {
                let elapsed = started.elapsed().as_secs_f64();
                if elapsed >= total_secs || stop.load(Ordering::Relaxed) {
                    break;
                }
                let Some(phase) = bounds.iter().position(|(_, end)| elapsed < *end) else { break };
                let (start, end) = bounds[phase];
                // Claim the slot the request is *meant* to leave in; latency is measured from there, so a server
                // that falls behind shows up as latency rather than as a lower send rate.
                let slot_at = match phases[phase].rate {
                    Some(rate) => {
                        let k = slots[phase].fetch_add(1, Ordering::Relaxed) as f64;
                        let at = start + k / rate;
                        if at >= end {
                            std::thread::sleep(Duration::from_secs_f64((end - elapsed).max(0.0)));
                            continue;
                        }
                        Some(at)
                    }
                    None => None,
                };
                let n = seq.fetch_add(1, Ordering::Relaxed);
                if max_requests.is_some_and(|max| n >= max) {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                // Build the body before waiting for the slot: the client's own generation time is not the
                // server's latency.
                let now_ms = tracer_server::now_ms() as u64;
                let (body, items) = synth::body(&profile, &mut rng, n, now_ms);
                let scheduled = match slot_at {
                    Some(at) => {
                        let wait = at - started.elapsed().as_secs_f64();
                        if wait > 0.0 {
                            std::thread::sleep(Duration::from_secs_f64(wait));
                        }
                        started + Duration::from_secs_f64(at)
                    }
                    None => Instant::now(),
                };
                bytes_sent.fetch_add(body.len() as u64, Ordering::Relaxed);
                let result = agent.post(&endpoint).header("x-tracer-key", &key).header("content-encoding", "gzip").send(&body[..]);
                let (status, text) = match result {
                    Ok(mut response) => (response.status().as_u16(), response.body_mut().read_to_string().unwrap_or_default()),
                    Err(_) => (0, String::new()),
                };
                if status == 200 {
                    items_sent.fetch_add(items, Ordering::Relaxed);
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                        mine.accepted += value["accepted"].as_u64().unwrap_or(0);
                        mine.duplicates += value["duplicates"].as_u64().unwrap_or(0);
                        mine.rejected += value["rejected"].as_u64().unwrap_or(0);
                    }
                }
                let lat = Instant::now().saturating_duration_since(scheduled).as_micros().min(u128::from(u32::MAX)) as u32;
                local.push(Rec { phase: phase as u8, status, lat_us: lat, at_s: started.elapsed().as_secs() as u32 });
            }
            if let Ok(mut total) = counts.lock() {
                total.accepted += mine.accepted;
                total.duplicates += mine.duplicates;
                total.rejected += mine.rejected;
            }
            if let Ok(mut all) = records.lock() {
                all.extend(local);
            }
        }));
    }
    for worker in workers {
        let _ = worker.join();
    }
    let wall = started.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    if let Some(s) = sampler {
        let _ = s.join();
    }
    if let Some(p) = prober {
        let _ = p.join();
    }
    let end_rss = args.server_pid.and_then(rss_kb);

    // ---- report ----
    let records = records.lock().map(|r| r.clone()).unwrap_or_default();
    let counts = counts.lock().map(|c| *c).unwrap_or_default();
    println!("\n== ingest load: {} requests in {:.1}s, concurrency {}, seed {} ==", records.len(), wall, args.concurrency, args.profile.seed);
    println!(
        "{:<8} {:>9} {:>8} {:>9} {:>8} {:>8} {:>8} {:>9}  status codes",
        "phase", "target/s", "req/s", "items/s", "p50 ms", "p90 ms", "p99 ms", "max ms"
    );
    let mut rows: Vec<(String, Vec<Rec>, f64, Option<f64>)> = Vec::new();
    for (i, p) in args.phases.iter().enumerate() {
        rows.push((format!("{}", i + 1), records.iter().copied().filter(|r| usize::from(r.phase) == i).collect(), p.secs, p.rate));
    }
    if args.phases.len() > 1 {
        rows.push(("all".into(), records.clone(), wall, None));
    }
    for (name, recs, secs, rate) in &rows {
        let mut lats: Vec<u32> = recs.iter().map(|r| r.lat_us).collect();
        lats.sort_unstable();
        let mut codes = std::collections::BTreeMap::new();
        for r in recs {
            *codes.entry(r.status).or_insert(0u64) += 1;
        }
        let ok = codes.get(&200).copied().unwrap_or(0);
        let items_per_req = items_sent.load(Ordering::Relaxed) as f64 / records.iter().filter(|r| r.status == 200).count().max(1) as f64;
        println!(
            "{:<8} {:>9} {:>8.0} {:>9.0} {:>8.1} {:>8.1} {:>8.1} {:>9.1}  {:?}",
            name,
            rate.map_or("max".to_string(), |r| format!("{r:.0}")),
            recs.len() as f64 / secs,
            ok as f64 * items_per_req / secs,
            percentile(&lats, 0.5),
            percentile(&lats, 0.9),
            percentile(&lats, 0.99),
            percentile(&lats, 1.0),
            codes
        );
    }
    if wall > 90.0 {
        println!("timeline (30 s windows): start  req/s  p50 ms  p99 ms  non-200");
        let mut window = 0;
        while f64::from(window * 30) < wall {
            let recs: Vec<&Rec> = records.iter().filter(|r| r.at_s / 30 == window).collect();
            let mut lats: Vec<u32> = recs.iter().map(|r| r.lat_us).collect();
            lats.sort_unstable();
            let bad = recs.iter().filter(|r| r.status != 200).count();
            println!(
                "  {:>4}s {:>8.0} {:>8.1} {:>8.1} {:>6}",
                window * 30,
                recs.len() as f64 / 30.0,
                percentile(&lats, 0.5),
                percentile(&lats, 0.99),
                bad
            );
            window += 1;
        }
    }
    let sent = bytes_sent.load(Ordering::Relaxed);
    println!(
        "bytes sent: {:.1} MiB ({:.0} KiB/s, {:.0} B/envelope)",
        sent as f64 / 1048576.0,
        sent as f64 / 1024.0 / wall,
        sent as f64 / records.len().max(1) as f64
    );
    if let Ok(rss) = rss.lock()
        && let (Some(first), Some(peak)) = (rss.first(), rss.iter().map(|s| s.1).max())
    {
        println!(
            "server RSS MiB: start {:.0}, peak {:.0}, end {:.0}",
            first.1 as f64 / 1024.0,
            peak as f64 / 1024.0,
            end_rss.unwrap_or(0) as f64 / 1024.0
        );
        let series: Vec<String> = rss.iter().map(|(t, kb)| format!("{:.0}s:{:.0}", t, *kb as f64 / 1024.0)).collect();
        println!("RSS series (s:MiB) {}", series.join(" "));
    }
    if let Ok(probes) = probes.lock()
        && !probes.is_empty()
    {
        for path in ["/v1/health", "/login"] {
            let mut lats: Vec<u32> = probes.iter().filter(|p| p.0 == path).map(|p| p.2).collect();
            lats.sort_unstable();
            let bad = probes.iter().filter(|p| p.0 == path && p.1 != 200).count();
            println!(
                "probe {path}: {} requests, p99 {:.1} ms, max {:.1} ms, non-200 {bad}",
                lats.len(),
                percentile(&lats, 0.99),
                percentile(&lats, 1.0)
            );
        }
    }
    // Recovery: seconds after the last overloaded second until a full second with only 200s.
    if args.phases.len() > 1 {
        let mut by_second: std::collections::BTreeMap<u32, (u64, u64)> = std::collections::BTreeMap::new();
        for r in &records {
            let e = by_second.entry(r.at_s).or_default();
            e.0 += 1;
            e.1 += u64::from(r.status != 200);
        }
        let worst = by_second.iter().filter(|(_, (_, bad))| *bad > 0).map(|(s, _)| *s).max();
        println!("last second with any non-200: {}", worst.map_or("none".to_string(), |s| format!("t={s}s")));
    }
    let mut ok = true;
    if let Some(path) = &args.db {
        let after = db_items(path);
        println!("database: {:.1} MiB incl. WAL", db_bytes(path) as f64 / 1048576.0);
        match (before, after) {
            (Some(b), Some(a)) => {
                let stored = (a - b) as u64;
                let verdict = if stored == counts.accepted { "OK" } else { "MISMATCH" };
                ok = stored == counts.accepted;
                println!(
                    "count check: server acknowledged {} items ({} duplicates, {} rejected); database stored {} -> {verdict}",
                    counts.accepted, counts.duplicates, counts.rejected, stored
                );
            }
            _ => println!("count check: could not read the database"),
        }
    }
    let server_errors = records.iter().filter(|r| r.status >= 500 || r.status == 0).count();
    println!("5xx or transport errors: {server_errors}");
    Report { ok }
}
