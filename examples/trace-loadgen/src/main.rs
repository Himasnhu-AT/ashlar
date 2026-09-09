//! `loadgen`: drives a tracer-server over real HTTP with realistic envelopes, seeds large databases and
//! measures the dashboard's read path.
//!
//! ```text
//! loadgen run  [--spawn | --url U --key K [--db PATH]] [--concurrency N] [--duration S | --requests N]
//!              [--rate R | --phases "R:S,R:S,max:S"] [--installs N] [--versions N] [--seed N]
//!              [--sample S] [--probe] [--env KEY=VALUE]...
//! loadgen seed --db PATH [--scale F]
//! loadgen read --dir DIR
//! ```

mod read;
mod run;
mod seed;
mod spawn;
mod synth;

use std::collections::HashMap;
use std::path::PathBuf;

use run::{Phase, RunArgs};
use synth::Profile;

struct Opts {
    values: HashMap<String, Vec<String>>,
    flags: Vec<String>,
}

impl Opts {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut values: HashMap<String, Vec<String>> = HashMap::new();
        let mut flags = Vec::new();
        let mut iter = args.iter().peekable();
        while let Some(arg) = iter.next() {
            let name = arg.strip_prefix("--").ok_or(format!("unexpected argument {arg}"))?.to_string();
            if matches!(name.as_str(), "spawn" | "probe" | "keep") {
                flags.push(name);
            } else {
                values.entry(name.clone()).or_default().push(iter.next().ok_or(format!("--{name} needs a value"))?.clone());
            }
        }
        Ok(Self { values, flags })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).and_then(|v| v.last()).map(String::as_str)
    }

    fn num<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.get(name) {
            Some(text) => text.parse().map_err(|_| format!("--{name}: cannot parse {text:?}")),
            None => Ok(default),
        }
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }
}

fn phases(opts: &Opts) -> Result<Vec<Phase>, String> {
    if let Some(spec) = opts.get("phases") {
        return spec
            .split(',')
            .map(|part| {
                let (rate, secs) = part.split_once(':').ok_or("phases look like RATE:SECONDS,RATE:SECONDS")?;
                let rate = if rate == "max" { None } else { Some(rate.parse::<f64>().map_err(|_| "bad rate")?) };
                Ok(Phase { rate, secs: secs.parse::<f64>().map_err(|_| "bad seconds")? })
            })
            .collect();
    }
    let secs = opts.num("duration", 10.0)?;
    Ok(vec![Phase { rate: opts.get("rate").map(|r| r.parse().unwrap_or(500.0)), secs }])
}

fn cmd_run(opts: &Opts) -> Result<bool, String> {
    let env: Vec<(String, String)> =
        opts.values.get("env").into_iter().flatten().filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
    let mut spawned = if opts.flag("spawn") { Some(spawn::spawn(opts.get("dir").map(PathBuf::from), &env)?) } else { None };
    let (url, key, db, pid) = match &spawned {
        Some(s) => (s.url.clone(), s.key.clone(), Some(s.db.clone()), Some(s.pid())),
        None => (
            opts.get("url").ok_or("--url or --spawn is required")?.to_string(),
            opts.get("key").ok_or("--key is required with --url")?.to_string(),
            opts.get("db").map(PathBuf::from),
            None,
        ),
    };
    let args = RunArgs {
        url,
        key,
        concurrency: opts.num("concurrency", 64)?,
        phases: phases(opts)?,
        max_requests: opts.get("requests").and_then(|r| r.parse().ok()),
        profile: Profile {
            seed: opts.num("seed", 1)?,
            installs: opts.num("installs", 20_000)?,
            versions: opts.num("versions", 5)?,
            crash_permille: 50,
        },
        db,
        server_pid: pid,
        sample_secs: opts.num("sample", 30.0)?,
        probe: opts.flag("probe"),
    };
    let report = run::run(&args);
    if let Some(s) = spawned.as_mut() {
        s.stop();
    }
    Ok(report.ok)
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (command, rest) = match args.split_first() {
        Some((c, rest)) if !c.starts_with("--") => (c.as_str(), rest),
        _ => ("run", args.as_slice()),
    };
    let result = Opts::parse(rest).and_then(|opts| match command {
        "run" => cmd_run(&opts),
        "seed" => seed::command(&opts_view(&opts)).map(|()| true),
        "read" => read::command(&opts_view(&opts)).map(|()| true),
        other => Err(format!("unknown command {other}")),
    });
    match result {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::from(1),
        Err(error) => {
            eprintln!("loadgen: {error}");
            std::process::ExitCode::from(2)
        }
    }
}

/// The subset of options the seed and read commands use, as plain `(name, value)` pairs.
fn opts_view(opts: &Opts) -> HashMap<String, String> {
    opts.values
        .iter()
        .filter_map(|(k, v)| v.last().map(|v| (k.clone(), v.clone())))
        .chain(opts.flags.iter().map(|f| (f.clone(), "1".to_string())))
        .collect()
}
