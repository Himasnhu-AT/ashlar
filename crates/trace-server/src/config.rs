//! Server settings, read from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;

/// Settings for every subcommand. All come from `TRACER_*` environment variables and all are optional.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub retention_days: u32,
    /// Ingest requests per minute per key and per client address. `0` switches the limit off.
    pub rate_per_min: u32,
    /// Ingest requests per minute for one write key, over all addresses: a capacity guard, far above the
    /// per-address limit. `0` switches it off.
    pub key_rate_per_min: u32,
    pub trust_proxy: bool,
    /// Only filled by `admin create`, which reads `TRACER_ADMIN_PASSWORD` itself; never by [`Config::from_env`].
    pub admin_password: Option<String>,
    /// Mark cookies `Secure` and use the `__Host-` prefix. On unless `TRACER_SECURE_COOKIES=0`.
    pub secure_cookies: bool,
    /// Ingest answers 507 once the database is larger than this many MiB. `0` switches the ceiling off.
    pub max_db_mb: u64,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("data_dir", &self.data_dir)
            .field("retention_days", &self.retention_days)
            .field("rate_per_min", &self.rate_per_min)
            .field("key_rate_per_min", &self.key_rate_per_min)
            .field("trust_proxy", &self.trust_proxy)
            .field("admin_password", &self.admin_password.as_ref().map(|_| "<redacted>"))
            .field("secure_cookies", &self.secure_cookies)
            .field("max_db_mb", &self.max_db_mb)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{name}: {problem}")]
pub struct ConfigError {
    pub name: &'static str,
    pub problem: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8090)),
            data_dir: PathBuf::from("./data"),
            retention_days: 90,
            rate_per_min: 120,
            key_rate_per_min: 6000,
            trust_proxy: false,
            admin_password: None,
            secure_cookies: true,
            max_db_mb: 10_240,
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Reads settings through `lookup`, so tests need not touch the process environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        let get = |name: &str| lookup(name).filter(|value| !value.is_empty());
        if let Some(value) = get("TRACER_BIND") {
            config.bind = value.parse().map_err(|_| bad("TRACER_BIND", "expected host:port such as 127.0.0.1:8090"))?;
        }
        if let Some(value) = get("TRACER_DATA_DIR") {
            config.data_dir = PathBuf::from(value);
        }
        if let Some(value) = get("TRACER_RETENTION_DAYS") {
            config.retention_days = value.parse().map_err(|_| bad("TRACER_RETENTION_DAYS", "expected a whole number of days"))?;
        }
        if let Some(value) = get("TRACER_RATE_PER_MIN") {
            config.rate_per_min = value.parse().map_err(|_| bad("TRACER_RATE_PER_MIN", "expected a whole number"))?;
        }
        if let Some(value) = get("TRACER_KEY_RATE_PER_MIN") {
            config.key_rate_per_min = value.parse().map_err(|_| bad("TRACER_KEY_RATE_PER_MIN", "expected a whole number"))?;
        }
        if let Some(value) = get("TRACER_MAX_DB_MB") {
            config.max_db_mb = value.parse().map_err(|_| bad("TRACER_MAX_DB_MB", "expected a whole number of MiB"))?;
        }
        config.trust_proxy = get("TRACER_TRUST_PROXY").as_deref() == Some("1");
        config.secure_cookies = get("TRACER_SECURE_COOKIES").as_deref() != Some("0");
        Ok(config)
    }

    /// Path of the database file.
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("tracer.db")
    }
}

fn bad(name: &'static str, problem: &str) -> ConfigError {
    ConfigError { name, problem: problem.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        Config::from_lookup(|name| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| value.to_string()))
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        assert_eq!(from(&[]).unwrap(), Config::default());
    }

    #[test]
    fn values_are_parsed() {
        let config = from(&[("TRACER_BIND", "0.0.0.0:9000"), ("TRACER_RETENTION_DAYS", "30"), ("TRACER_TRUST_PROXY", "1")]).unwrap();
        assert_eq!(config.bind.port(), 9000);
        assert_eq!(config.retention_days, 30);
        assert!(config.trust_proxy);
        assert!(config.secure_cookies, "cookies are Secure unless switched off");
        assert!(!from(&[("TRACER_SECURE_COOKIES", "0")]).unwrap().secure_cookies);
        assert!(from(&[("TRACER_SECURE_COOKIES", "1")]).unwrap().secure_cookies);
    }

    #[test]
    fn new_limits_have_defaults_and_parse() {
        let config = from(&[]).unwrap();
        assert_eq!((config.key_rate_per_min, config.max_db_mb), (6000, 10_240));
        let config = from(&[("TRACER_KEY_RATE_PER_MIN", "0"), ("TRACER_MAX_DB_MB", "5")]).unwrap();
        assert_eq!((config.key_rate_per_min, config.max_db_mb), (0, 5));
        assert_eq!(from(&[("TRACER_MAX_DB_MB", "big")]).unwrap_err().name, "TRACER_MAX_DB_MB");
    }

    #[test]
    fn the_admin_password_is_never_read_from_the_environment_and_never_printed() {
        let config = from(&[("TRACER_ADMIN_PASSWORD", "hunter2hunter2")]).unwrap();
        assert_eq!(config.admin_password, None);
        let with = Config { admin_password: Some("hunter2hunter2".into()), ..Config::default() };
        let text = format!("{with:?}");
        assert!(!text.contains("hunter2") && text.contains("redacted"), "{text}");
    }

    #[test]
    fn bad_values_are_reported_by_name() {
        assert_eq!(from(&[("TRACER_RATE_PER_MIN", "many")]).unwrap_err().name, "TRACER_RATE_PER_MIN");
    }
}
