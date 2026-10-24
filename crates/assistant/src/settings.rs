//! The user's choice of provider and model, as an app saves it, and turning it into a
//! [`Provider`](crate::Provider). Keys are not part of it: they are in a [`KeyStore`].

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::account::TokenSource;
use crate::types::Effort;

/// Which provider answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// The app's account backend (the user's plan).
    #[default]
    Account,
    /// Anthropic with the user's key.
    Anthropic,
    /// An OpenAI-compatible server (OpenAI, OpenRouter, a local server), key optional.
    #[serde(rename = "openai")]
    OpenAi,
}

impl ProviderKind {
    pub const ALL: [Self; 3] = [Self::Account, Self::Anthropic, Self::OpenAi];

    pub fn id(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == s)
    }
}

/// The Anthropic models offered for a key of the user's own.
pub const ANTHROPIC_MODELS: &[(&str, &str)] =
    &[("claude-opus-5-5", "Claude Opus 5.5"), ("claude-sonnet-5-5", "Claude Sonnet 5.5"), ("claude-haiku-5-5", "Claude Haiku 5.5")];
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

fn default_model() -> String {
    DEFAULT_MODEL.into()
}
fn default_openai_url() -> String {
    crate::openai::DEFAULT_BASE_URL.into()
}
fn default_effort() -> Effort {
    Effort::Medium
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub provider: ProviderKind,
    /// The account backend's model (`None`: the backend's default).
    #[serde(default)]
    pub account_model: Option<String>,
    #[serde(default = "default_model")]
    pub anthropic_model: String,
    #[serde(default = "default_openai_url")]
    pub openai_base_url: String,
    #[serde(default)]
    pub openai_model: String,
    #[serde(default = "default_effort")]
    pub effort: Effort,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: ProviderKind::Account,
            account_model: None,
            anthropic_model: default_model(),
            openai_base_url: default_openai_url(),
            openai_model: String::new(),
            effort: default_effort(),
        }
    }
}

impl Settings {
    /// The model the chosen provider uses (`account_default`: the backend's default model).
    pub fn model(&self, account_default: Option<&str>) -> String {
        match self.provider {
            ProviderKind::Account => self.account_model.clone().or(account_default.map(str::to_string)).unwrap_or_else(default_model),
            ProviderKind::Anthropic => self.anthropic_model.clone(),
            ProviderKind::OpenAi => self.openai_model.clone(),
        }
    }

    /// Whether this provider speaks Anthropic's format (effort and thinking apply).
    pub fn is_claude(&self) -> bool {
        self.provider != ProviderKind::OpenAi
    }
}

/// How to reach the app's account backend.
#[derive(Clone)]
pub struct AccountConfig {
    pub base_url: String,
    /// How errors name it ("the account server").
    pub name: String,
    pub tokens: Arc<dyn TokenSource>,
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use build::{build_provider, check_connection};

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
mod build {
    use super::*;
    use crate::account::AccountBackend;
    use crate::anthropic::{AnthropicApi, DEFAULT_BASE_URL, VERSION};
    use crate::error::{Error, from_http};
    use crate::http::{Http, check_url};
    use crate::keys::{ANTHROPIC, KeyStore, OPENAI};
    use crate::openai::OpenAiCompatible;
    use crate::provider::Provider;

    fn key(keys: &dyn KeyStore, name: &str) -> Result<Option<crate::keys::ApiKey>, Error> {
        keys.get(name).map_err(|e| Error::Other(format!("the API key could not be read from {}: {e}", keys.describe())))
    }

