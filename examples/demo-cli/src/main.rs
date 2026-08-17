//! A scriptable host process for the Tracer SDK.
//!
//! It behaves like a short-lived host app: it declares an event catalog, starts the SDK, does a little
//! work and exits in a way chosen on the command line. The end-to-end tests run it as a child process and
//! look at what the server stored. Everything it says goes to stderr; stdout stays empty.
//!
//! ```text
//! demo-cli <scenario> --data-dir DIR [--endpoint URL] [--key KEY] [--version V] [options]
//! ```
//!
//! Scenarios: `normal`, `panic`, `abort`, `hang`, `child`, `relaunch`, `forget`.

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use tracer_sdk::proto::{ConsentCategory, ProcessKind};
use tracer_sdk::{EventCategory, LoggerMode, Prop, Tracer};

tracer_sdk::events! {
    pub enum Category {
        File = "file",
        Edit = "edit",
        View = "view",
    }
    pub enum DemoEvent {
        FileOpen = "file.open" in File,
        FileSave = "file.save" in File,
        EditUndo = "edit.undo" in Edit,
        ViewZoom = "view.zoom" in View,
    }
}

const USAGE: &str = "usage: demo-cli <normal|panic|abort|hang|child|relaunch|forget> --data-dir DIR [options]

options:
  --endpoint URL        server base URL (default: send nothing)
  --key KEY             write key
  --version V           app version (default 1.0.0)
  --build ID            build identifier
  --process KIND        gui | cli | service | child (default gui)
  --no-usage            switch the usage category off after start
  --no-crash            switch the crash category off after start
  --no-perf             switch the performance category off after start
  --wrap-logger         keep the host's own logger and wrap it, instead of LoggerMode::Install
  --listen A,B         listen to these categories only (default: all)
  --tick-ms N           worker tick (default: SDK default)
  --heartbeat-ms N      session heartbeat interval
  --perf-window-ms N    performance window
  --flush-ms N          send interval
  --hang-ms N           hang threshold
  --retry-ms N          shortest retry wait
  --run-ms N            how long a scenario keeps its loop going
  --flush-timeout-ms N  how long the final flush may take (default 5000)
  --fast                small intervals for tests: tick 20, heartbeat 100, window 300, flush 100, hang 400, retry 100";

#[derive(Debug, Default)]
struct Args {
    scenario: String,
    data_dir: PathBuf,
    endpoint: Option<String>,
    key: String,
    version: String,
    build: Option<String>,
    process: Option<ProcessKind>,
    off: Vec<ConsentCategory>,
    wrap_logger: bool,
    listen: Option<Vec<String>>,
    tick: Option<Duration>,
    heartbeat: Option<Duration>,
    perf_window: Option<Duration>,
    flush: Option<Duration>,
    hang: Option<Duration>,
    retry: Option<Duration>,
    run: Option<Duration>,
    flush_timeout: Duration,
}

