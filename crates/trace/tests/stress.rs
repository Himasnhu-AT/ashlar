//! Benchmarks and heavy stress tests. Everything here is `#[ignore]`d: it is run by `cargo xtask trace-stress`,
//! in a release build and one test at a time, never by the gate.
//!
//! The numbers are wall-clock timings from `Instant`, taken in batches so that the clock's own cost does
//! not drown a call of a few nanoseconds. Each line of a table is: the median and the 99th percentile of
//! the per-call cost over all batches, and how far the median moved between repeats.

// This whole file is test code, but the lint only exempts `#[test]` functions, not their helpers.
#![allow(clippy::unwrap_used, clippy::panic)]

mod support;

use std::hint::black_box;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use ashlar_trace::{ConsentCategory, LoggerMode, Prop, Signal, Tracer};
use support::{AppEvent, Category, Instant200, TempDir};

const BATCH: u32 = 1_000;

/// Per-call nanoseconds over batches: (median, p99).
fn percentiles(mut batches: Vec<f64>) -> (f64, f64) {
    batches.sort_by(f64::total_cmp);
    let at = |q: f64| batches[((batches.len() - 1) as f64 * q) as usize];
    (at(0.5), at(0.99))
}

/// Times `call` in batches on one thread and returns the per-call cost of each batch.
fn time_batches(batches: usize, call: &dyn Fn()) -> Vec<f64> {
    for _ in 0..20_000 {
        call();
    }
    let mut costs = Vec::with_capacity(batches);
    for _ in 0..batches {
        let started = Instant::now();
        for _ in 0..BATCH {
            call();
        }
        costs.push(started.elapsed().as_nanos() as f64 / f64::from(BATCH));
    }
    costs
}

/// One line of the table: `call` on `threads` threads at once, `repeats` times.
fn bench(name: &str, threads: usize, batches: usize, between: &dyn Fn(), call: &(dyn Fn() + Sync)) {
    let mut medians = Vec::new();
    let mut worst_p99 = 0f64;
    for _ in 0..3 {
        between();
        let barrier = Barrier::new(threads);
        let all: Vec<f64> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        time_batches(batches, call)
                    })
                })
                .collect();
            handles.into_iter().flat_map(|handle| handle.join().unwrap()).collect()
        });
        let (median, p99) = percentiles(all);
        medians.push(median);
        worst_p99 = worst_p99.max(p99);
    }
    medians.sort_by(f64::total_cmp);
    println!("| {name:<44} | {threads:>2} | {:>9.1} | {:>10.1} | {:>6.1} .. {:<6.1} |", medians[1], worst_p99, medians[0], medians[2]);
}

fn header(title: &str) {
    println!("\n### {title}\n");
    println!("| {:<44} | thr | median ns | p99 ns     | median range     |", "call");
    println!("|{}|-----|-----------|------------|------------------|", "-".repeat(46));
}

