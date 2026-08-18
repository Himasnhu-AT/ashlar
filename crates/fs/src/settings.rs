//! Keyed JSON settings with a schema version and a debounced, durable save.
//!
//! The file is `{"schema_version": N, "values": {"key": value, …}}`. Keys are flat strings (use
//! dotted names such as `"editor.font_size"` for grouping). Changes are saved by [`Settings::tick`]
//! once no change has happened for the debounce period, or at once by [`Settings::flush`] (call it
//! on exit). The clock is the caller's (a monotonic `Duration` since any fixed start), so the
//! type works the same on the web, in tests and on desktop.
//!
//! Loading never fails: a missing file gives empty settings, an unreadable one is moved aside to
//! `<name>.corrupt` and replaced by empty settings, and a file from a newer schema is read but
//! never overwritten (so going back to an older build doesn't destroy a newer one's settings).

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::backend::ConfigStore;

/// Upgrades the values from schema `n` to `n + 1`.
pub type SettingsMigration = fn(&mut Map<String, Value>);

/// Largest settings file read (bigger is treated as corrupt).
pub const MAX_SETTINGS_BYTES: usize = 16 << 20;

/// Quiet period before a change is saved, unless set with [`Settings::with_debounce`].
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(500);

/// How the settings were loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// No file yet.
    Fresh,
    /// Read (and migrated if `from` is older than the current schema).
    Loaded { from: u64 },
    /// The file could not be read; it was moved aside to `<name>.corrupt`.
    Corrupt,
    /// The file is from a newer schema: values are readable, saves are skipped.
    TooNew { found: u64 },
}

/// Keyed settings persisted in a [`ConfigStore`].
pub struct Settings {
    store: Arc<dyn ConfigStore>,
    name: String,
    schema_version: u64,
    values: Map<String, Value>,
    state: LoadState,
    debounce: Duration,
    /// Bumped by every change; `saved_gen` is the one on disk.
    generation: u64,
    saved_gen: u64,
    /// The generation seen by the last tick and when it was first seen.
    seen: Option<(u64, Duration)>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings").field("name", &self.name).field("state", &self.state).field("values", &self.values).finish()
    }
}

impl Settings {
    /// Load `name` from `store`. `migrations[i]` upgrades schema `i + 1` to `i + 2`; the current
    /// schema is `migrations.len() + 1`.
    pub fn load(store: Arc<dyn ConfigStore>, name: &str, migrations: &[SettingsMigration]) -> Settings {
        let schema_version = migrations.len() as u64 + 1;
        let mut s = Settings {
            store,
            name: name.to_string(),
            schema_version,
            values: Map::new(),
            state: LoadState::Fresh,
            debounce: DEFAULT_DEBOUNCE,
            generation: 0,
            saved_gen: 0,
            seen: None,
        };
        let Some(text) = s.store.read(name) else { return s };
        match parse(&text) {
            Some((found, values)) if found > schema_version => {
                log::warn!("settings `{name}` are from a newer schema (v{found} > v{schema_version}); they will not be saved");
                s.values = values;
                s.state = LoadState::TooNew { found };
            }
            Some((found, mut values)) => {
                for m in migrations.iter().skip(found.saturating_sub(1) as usize) {
                    m(&mut values);
                }
                s.values = values;
                s.state = LoadState::Loaded { from: found };
                if found < schema_version {
                    s.generation = 1; // save the upgraded file
                }
            }
            None => {
                log::warn!("settings `{name}` are unreadable; moved to `{name}.corrupt`");
                let _ = s.store.write(&format!("{name}.corrupt"), &text);
                s.state = LoadState::Corrupt;
            }
        }
        s
    }

    /// Change the debounce period.
    pub fn with_debounce(mut self, debounce: Duration) -> Settings {
        self.debounce = debounce;
        self
    }

    pub fn state(&self) -> LoadState {
        self.state
    }

    pub fn schema_version(&self) -> u64 {
        self.schema_version
    }

    // ------------------------------------------------------------------ values

