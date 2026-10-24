//! API keys: kept in the OS keychain ([`KeychainKeys`], feature `keychain`) or, for tests, in
//! memory ([`MemoryKeys`]). Never in a plain file, never logged: an [`ApiKey`] prints as `****`.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

/// The key's name for Anthropic.
pub const ANTHROPIC: &str = "assistant-anthropic-api-key";
/// The key's name for the OpenAI-compatible provider.
pub const OPENAI: &str = "assistant-openai-api-key";

/// A secret. `Debug` and `Display` never show it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// A key as typed: surrounding whitespace removed; `None` when nothing is left.
    pub fn new(key: impl Into<String>) -> Option<Self> {
        let k = key.into().trim().to_string();
        (!k.is_empty()).then_some(Self(k))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The last four characters, to tell keys apart in a settings page.
    pub fn hint(&self) -> String {
        let n = self.0.chars().count();
        let tail: String = self.0.chars().skip(n.saturating_sub(4)).collect();
        format!("…{tail}")
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(****)")
    }
}

impl std::fmt::Display for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("****")
    }
}

/// Where keys are kept. Errors are readable text without the key in it.
pub trait KeyStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<ApiKey>, String>;
    fn set(&self, name: &str, key: &ApiKey) -> Result<(), String>;
    /// `false` when there was none.
    fn delete(&self, name: &str) -> Result<bool, String>;
    /// Where the keys go, for the settings page ("the macOS Keychain").
    fn describe(&self) -> String;
}

/// Keys in memory (tests, and builds without a keychain).
#[derive(Default)]
pub struct MemoryKeys(Mutex<HashMap<String, ApiKey>>);

impl MemoryKeys {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for MemoryKeys {
    fn get(&self, name: &str) -> Result<Option<ApiKey>, String> {
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).get(name).cloned())
    }

    fn set(&self, name: &str, key: &ApiKey) -> Result<(), String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(name.to_string(), key.clone());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<bool, String> {
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(name).is_some())
    }

    fn describe(&self) -> String {
        "memory (not saved)".into()
    }
}

#[cfg(feature = "keychain")]
pub use keychain::KeychainKeys;

#[cfg(feature = "keychain")]
mod keychain {
    use super::*;

    /// Keys in the OS keychain under one service (the app's bundle identifier).
    pub struct KeychainKeys(ashlar_native::secrets::Keychain);

    impl KeychainKeys {
        pub fn new(service: &str) -> Result<Self, String> {
            ashlar_native::secrets::Keychain::new(service).map(Self).map_err(|e| e.to_string())
        }
    }

    impl KeyStore for KeychainKeys {
        fn get(&self, name: &str) -> Result<Option<ApiKey>, String> {
            self.0.get(name).map(|k| k.and_then(ApiKey::new)).map_err(|e| e.to_string())
        }

        fn set(&self, name: &str, key: &ApiKey) -> Result<(), String> {
            self.0.set(name, key.expose()).map_err(|e| e.to_string())
        }

        fn delete(&self, name: &str) -> Result<bool, String> {
            self.0.delete(name).map_err(|e| e.to_string())
        }

        fn describe(&self) -> String {
            if cfg!(target_os = "macos") { "the macOS Keychain".into() } else { "the system keychain".into() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_never_print() {
        let k = ApiKey::new("  sk-secret-1234 ").unwrap_or_else(|| ApiKey("x".into()));
        assert_eq!(k.expose(), "sk-secret-1234");
        assert_eq!(format!("{k:?} {k}"), "ApiKey(****) ****");
        assert_eq!(k.hint(), "…1234");
        assert!(ApiKey::new("   ").is_none());
        let s = MemoryKeys::new();
        assert_eq!(s.get(ANTHROPIC), Ok(None));
        assert_eq!(s.set(ANTHROPIC, &k), Ok(()));
        assert_eq!(s.get(ANTHROPIC), Ok(Some(k)));
        assert_eq!(s.delete(ANTHROPIC), Ok(true));
        assert_eq!(s.delete(ANTHROPIC), Ok(false));
    }
}
