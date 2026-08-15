//! Subcommands. Argument parsing is hand-rolled: the grammar is tiny.

use std::io::{BufRead, Write};
use std::sync::Arc;

use argon2::password_hash::{PasswordHash, PasswordVerifier, SaltString};
use argon2::{Argon2, PasswordHasher};

use crate::config::{Config, ConfigError};
use crate::http::{self, AppState};
use crate::jobs;
use crate::store::{SqliteStore, Store, StoreError};

pub const USAGE: &str = "usage: tracer-server <command>

commands:
  serve                       run the HTTP server, rollup timer and retention timer
  app create <slug> <name>    create an app and print its first write key
  app list                    list apps
  key create <slug>           print a new write key for an app
  key revoke <key>            revoke a write key
  admin create <username>     create a dashboard login (password from TRACER_ADMIN_PASSWORD or stdin)
  prune                       run retention once
  rollup [--days N]           recompute rollups once (default 3 days)

settings come from TRACER_* environment variables; see docs/architecture.md section 4.1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Serve,
    AppCreate { slug: String, name: String },
    AppList,
    KeyCreate { slug: String },
    KeyRevoke { key: String },
    AdminCreate { username: String },
    Prune,
    Rollup { days: u32 },
    Help,
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

fn usage(message: &str) -> CliError {
    CliError::Usage(message.to_string())
}

/// Parses the arguments after the program name.
pub fn parse(args: &[String]) -> Result<Command, CliError> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["help" | "--help" | "-h"] => Ok(Command::Help),
        ["serve"] => Ok(Command::Serve),
        ["app", "create", slug, name] => Ok(Command::AppCreate { slug: slug.to_string(), name: name.to_string() }),
        ["app", "create", ..] => Err(usage("usage: app create <slug> <name>")),
        ["app", "list"] => Ok(Command::AppList),
        ["key", "create", slug] => Ok(Command::KeyCreate { slug: slug.to_string() }),
        ["key", "revoke", key] => Ok(Command::KeyRevoke { key: key.to_string() }),
        ["admin", "create", username] => Ok(Command::AdminCreate { username: username.to_string() }),
        ["prune"] => Ok(Command::Prune),
        ["rollup"] => Ok(Command::Rollup { days: jobs::ROLLUP_DAYS }),
        ["rollup", "--days", days] => match days.parse::<u32>() {
            Ok(days) if days >= 1 => Ok(Command::Rollup { days }),
            _ => Err(usage("--days takes a whole number of at least 1")),
        },
        other => Err(CliError::Usage(format!("unknown command: {}\n\n{USAGE}", other.join(" ")))),
    }
}

/// Runs a command. `out` receives normal output; `stdin` is read only by `admin create`.
pub fn execute(command: Command, config: &Config, out: &mut dyn Write, stdin: &mut dyn BufRead) -> Result<(), CliError> {
    match command {
        Command::Help => writeln!(out, "{USAGE}")?,
        Command::Serve => serve(config)?,
        Command::AppCreate { slug, name } => {
            validate_slug(&slug)?;
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
                return Err(usage("the app name must be 1 to 128 characters"));
            }
            let store = open_store(config)?;
            let key = new_write_key()?;
            let app =
                store.create_app(&slug, name, &key, crate::now_ms()).map_err(|error| conflict_as(error, "an app with that slug already exists"))?;
            writeln!(out, "created app {} ({})", app.slug, app.name)?;
            writeln!(out, "write key: {key}")?;
        }
        Command::AppList => {
            let store = open_store(config)?;
            for app in store.list_apps()? {
                writeln!(out, "{}\t{}", app.slug, app.name)?;
            }
        }
        Command::KeyCreate { slug } => {
            let store = open_store(config)?;
            let app = store.find_app(&slug)?.ok_or_else(|| usage("no app with that slug"))?;
            let key = new_write_key()?;
            store.create_key(app.id, &key, crate::now_ms())?;
            writeln!(out, "write key: {key}")?;
        }
        Command::KeyRevoke { key } => {
            let store = open_store(config)?;
            if store.revoke_key(&key, crate::now_ms())? {
                writeln!(out, "revoked")?;
            } else {
                return Err(usage("no active key matched"));
            }
        }
        Command::AdminCreate { username } => {
            let username = username.trim();
            if username.is_empty() || username.chars().count() > 64 || username.chars().any(|c| c.is_control() || c.is_whitespace()) {
                return Err(usage("the username must be 1 to 64 characters without spaces"));
            }
            let password = match config.admin_password.clone() {
                Some(password) => password,
                None => {
                    let mut line = String::new();
                    stdin.read_line(&mut line)?;
                    line.trim_end_matches(['\r', '\n']).to_string()
                }
            };
            if password.chars().count() < 8 {
                return Err(usage("the password must be at least 8 characters"));
            }
            let hash = hash_password(&password)?;
            let store = open_store(config)?;
            store.create_admin(username, &hash, crate::now_ms()).map_err(|error| conflict_as(error, "that username is taken"))?;
            writeln!(out, "created admin {username}")?;
        }
        Command::Prune => {
            let store = open_store(config)?;
            let stats = jobs::run_retention(store.as_ref(), crate::now_ms(), config.retention_days)?;
            writeln!(
                out,
                "removed {} events, {} perf rows, {} crashes, {} sessions, {} installs, {} issues, {} dedup ids",
                stats.events, stats.perf, stats.crashes, stats.sessions, stats.installs, stats.issues, stats.items_seen
            )?;
        }
        Command::Rollup { days } => {
            let store = open_store(config)?;
            jobs::close_stale_sessions(store.as_ref(), crate::now_ms())?;
            jobs::rollup_recent(store.as_ref(), crate::now_ms(), days, config.retention_days)?;
            writeln!(out, "rollups recomputed for the last {days} days")?;
        }
    }
    Ok(())
}