fn host_tracer(dir: &TempDir, queue: usize) -> Tracer {
    Tracer::builder("key", "host", "1.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(Instant200)
        .send_before_notice(true)
        .listen(Category::File)
        .logger(LoggerMode::None)
        .configure(|config| {
            config.queue_capacity = queue;
            config.spool_max_bytes = 64 * 1024 * 1024;
        })
        .init()
}

/// Every hot-path call, as the table of per-call cost.
fn host_calls(tracer: &Tracer, threads: usize, label: &str) {
    header(&format!("Host-thread calls, {label}, {threads} thread(s)"));
    let counter = tracer.counter("bench_counter");
    let gauge = tracer.gauge("bench_gauge");
    let logger = tracer.logger(None);
    let failure = std::io::Error::other("the export failed");
    let drain = || {
        tracer.flush_blocking(Duration::from_secs(60));
    };
    let idle = || {};
    // The queue holds every call of one repeat, so these measure the cost of a call that is accepted.
    let queued = 150 / threads.max(1);
    bench("track (listened, queued)", threads, queued, &drain, &|| tracer.track(black_box(AppEvent::FileOpen)));
    bench("track_with 4 props (queued)", threads, queued, &drain, &|| {
        tracer.track_with(
            black_box(AppEvent::FileSave),
            &[("count", Prop::Int(3)), ("format", Prop::Str("native")), ("ratio", Prop::Float(0.5)), ("recent", Prop::Bool(true))],
        );
    });
    bench("track_id (queued)", threads, queued, &drain, &|| tracer.track_id(black_box(Category::File), "file.export_pdf"));
    bench("track (listened, queue full)", threads, 2_000, &drain, &|| tracer.track(black_box(AppEvent::FileOpen)));
    bench("track (not listened)", threads, 2_000, &idle, &|| tracer.track(black_box(AppEvent::Undo)));
    bench("frame", threads, 2_000, &idle, &|| tracer.frame(black_box(Duration::from_micros(4_200))));
    bench("begin_turn + end_turn", threads, 2_000, &idle, &|| {
        tracer.begin_turn();
        tracer.end_turn();
    });
    bench("turn() guard", threads, 2_000, &idle, &|| drop(tracer.turn()));
    bench("heartbeat", threads, 2_000, &idle, &|| tracer.heartbeat());
    bench("note_input", threads, 2_000, &idle, &|| tracer.note_input());
    bench("set_focused", threads, 2_000, &idle, &|| tracer.set_focused(black_box(true)));
    bench("Counter::inc", threads, 2_000, &idle, &|| counter.inc());
    bench("Gauge::set", threads, 2_000, &idle, &|| gauge.set(black_box(0.5)));
    bench("log info (kept as breadcrumb)", threads, 300, &idle, &|| {
        logger.log(&log::Record::builder().level(log::Level::Info).target("host").args(format_args!("saved document {}", black_box(7))).build());
    });
    bench("log debug (below the kept level)", threads, 2_000, &idle, &|| {
        logger.log(&log::Record::builder().level(log::Level::Debug).target("host").args(format_args!("saved document {}", black_box(7))).build());
    });
    bench("report_handled (queued or dropped)", threads, 100 / threads.max(1), &drain, &|| tracer.report_handled(&failure, Some("export")));
    bench("consent()", threads, 2_000, &idle, &|| {
        black_box(tracer.consent(ConsentCategory::Usage));
    });
}

#[test]
#[ignore = "benchmark; run with cargo xtask trace-stress"]
fn bench_host_thread_calls() {
    let dir = TempDir::new();
    let tracer = host_tracer(&dir, 1 << 18);
    for threads in [1, 8, 32] {
        host_calls(&tracer, threads, "enabled");
    }
    drop(tracer);
    host_calls(&Tracer::disabled(), 1, "disabled tracer");
}

/// The longest single call seen. Every call is timed on its own here, so the figures include the clock.
#[test]
#[ignore = "benchmark; run with cargo xtask trace-stress"]
fn bench_worst_single_call() {
    let calls: u64 = std::env::var("TRACER_STRESS_CALLS").ok().and_then(|text| text.parse().ok()).unwrap_or(10_000_000);
    let dir = TempDir::new();
    // The worker is kept as busy as it gets: a small queue it must drain all the time, a spool it writes
    // to and reads back, and a send every 20 ms.
    let tracer = Tracer::builder("key", "host", "1.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(Instant200)
        .send_before_notice(true)
        .listen_all::<AppEvent>()
        .signals(&Signal::ALL)
        .configure(|config| {
            config.tick_interval = Duration::from_millis(10);
            config.flush_interval = Duration::from_millis(20);
            config.perf_window = Duration::from_millis(100);
            config.heartbeat_interval = Duration::from_millis(50);
        })
        .init();
    let counter = tracer.counter("bench_counter");
    println!("\n### Longest single call over {calls} calls each, worker flushing every 20 ms\n");
    println!("| {:<28} | longest µs | over 100 µs | over 1 ms |", "call");
    println!("|{}|------------|-------------|-----------|", "-".repeat(30));
    let worst = |name: &str, call: &dyn Fn()| {
        let (mut longest, mut over_100us, mut over_1ms) = (Duration::ZERO, 0u64, 0u64);
        for _ in 0..calls {
            let started = Instant::now();
            call();
            let took = started.elapsed();
            longest = longest.max(took);
            over_100us += u64::from(took > Duration::from_micros(100));
            over_1ms += u64::from(took > Duration::from_millis(1));
        }
        println!("| {name:<28} | {:>10.1} | {over_100us:>11} | {over_1ms:>9} |", longest.as_nanos() as f64 / 1000.0);
    };
    worst("track", &|| tracer.track(AppEvent::FileOpen));
    worst("track_with 4 props", &|| {
        tracer.track_with(
            AppEvent::FileSave,
            &[("count", Prop::Int(3)), ("format", Prop::Str("native")), ("ratio", Prop::Float(0.5)), ("recent", Prop::Bool(true))],
        );
    });
    worst("frame", &|| tracer.frame(Duration::from_micros(4_200)));
    worst("begin_turn + end_turn", &|| {
        tracer.begin_turn();
        tracer.end_turn();
    });
    worst("note_input", &|| tracer.note_input());
    worst("Counter::inc", &|| counter.inc());
    worst("(the clock alone)", &|| {});
}

#[test]
#[ignore = "benchmark; run with cargo xtask trace-stress"]
fn bench_worker_throughput() {
    println!("\n### Worker throughput\n");
    for (label, endpoint) in [("queue to spool (nothing sent)", false), ("queue to spool to an instant transport", true)] {
        let dir = TempDir::new();
        let mut builder = Tracer::builder("key", "host", "1.0.0")
            .data_dir(dir.path())
            .transport(Instant200)
            .send_before_notice(true)
            .listen_all::<AppEvent>()
            .signals(&[Signal::Usage])
            .configure(|config| {
                config.queue_capacity = 1 << 20;
                config.spool_max_bytes = 1 << 30;
            });
        if endpoint {
            builder = builder.endpoint("https://telemetry.example.com");
        }
        let tracer = builder.init();
        let items = 500_000;
        let started = Instant::now();
        for _ in 0..items {
            tracer.track(AppEvent::FileOpen);
        }
        let queued = started.elapsed();
        tracer.flush_blocking(Duration::from_secs(300));
        let done = started.elapsed();
        println!("- {label}: {items} items queued in {queued:.2?}, all handled in {done:.2?}: {:.0} items/s", f64::from(items) / done.as_secs_f64());
    }
}

// ---- startup and shutdown ----

/// Leaves `count` session folders behind as dead processes would, each with one small segment, plus
/// `extra_bytes` of items in one more folder.
fn leave_folders(dir: &TempDir, count: usize, extra_bytes: usize) {
    let install_id = {
        let tracer = Tracer::builder("key", "host", "1.0.0").data_dir(dir.path()).init();
        tracer.install_id()
    };
    let line = r#"u{"id":"0123456789abcdef0123456789abcdef","ts":1700000000000,"type":"event","name":"file.open","category":"file"}"#;
    let write = |index: usize, bytes: usize| {
        let session = format!("{index:032x}");
        let folder = dir.spool().join(&session);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("lock"), b"").unwrap();
        let header = format!(
            r#"{{"schema":1,"app":{{"name":"host","version":"1.0.0","process":"gui"}},"install_id":"{install_id}","session_id":"{session}","created":{index}}}"#
        );
        std::fs::write(folder.join("header.json"), header).unwrap();
        let mut segment = String::new();
        let mut seq = 0;
        let mut written = 0;
        while written < bytes.max(1) {
            segment.push_str(line);
            segment.push('\n');
            written += line.len() + 1;
            if segment.len() >= 200 * 1024 || written >= bytes.max(1) {
                std::fs::write(folder.join(format!("{seq:08}.jsonl")), &segment).unwrap();
                segment.clear();
                seq += 1;
            }
        }
    };
    for index in 0..count {
        write(index + 1, 1);
    }
    if extra_bytes > 0 {
        write(count + 1, extra_bytes);
    }
}

fn timed_init(dir: &TempDir, signals: &[Signal]) -> (Duration, Tracer) {
    let started = Instant::now();
    let tracer = Tracer::builder("key", "host", "1.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(Instant200)
        .send_before_notice(true)
        .listen_all::<AppEvent>()
        .signals(signals)
        .init();
    (started.elapsed(), tracer)
}

#[test]
#[ignore = "benchmark; run with cargo xtask trace-stress"]
fn bench_init_and_shutdown() {
    println!("\n### `Builder::init` wall time, median of 5, and the first flush after it\n");
    println!("| {:<52} | init ms   | first flush ms |", "state of the data directory");
    println!("|{}|-----------|----------------|", "-".repeat(54));
    let cases: [(&str, usize, usize); 5] = [
        ("empty (first run ever)", 0, 0),
        ("one clean earlier run", 0, 0),
        ("100 leftover session folders", 100, 0),
        ("1,000 leftover session folders", 1_000, 0),
        ("a full 8 MiB spool in one folder", 0, 8 * 1024 * 1024),
    ];
    for (index, (label, folders, bytes)) in cases.into_iter().enumerate() {
        let mut inits = Vec::new();
        let mut flushes = Vec::new();
        for _ in 0..5 {
            let dir = TempDir::new();
            if index > 0 {
                leave_folders(&dir, folders, bytes);
            }
            let (init, tracer) = timed_init(&dir, &Signal::ALL);
            let started = Instant::now();
            tracer.flush_blocking(Duration::from_secs(120));
            flushes.push(started.elapsed());
            inits.push(init);
            drop(tracer);
        }
        inits.sort();
        flushes.sort();
        println!("| {label:<52} | {:>9.2} | {:>14.1} |", inits[2].as_secs_f64() * 1000.0, flushes[2].as_secs_f64() * 1000.0);
    }
    let dir = TempDir::new();
    let (without_sys, tracer) = timed_init(&dir, &[Signal::Session, Signal::Usage, Signal::Crash, Signal::Hang, Signal::Perf, Signal::Logs]);
    drop(tracer);
    println!("\n- init on an empty directory without the `sys` signal: {:.2} ms", without_sys.as_secs_f64() * 1000.0);

    println!("\n### Shutdown (dropping the last `Tracer`), `shutdown_timeout` 3 s\n");
    println!("| {:<40} | queue  | drop ms  |", "server");
    println!("|{}|--------|----------|", "-".repeat(42));
    for (label, delay, fail) in
        [("reachable, instant", 0u64, false), ("slow, 1 s per request", 1_000, false), ("unreachable (refuses at once)", 0, true)]
    {
        for full in [false, true] {
            let dir = TempDir::new();
            let tracer = Tracer::builder("key", "host", "1.0.0")
                .data_dir(dir.path())
                .endpoint("https://telemetry.example.com")
                .transport(support::Slow { delay: Duration::from_millis(delay), fail })
                .send_before_notice(true)
                .listen_all::<AppEvent>()
                .init();
            std::thread::sleep(Duration::from_millis(if delay > 0 { 1_200 } else { 50 }));
            if full {
                for _ in 0..1_024 {
                    tracer.track(AppEvent::FileOpen);
                }
            }
            let started = Instant::now();
            drop(tracer);
            println!("| {label:<40} | {:<6} | {:>8.1} |", if full { "full" } else { "empty" }, started.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

// ---- idle cost and memory ----

/// This process's CPU time so far and its resident memory.
fn process_usage() -> (Duration, u64) {
    let mut system = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(std::process::id());
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );
    let process = system.process(pid).unwrap();
    (Duration::from_millis(process.accumulated_cpu_time()), process.memory())
}

fn seconds(variable: &str, default: u64) -> Duration {
    Duration::from_secs(std::env::var(variable).ok().and_then(|text| text.parse().ok()).unwrap_or(default))
}

#[test]
#[ignore = "benchmark; run with cargo xtask trace-stress"]
fn bench_idle_and_steady_cpu() {
    let length = seconds("TRACER_STRESS_IDLE_SECS", 30);
    println!("\n### CPU used by the whole process over {length:?}, host thread asleep or pacing\n");
    let dir = TempDir::new();
    let (_, tracer) = timed_init(&dir, &Signal::ALL);
    tracer.begin_turn();
    tracer.end_turn();
    std::thread::sleep(Duration::from_secs(2));
    let (before, _) = process_usage();
    std::thread::sleep(length);
    let (after, _) = process_usage();
    let idle = (after - before).as_secs_f64() / length.as_secs_f64() * 100.0;
    println!("- idle app (no calls; worker and watchdog threads only): {idle:.3}% of one core");

    let (before, _) = process_usage();
    let started = Instant::now();
    let mut frames = 0u64;
    while started.elapsed() < length {
        {
            let _turn = tracer.turn();
            tracer.frame(Duration::from_micros(4_000));
            tracer.note_input();
            if frames % 12 < 5 {
                tracer.track(AppEvent::FileOpen);
            }
            frames += 1;
        }
        std::thread::sleep(Duration::from_micros(8_333));
    }
    let (after, _) = process_usage();
    let steady = (after - before).as_secs_f64() / length.as_secs_f64() * 100.0;
    let per_second = frames as f64 / length.as_secs_f64();
    println!("- {per_second:.0} frames/s with {:.0} events/s, including the pacing loop itself: {steady:.3}% of one core", per_second * 5.0 / 12.0);
}

#[test]
#[ignore = "benchmark; run with cargo xtask trace-stress"]
fn bench_memory_stays_flat_with_the_server_unreachable() {
    let length = seconds("TRACER_STRESS_MEMORY_SECS", 300);
    println!("\n### Resident memory, 120 frames/s and 50 events/s, server unreachable, spool cap 64 KiB, over {length:?}\n");
    let dir = TempDir::new();
    let tracer = Tracer::builder("key", "host", "1.0.0")
        .data_dir(dir.path())
        .endpoint("https://telemetry.example.com")
        .transport(support::Slow { delay: Duration::ZERO, fail: true })
        .send_before_notice(true)
        .listen_all::<AppEvent>()
        .configure(|config| {
            // A small cap and short intervals, so that the steady state (spool full, evicting) is reached
            // in the first seconds and the rest of the run can only show growth.
            config.spool_max_bytes = 64 * 1024;
            config.flush_interval = Duration::from_secs(1);
            config.perf_window = Duration::from_secs(1);
            config.retry_min = Duration::from_millis(200);
            config.retry_max = Duration::from_secs(2);
        })
        .init();
    let logger = tracer.logger(None);
    let started = Instant::now();
    let mut samples = Vec::new();
    let mut next_sample = Duration::ZERO;
    let mut frames = 0u64;
    while started.elapsed() < length {
        {
            let _turn = tracer.turn();
            tracer.frame(Duration::from_micros(4_000));
            tracer.note_input();
            if frames % 12 < 5 {
                tracer.track_with(AppEvent::FileSave, &[("count", Prop::Int(3)), ("format", Prop::Str("native"))]);
            }
            if frames.is_multiple_of(120) {
                logger.log(&log::Record::builder().level(log::Level::Info).target("host").args(format_args!("frame {frames}")).build());
            }
            frames += 1;
        }
        if started.elapsed() >= next_sample {
            samples.push((started.elapsed(), process_usage().1, support::disk_bytes(&dir.spool())));
            next_sample += length / 10;
        }
        std::thread::sleep(Duration::from_micros(8_333));
    }
    println!("| at s   | resident MiB | spool KiB |");
    println!("|--------|--------------|-----------|");
    for (at, resident, spool) in &samples {
        println!("| {:>6.0} | {:>12.2} | {:>9.0} |", at.as_secs_f64(), *resident as f64 / 1_048_576.0, *spool as f64 / 1024.0);
    }
    // Growth is judged from the point the steady state has been reached: the third sample on.
    let settled = samples[2].1 as f64;
    let last = samples.last().unwrap().1 as f64;
    println!("\n- resident change from {:.0} s to the end: {:+.2} MiB", samples[2].0.as_secs_f64(), (last - settled) / 1_048_576.0);
    assert!(samples.iter().all(|(_, _, spool)| *spool <= 64 * 1024), "the spool went over its cap");
    assert!(last - settled < 4.0 * 1_048_576.0, "resident memory kept growing");
}

#[test]
#[ignore = "benchmark; run with cargo xtask trace-stress"]
fn bench_worker_throughput_with_many_adopted_folders() {
    let dir = TempDir::new();
    leave_folders(&dir, 1_000, 0);
    // No endpoint: the folders are adopted and stay, so every append has them to account for.
    let tracer = Tracer::builder("key", "host", "1.0.0")
        .data_dir(dir.path())
        .listen_all::<AppEvent>()
        .signals(&[Signal::Usage])
        .configure(|config| {
            config.queue_capacity = 1 << 20;
            config.spool_max_bytes = 1 << 30;
        })
        .init();
    tracer.flush_blocking(Duration::from_secs(60));
    let items = 200_000;
    let started = Instant::now();
    for _ in 0..items {
        tracer.track(AppEvent::FileOpen);
    }
    tracer.flush_blocking(Duration::from_secs(300));
    println!("- queue to spool with 1,000 adopted folders waiting: {:.0} items/s", f64::from(items) / started.elapsed().as_secs_f64());
}

/// What the compression level costs and buys, on a body shaped like a real batch.
#[test]
#[ignore = "benchmark; run with cargo xtask trace-stress"]
fn bench_gzip_levels() {
    use std::io::Write;

    let mut rng = support::Rng(7);
    let mut body = String::from(
        r#"{"schema":1,"app":{"name":"host","version":"1.0.0","process":"gui"},"install_id":"0123456789abcdef0123456789abcdef","session_id":"fedcba9876543210fedcba9876543210","sent_at":1700000000000,"items":["#,
    );
    let names = ["file.open", "file.save", "edit.undo", "view.zoom", "tool.brush", "layer.add"];
    while body.len() < 1_000_000 {
        let name = names[rng.below(names.len() as u64) as usize];
        body.push_str(&format!(
            r#"{{"id":"{:016x}{:016x}","ts":{},"type":"event","name":"{name}","category":"{}","props":{{"count":{}}}}},"#,
            rng.next(),
            rng.next(),
            1_700_000_000_000 + rng.below(100_000),
            name.split('.').next().unwrap(),
            rng.below(50)
        ));
    }
    body.push_str("]}");
    println!("\n### gzip level on a {} KiB batch of events (median of 7)\n", body.len() / 1024);
    println!("| level | ms     | bytes out | ratio |");
    println!("|-------|--------|-----------|-------|");
    for level in [1, 3, 6, 9] {
        let mut times = Vec::new();
        let mut size = 0;
        for _ in 0..7 {
            let started = Instant::now();
            let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(level));
            encoder.write_all(body.as_bytes()).unwrap();
            size = encoder.finish().unwrap().len();
            times.push(started.elapsed());
        }
        times.sort();
        println!("| {level:>5} | {:>6.2} | {size:>9} | {:>5.1} |", times[3].as_secs_f64() * 1000.0, body.len() as f64 / size as f64);
    }
}

// ---- heavy stress ----

fn seeds() -> std::ops::RangeInclusive<u64> {
    let first = std::env::var("TRACER_STRESS_SEED").ok().and_then(|text| text.parse().ok()).unwrap_or(1);
    let runs = std::env::var("TRACER_STRESS_RUNS").ok().and_then(|text| text.parse().ok()).unwrap_or(20);
    first..=first + runs - 1
}

#[test]
#[ignore = "heavy; run with cargo xtask trace-stress"]
fn stress_flood_with_exact_accounting() {
    println!("\n### Flood: 32 threads, 20,000 calls each, misbehaving transport, exact accounting\n");
    let mut statuses = std::collections::BTreeMap::new();
    let (mut delivered, mut requests) = (0, 0);
    for seed in seeds() {
        let tally = support::flood_with_exact_accounting(seed, 32, 20_000);
        delivered += tally.delivered_events;
        requests += tally.requests;
        for (status, count) in tally.statuses {
            *statuses.entry(status).or_insert(0) += count;
        }
    }
    println!("- {} runs passed: {delivered} events delivered in {requests} requests; replies by status (0 = timeout): {statuses:?}", seeds().count());
}

#[test]
#[ignore = "heavy; run with cargo xtask trace-stress"]
fn stress_flood_with_consent_and_forget() {
    println!("\n### Flood: 32 threads, 20,000 calls each, with consent changes, forgets and flushes\n");
    let mut requests = 0;
    for seed in seeds() {
        requests += support::flood_with_consent_and_forget(seed, 32, 20_000).requests;
    }
    println!("- {} runs passed, {requests} requests in all", seeds().count());
}

/// Not a test of its own: the part a child process plays in the many-process test.
#[test]
#[ignore = "run as a child process by stress_many_processes_on_one_data_directory"]
fn stress_child_process() {
    support::child_process();
}

#[test]
#[ignore = "heavy; run with cargo xtask trace-stress"]
fn stress_many_processes_on_one_data_directory() {
    println!("\n### 20 real processes on one data directory, some killed\n");
    for seed in seeds() {
        support::many_processes(seed, 20, "stress_child_process");
    }
    println!("- {} runs passed", seeds().count());
}

/// Threads of this process, where they can be counted.
fn thread_count() -> Option<usize> {
    let output = std::process::Command::new("ps").args(["-M", "-p", &std::process::id().to_string()]).output().ok()?;
    let lines = String::from_utf8_lossy(&output.stdout).lines().count();
    (output.status.success() && lines > 1).then(|| lines - 1)
}

#[test]
#[ignore = "heavy; run with cargo xtask trace-stress"]
fn stress_a_thousand_starts_and_stops_leak_nothing() {
    println!("\n### 1,000 starts and stops in one process\n");
    let dir = TempDir::new();
    let run = |flush: bool| {
        let (_, tracer) = timed_init(&dir, &Signal::ALL);
        tracer.track(AppEvent::FileOpen);
        drop(tracer.turn());
        if flush {
            assert!(tracer.flush_blocking(Duration::from_secs(60)));
        }
    };
    for _ in 0..5 {
        run(true);
    }
    let open_files = || std::fs::read_dir("/dev/fd").ok().map(Iterator::count);
    let (files, threads, (_, resident)) = (open_files(), thread_count(), process_usage());
    let started = Instant::now();
    for round in 0..1_000 {
        run(round % 50 == 0);
    }
    let took = started.elapsed();
    let (files_after, threads_after, (_, resident_after)) = (open_files(), thread_count(), process_usage());
    println!(
        "- took {took:.2?}; open files {files:?} -> {files_after:?}; threads {threads:?} -> {threads_after:?}; resident {:.1} -> {:.1} MiB",
        resident as f64 / 1_048_576.0,
        resident_after as f64 / 1_048_576.0
    );
    if let (Some(before), Some(after)) = (files, files_after) {
        assert!(after <= before + 2, "open files grew from {before} to {after}");
    }
    if let (Some(before), Some(after)) = (threads, threads_after) {
        assert!(after <= before + 1, "threads grew from {before} to {after}");
    }
    assert!(resident_after < resident + 16 * 1024 * 1024, "resident memory grew by {} KiB", (resident_after - resident) / 1024);
    assert_eq!(std::fs::read_dir(dir.spool()).unwrap().count(), 0, "session folders were left behind");
}