    /// The provider `settings` choose. `account` is `None` when the app has no account backend.
    pub fn build_provider(settings: &Settings, keys: &dyn KeyStore, account: Option<&AccountConfig>) -> Result<Box<dyn Provider>, Error> {
        match settings.provider {
            ProviderKind::Account => {
                let a = account.ok_or_else(|| Error::Unavailable("This build has no account server. Use your own API key instead.".into()))?;
                Ok(Box::new(AccountBackend::new(&a.base_url, &a.name, a.tokens.clone())))
            }
            ProviderKind::Anthropic => {
                let k = key(keys, ANTHROPIC)?.ok_or(Error::MissingKey)?;
                Ok(Box::new(AnthropicApi::new(k)))
            }
            ProviderKind::OpenAi => {
                if settings.openai_base_url.trim().is_empty() {
                    return Err(Error::Config("Enter the server's base URL in the assistant's settings.".into()));
                }
                Ok(Box::new(OpenAiCompatible::new(&settings.openai_base_url, key(keys, OPENAI)?)))
            }
        }
    }

    /// Check the chosen provider without spending tokens: the account's AI status, or the
    /// provider's model list (with the key). Returns a line for the settings page; for an
    /// OpenAI-compatible server also the model ids it lists.
    pub fn check_connection(settings: &Settings, keys: &dyn KeyStore, account: Option<&AccountConfig>) -> Result<(String, Vec<String>), Error> {
        let http = Http::new();
        match settings.provider {
            ProviderKind::Account => {
                let a = account.ok_or_else(|| Error::Unavailable("This build has no account server.".into()))?;
                let s = AccountBackend::new(&a.base_url, &a.name, a.tokens.clone()).status()?;
                let models = s.models.iter().map(|m| m.id.clone()).collect();
                match s.reason_text() {
                    None => Ok((format!("Connected. AI is included{}.", s.plan.map(|p| format!(" in your {p} plan")).unwrap_or_default()), models)),
                    Some(why) => Err(Error::PlanRequired(why)),
                }
            }
            ProviderKind::Anthropic => {
                let k = key(keys, ANTHROPIC)?.ok_or(Error::MissingKey)?;
                let url = format!("{DEFAULT_BASE_URL}/v1/models");
                check_url(&url, true)?;
                let mut r = http.get(&url, &[("x-api-key", k.expose()), ("anthropic-version", VERSION)], "Anthropic")?;
                if r.status != 200 {
                    let (s, a) = (r.status, r.retry_after);
                    return Err(from_http(s, a, &r.error_body(), "Anthropic"));
                }
                let v = r.json()?;
                let ids: Vec<String> = v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)).collect();
                Ok((format!("Connected. The key works ({} models).", ids.len()), ids))
            }
            ProviderKind::OpenAi => {
                let k = key(keys, OPENAI)?;
                let base = crate::openai::endpoint(&settings.openai_base_url);
                let url = format!("{}/models", base.trim_end_matches("/chat/completions"));
                check_url(&url, k.is_some())?;
                let auth = k.as_ref().map(|k| format!("Bearer {}", k.expose()));
                let headers: Vec<(&str, &str)> = auth.as_deref().map(|a| vec![("authorization", a)]).unwrap_or_default();
                let name = crate::http::host(&url).unwrap_or_else(|| "the server".into());
                let mut r = http.get(&url, &headers, &name)?;
                if r.status != 200 {
                    let (s, a) = (r.status, r.retry_after);
                    return Err(from_http(s, a, &r.error_body(), &name));
                }
                let v = r.json()?;
                let mut ids: Vec<String> = v["data"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)).collect();
                ids.sort();
                Ok((format!("Connected to {name} ({} models).", ids.len()), ids))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_defaults() {
        let s: Settings = serde_json::from_str(r#"{"provider":"openai","openai_model":"llama3"}"#).unwrap_or_default();
        assert_eq!(s.provider, ProviderKind::OpenAi);
        assert_eq!(s.model(None), "llama3");
        assert_eq!(s.anthropic_model, DEFAULT_MODEL);
        assert!(!s.is_claude());
        let d = Settings::default();
        assert_eq!(d.model(Some("claude-haiku-5-5")), "claude-haiku-5-5");
        assert_eq!(ProviderKind::parse("anthropic"), Some(ProviderKind::Anthropic));
    }
}