    /// The raw value of `key`.
    pub fn get_value(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    /// `key` as a `T`; `None` when missing or of another shape.
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        T::deserialize(self.values.get(key)?).ok()
    }

    /// `key` as a `T`, or `default` when missing or of another shape.
    pub fn get_or<T: DeserializeOwned>(&self, key: &str, default: T) -> T {
        self.get(key).unwrap_or(default)
    }

    /// Set the raw value of `key` (a no-op when unchanged).
    pub fn set_value(&mut self, key: &str, value: Value) {
        if self.values.get(key) != Some(&value) {
            self.values.insert(key.to_string(), value);
            self.changed();
        }
    }

    /// Set `key` to `value`. Fails only when `value` can't be represented as JSON (a map with
    /// non-string keys, a non-finite float…).
    pub fn set<T: Serialize + ?Sized>(&mut self, key: &str, value: &T) -> Result<(), serde_json::Error> {
        let v = serde_json::to_value(value)?;
        self.set_value(key, v);
        Ok(())
    }

    /// Remove `key`; returns its old value.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let old = self.values.remove(key);
        if old.is_some() {
            self.changed();
        }
        old
    }

    /// Every key, sorted.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }

    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    // ------------------------------------------------------------------ saving

    /// Unsaved changes exist.
    pub fn is_dirty(&self) -> bool {
        self.generation != self.saved_gen
    }

    /// Save if there are changes and none happened for the debounce period. Call it every frame or
    /// on a timer with a monotonic `now`. Returns `Ok(true)` when it wrote.
    pub fn tick(&mut self, now: Duration) -> std::io::Result<bool> {
        if !self.is_dirty() {
            self.seen = None;
            return Ok(false);
        }
        match self.seen {
            Some((g, since)) if g == self.generation => {
                if now.saturating_sub(since) >= self.debounce {
                    self.flush()?;
                    return Ok(true);
                }
            }
            _ => self.seen = Some((self.generation, now)),
        }
        Ok(false)
    }

    /// Save now if there are changes (on exit). A file from a newer schema is never overwritten.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if !self.is_dirty() {
            return Ok(());
        }
        if matches!(self.state, LoadState::TooNew { .. }) {
            self.saved_gen = self.generation;
            return Ok(());
        }
        let doc = serde_json::json!({ "schema_version": self.schema_version, "values": self.values });
        let text = serde_json::to_string_pretty(&doc).map_err(std::io::Error::other)?;
        self.store.write(&self.name, &text)?;
        self.saved_gen = self.generation;
        self.seen = None;
        Ok(())
    }
}

