//! Server settings, read from the environment.

use std::net::SocketAddr;
use std::path::PathBuf;

/// Settings for every subcommand. All come from `TRACER_*` environment variables and all are optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub retention_days: u32,
    /// Ingest requests per minute per key and per client address. `0` switches the limit off.
    pub rate_per_min: u32,
    pub trust_proxy: bool,
    pub admin_password: Option<String>,
    pub secure_cookies: bool,
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
            trust_proxy: false,
            admin_password: None,
            secure_cookies: false,
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
        config.trust_proxy = get("TRACER_TRUST_PROXY").as_deref() == Some("1");
        config.admin_password = get("TRACER_ADMIN_PASSWORD");
        config.secure_cookies = get("TRACER_SECURE_COOKIES").as_deref() == Some("1");
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
        assert!(!config.secure_cookies);
    }

    #[test]
    fn bad_values_are_reported_by_name() {
        assert_eq!(from(&[("TRACER_RATE_PER_MIN", "many")]).unwrap_err().name, "TRACER_RATE_PER_MIN");
    }
}