fn parse(raw: Vec<String>) -> Result<Args, String> {
    let mut args = Args { version: "1.0.0".to_owned(), key: "tk_unset".to_owned(), flush_timeout: Duration::from_secs(5), ..Args::default() };
    let mut iter = raw.into_iter();
    args.scenario = iter.next().ok_or("missing scenario")?;
    while let Some(flag) = iter.next() {
        let mut value = |name: &str| iter.next().ok_or_else(|| format!("{name} needs a value"));
        let millis = |text: String| text.parse::<u64>().map(Duration::from_millis).map_err(|_| format!("{flag}: expected milliseconds"));
        match flag.as_str() {
            "--data-dir" => args.data_dir = PathBuf::from(value(&flag)?),
            "--endpoint" => args.endpoint = Some(value(&flag)?),
            "--key" => args.key = value(&flag)?,
            "--version" => args.version = value(&flag)?,
            "--build" => args.build = Some(value(&flag)?),
            "--process" => {
                args.process = Some(match value(&flag)?.as_str() {
                    "gui" => ProcessKind::Gui,
                    "cli" => ProcessKind::Cli,
                    "service" => ProcessKind::Service,
                    "child" => ProcessKind::Child,
                    other => return Err(format!("unknown process kind {other}")),
                })
            }
            "--no-usage" => args.off.push(ConsentCategory::Usage),
            "--no-crash" => args.off.push(ConsentCategory::Crash),
            "--no-perf" => args.off.push(ConsentCategory::Performance),
            "--wrap-logger" => args.wrap_logger = true,
            "--listen" => args.listen = Some(value(&flag)?.split(',').filter(|name| !name.is_empty()).map(str::to_owned).collect()),
            "--tick-ms" => args.tick = Some(millis(value(&flag)?)?),
            "--heartbeat-ms" => args.heartbeat = Some(millis(value(&flag)?)?),
            "--perf-window-ms" => args.perf_window = Some(millis(value(&flag)?)?),
            "--flush-ms" => args.flush = Some(millis(value(&flag)?)?),
            "--hang-ms" => args.hang = Some(millis(value(&flag)?)?),
            "--retry-ms" => args.retry = Some(millis(value(&flag)?)?),
            "--run-ms" => args.run = Some(millis(value(&flag)?)?),
            "--flush-timeout-ms" => args.flush_timeout = millis(value(&flag)?)?,
            "--fast" => {
                args.tick = Some(Duration::from_millis(20));
                args.heartbeat = Some(Duration::from_millis(100));
                args.perf_window = Some(Duration::from_millis(300));
                args.flush = Some(Duration::from_millis(100));
                args.hang = Some(Duration::from_millis(400));
                args.retry = Some(Duration::from_millis(100));
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if args.data_dir.as_os_str().is_empty() {
        return Err("--data-dir is required".to_owned());
    }
    Ok(args)
}

/// Stands in for the logger a host already has.
struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        eprintln!("[{}] {}", record.level(), record.args());
    }

    fn flush(&self) {}
}

/// The category named `name`, if the catalog has one.
fn category_named(name: &str) -> Option<Category> {
    Category::ALL.iter().copied().find(|category| category.name() == name)
}

/// What a host does once at startup: catalog, listening, intervals, logger.
fn start(args: &Args) -> Result<Tracer, String> {
    let mut builder = Tracer::builder(&args.key, "demo", &args.version)
        .data_dir(&args.data_dir)
        .catalog::<DemoEvent>()
        .logger(if args.wrap_logger { LoggerMode::None } else { LoggerMode::Install })
        .process(args.process.unwrap_or(ProcessKind::Gui));
    if let Some(endpoint) = &args.endpoint {
        builder = builder.endpoint(endpoint);
    }
    if let Some(build) = &args.build {
        builder = builder.build(build);
    }
    builder = match &args.listen {
        None => builder.listen_all::<DemoEvent>(),
        Some(names) => {
            for name in names {
                builder = builder.listen(category_named(name).ok_or_else(|| format!("unknown category {name}"))?);
            }
            builder
        }
    };
    builder = builder.configure(|config| {
        let set = |slot: &mut Duration, value: Option<Duration>| {
            if let Some(value) = value {
                *slot = value;
            }
        };
        set(&mut config.tick_interval, args.tick);
        set(&mut config.heartbeat_interval, args.heartbeat);
        set(&mut config.perf_window, args.perf_window);
        set(&mut config.flush_interval, args.flush);
        set(&mut config.hang_threshold, args.hang);
        set(&mut config.retry_min, args.retry);
        set(&mut config.sys_interval, args.perf_window);
    });
    let tracer = builder.init();
    if args.wrap_logger {
        // A host with its own global logger wraps it, then installs the wrapper in its place.
        let wrapped = tracer.logger(Some(Box::new(StderrLogger)));
        if log::set_boxed_logger(wrapped).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
    }
    for category in &args.off {
        tracer.set_consent(*category, false);
    }
    Ok(tracer)
}

/// Keeps the "main loop" alive for `total`, beating the hang watchdog like a GUI would.
fn run_loop(tracer: &Tracer, total: Duration) {
    let started = Instant::now();
    while started.elapsed() < total {
        tracer.heartbeat();
        tracer.note_input();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn some_work(tracer: &Tracer) {
    tracer.first_frame();
    tracer.track(DemoEvent::FileOpen);
    log::info!("opened a document");
    tracer.heartbeat();
}

fn normal(tracer: &Tracer, args: &Args) {
    some_work(tracer);
    tracer.track_with(
        DemoEvent::FileSave,
        &[("count", Prop::Int(3)), ("format", Prop::Str("native")), ("ratio", Prop::Float(0.5)), ("recent", Prop::Bool(true))],
    );
    tracer.track(DemoEvent::EditUndo);
    tracer.track(DemoEvent::ViewZoom);
    tracer.track_id(Category::Edit, "edit.palette_command");

    for _ in 0..40 {
        tracer.frame(Duration::from_millis(5));
    }
    tracer.frame(Duration::from_millis(30)); // slow
    tracer.frame(Duration::from_millis(800)); // frozen

    let saves = tracer.counter("saves");
    saves.add(7);
    let layers = tracer.gauge("layers");
    layers.set(4.0);

    log::info!("saved a document");
    log::warn!("disk is almost full");
    let failure = std::io::Error::other("could not write the preview");
    tracer.report_handled(&failure, Some("preview.write"));

    run_loop(tracer, args.run.unwrap_or(Duration::from_millis(1300)));
}

/// The deliberate crash of the `panic` scenario.
#[allow(clippy::panic, reason = "this scenario exists to crash the host on purpose")]
fn crash_on_purpose() -> ! {
    panic!("demo panic in the main loop")
}

fn hang(tracer: &Tracer, args: &Args) {
    some_work(tracer);
    for _ in 0..5 {
        tracer.heartbeat();
        std::thread::sleep(Duration::from_millis(30));
    }
    let threshold = args.hang.unwrap_or(Duration::from_secs(2));
    log::info!("blocking the main loop");
    std::thread::sleep(threshold * 3);
    tracer.heartbeat();
    std::thread::sleep(Duration::from_millis(100));
    tracer.heartbeat();
}

fn child(tracer: &Tracer, args: &Args, raw: &[String]) -> Result<(), String> {
    some_work(tracer);
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    // The child gets the parent's options with its own scenario and process kind.
    let mut command = Command::new(exe);
    command.arg("abort").args(raw.iter().skip(1)).args(["--process", "child"]);
    command.stdout(std::process::Stdio::null());
    let status = command.status().map_err(|error| error.to_string())?;
    eprintln!("demo-cli: child ended with {status}");
    tracer.report_child_exit("demo-child", status);
    run_loop(tracer, args.run.unwrap_or(Duration::from_millis(200)));
    Ok(())
}

fn real_main() -> Result<(), String> {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = parse(raw.clone())?;
    let tracer = start(&args)?;
    eprintln!("demo-cli: scenario {} install {} session {}", args.scenario, tracer.install_id(), tracer.session_id());
    match args.scenario.as_str() {
        "normal" => normal(&tracer, &args),
        "panic" => {
            some_work(&tracer);
            tracer.frame(Duration::from_millis(4));
            log::warn!("about to fail");
            run_loop(&tracer, Duration::from_millis(100));
            crash_on_purpose();
        }
        "abort" => {
            some_work(&tracer);
            log::warn!("about to abort");
            // Give the worker a few ticks to save the breadcrumbs, as a real host would have by now.
            let wait = args.tick.unwrap_or(Duration::from_secs(1)) * 6 + Duration::from_millis(100);
            run_loop(&tracer, wait);
            std::process::abort();
        }
        "hang" => hang(&tracer, &args),
        "child" => child(&tracer, &args, &raw)?,
        // Just start: the SDK sends whatever an earlier run left behind.
        "relaunch" => {}
        "forget" => {
            some_work(&tracer);
            tracer.flush_blocking(args.flush_timeout);
            tracer.forget();
            eprintln!("demo-cli: forgot, new install {}", tracer.install_id());
            run_loop(&tracer, Duration::from_millis(100));
        }
        other => return Err(format!("unknown scenario {other}")),
    }
    if !tracer.flush_blocking(args.flush_timeout) {
        eprintln!("demo-cli: flush timed out, data stays on disk");
    }
    // Dropping the last handle ends the session and sends what is left.
    drop(tracer);
    Ok(())
}

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("demo-cli: {message}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tracer_sdk::Tracer;

    use super::{DemoEvent, normal, parse};

    #[test]
    fn a_disabled_tracer_accepts_every_call() {
        // What a host passes to its own tests: no files, no threads, nothing recorded.
        let tracer = Tracer::disabled();
        assert!(!tracer.is_enabled());
        let args = parse(vec!["normal".into(), "--data-dir".into(), "unused".into(), "--run-ms".into(), "0".into()]).unwrap_or_default();
        normal(&tracer, &args);
        tracer.track(DemoEvent::FileOpen);
        assert!(tracer.flush_blocking(Duration::from_millis(1)));
        assert_eq!(tracer.install_id(), "");
    }
}