fn parse(text: &str) -> Option<(u64, Map<String, Value>)> {
    if text.len() > MAX_SETTINGS_BYTES {
        return None;
    }
    let Value::Object(mut o) = serde_json::from_str::<Value>(text).ok()? else { return None };
    let version = o.get("schema_version")?.as_u64()?;
    if version == 0 {
        return None;
    }
    let Value::Object(values) = o.remove("values")? else { return None };
    Some((version, values))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemoryConfig;

    fn mem() -> Arc<dyn ConfigStore> {
        Arc::new(MemoryConfig::new())
    }

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn typed_get_set_remove_keys() {
        let mut s = Settings::load(mem(), "settings.json", &[]);
        assert_eq!(s.state(), LoadState::Fresh);
        s.set("editor.font_size", &14u32).unwrap();
        s.set("theme", "dark").unwrap();
        s.set("list", &vec![1, 2]).unwrap();
        assert_eq!(s.get::<u32>("editor.font_size"), Some(14));
        assert_eq!(s.get::<String>("editor.font_size"), None, "wrong type is None");
        assert_eq!(s.get_or("missing", 7u8), 7);
        assert_eq!(s.keys().collect::<Vec<_>>(), vec!["editor.font_size", "list", "theme"]);
        assert_eq!(s.remove("theme"), Some(Value::from("dark")));
        assert_eq!(s.remove("theme"), None);
        assert!(s.set("bad", &f64::NAN).is_ok(), "NaN serializes to null");
        let mut m = std::collections::HashMap::new();
        m.insert((1, 2), 3);
        assert!(s.set("bad", &m).is_err());
    }

    #[test]
    fn debounced_save_waits_for_quiet() {
        let store = mem();
        let mut s = Settings::load(store.clone(), "s.json", &[]).with_debounce(100 * MS);
        assert!(!s.tick(Duration::ZERO).unwrap());
        s.set("a", &1).unwrap();
        assert!(!s.tick(Duration::ZERO).unwrap());
        assert!(!s.tick(50 * MS).unwrap());
        s.set("a", &2).unwrap(); // restarts the quiet period
        assert!(!s.tick(60 * MS).unwrap());
        assert!(!s.tick(150 * MS).unwrap());
        assert!(store.read("s.json").is_none());
        assert!(s.tick(160 * MS).unwrap());
        assert!(!s.is_dirty());
        let back = Settings::load(store, "s.json", &[]);
        assert_eq!(back.get::<i32>("a"), Some(2));
        assert_eq!(back.state(), LoadState::Loaded { from: 1 });
    }

    #[test]
    fn unchanged_set_is_not_a_change() {
        let mut s = Settings::load(mem(), "s.json", &[]);
        s.set("a", &1).unwrap();
        s.flush().unwrap();
        s.set("a", &1).unwrap();
        assert!(!s.is_dirty());
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        for bad in [
            "{",
            "",
            "[]",
            "null",
            r#"{"values":{}}"#,
            r#"{"schema_version":0,"values":{}}"#,
            r#"{"schema_version":"1","values":{}}"#,
            r#"{"schema_version":1,"values":[]}"#,
        ] {
            let store = mem();
            store.write("s.json", bad).unwrap();
            let s = Settings::load(store.clone(), "s.json", &[]);
            assert_eq!(s.state(), LoadState::Corrupt, "{bad}");
            assert_eq!(store.read("s.json.corrupt").as_deref(), Some(bad));
            assert_eq!(s.keys().count(), 0);
        }
    }

    #[test]
    fn newer_schema_is_read_but_never_overwritten() {
        let store = mem();
        let newer = r#"{"schema_version":18446744073709551615,"values":{"a":1}}"#;
        store.write("s.json", newer).unwrap();
        let mut s = Settings::load(store.clone(), "s.json", &[]);
        assert_eq!(s.state(), LoadState::TooNew { found: u64::MAX });
        assert_eq!(s.get::<i32>("a"), Some(1));
        s.set("a", &2).unwrap();
        s.flush().unwrap();
        assert_eq!(store.read("s.json").as_deref(), Some(newer));
        assert!(!s.is_dirty());
    }

    #[test]
    fn migrations_run_from_the_file_version() {
        fn v1_to_v2(m: &mut Map<String, Value>) {
            if let Some(v) = m.remove("fontSize") {
                m.insert("font_size".into(), v);
            }
        }
        fn v2_to_v3(m: &mut Map<String, Value>) {
            m.entry("theme").or_insert(Value::from("light"));
        }
        let store = mem();
        store.write("s.json", r#"{"schema_version":1,"values":{"fontSize":12}}"#).unwrap();
        let mut s = Settings::load(store.clone(), "s.json", &[v1_to_v2, v2_to_v3]);
        assert_eq!(s.state(), LoadState::Loaded { from: 1 });
        assert_eq!(s.get::<i32>("font_size"), Some(12));
        assert_eq!(s.get::<String>("theme").as_deref(), Some("light"));
        assert!(s.is_dirty(), "upgraded file is saved");
        s.flush().unwrap();
        let v: Value = serde_json::from_str(&store.read("s.json").unwrap()).unwrap();
        assert_eq!(v["schema_version"], 3);

        store.write("s.json", r#"{"schema_version":2,"values":{"fontSize":9}}"#).unwrap();
        let s = Settings::load(store, "s.json", &[v1_to_v2, v2_to_v3]);
        assert_eq!(s.get::<i32>("fontSize"), Some(9), "v1→v2 skipped for a v2 file");
    }
}