fn conflict_as(error: StoreError, message: &str) -> CliError {
    match error {
        StoreError::Conflict(_) => usage(message),
        other => CliError::Store(other),
    }
}

fn open_store(config: &Config) -> Result<Arc<dyn Store>, CliError> {
    Ok(Arc::new(SqliteStore::open(&config.db_path())?))
}

fn validate_slug(slug: &str) -> Result<(), CliError> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok { Ok(()) } else { Err(usage("the slug must be lower-case letters, digits, '-' or '_', up to 64 characters")) }
}

fn serve(config: &Config) -> Result<(), CliError> {
    let store = open_store(config)?;
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(config.bind).await?;
        log::info!("tracer-server {} listening on {}", env!("CARGO_PKG_VERSION"), listener.local_addr()?);
        let timers = jobs::spawn_timers(Arc::clone(&store), config.retention_days);
        let app = http::router(AppState::new(store, config.clone()));
        let result = http::serve(listener, app, http::ctrl_c()).await;
        for timer in timers {
            timer.abort();
        }
        log::info!("shut down");
        result.map_err(CliError::from)
    })
}

/// A new write key: `tk_` plus 192 random bits in URL-safe base64.
pub fn new_write_key() -> Result<String, CliError> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|error| CliError::Usage(format!("no random source: {error}")))?;
    Ok(format!("tk_{}", base64url(&bytes)))
}

fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |acc, (index, byte)| acc | (u32::from(*byte) << (16 - 8 * index)));
        let chars = chunk.len() + 1;
        for index in 0..chars {
            out.push(char::from(ALPHABET[((value >> (18 - 6 * index)) & 63) as usize]));
        }
    }
    out
}

/// Hashes a password with argon2id and a random salt, in PHC string form.
pub fn hash_password(password: &str) -> Result<String, CliError> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|error| CliError::Usage(format!("no random source: {error}")))?;
    let salt = SaltString::encode_b64(&salt).map_err(|error| CliError::Usage(format!("salt: {error}")))?;
    let hash = Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|error| CliError::Usage(format!("hashing failed: {error}")))?;
    Ok(hash.to_string())
}

/// Checks a password against a stored PHC hash.
pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}

/// Entry point for the binary. Returns the process exit code.
pub fn main(args: Vec<String>) -> std::process::ExitCode {
    crate::logging::init();
    let result = parse(&args).and_then(|command| {
        let config = Config::from_env()?;
        let stdout = std::io::stdout();
        let stdin = std::io::stdin();
        execute(command, &config, &mut stdout.lock(), &mut stdin.lock())
    });
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tracer-server: {error}");
            std::process::ExitCode::from(if matches!(error, CliError::Usage(_) | CliError::Config(_)) { 2 } else { 1 })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn commands_parse() {
        assert_eq!(parse(&args(&["serve"])).unwrap(), Command::Serve);
        assert_eq!(
            parse(&args(&["app", "create", "demo", "Demo App"])).unwrap(),
            Command::AppCreate { slug: "demo".into(), name: "Demo App".into() }
        );
        assert_eq!(parse(&args(&["rollup", "--days", "7"])).unwrap(), Command::Rollup { days: 7 });
        assert_eq!(parse(&args(&["rollup"])).unwrap(), Command::Rollup { days: 3 });
        assert!(parse(&args(&["rollup", "--days", "0"])).is_err());
        assert!(parse(&args(&["app", "create", "only-slug"])).is_err());
        assert!(parse(&args(&["frobnicate"])).is_err());
    }

    #[test]
    fn base64url_matches_known_vectors() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn write_keys_are_long_url_safe_and_distinct() {
        let a = new_write_key().unwrap();
        let b = new_write_key().unwrap();
        assert_ne!(a, b);
        assert!(a.len() >= 3 + 32);
        assert!(a.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'));
    }

    #[test]
    fn passwords_hash_and_verify() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong", &hash));
        assert!(!verify_password("x", "not a hash"));
    }
}
