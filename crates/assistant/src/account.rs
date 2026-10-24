//! An account backend that holds the model provider's key for its users: the app signs the user
//! in, and the backend forwards Anthropic-format requests for accounts whose plan includes AI.
//!
//! - `GET {base}/v1/ai/status` → [`AccountStatus`]: whether AI is available to this account and
//!   why not, the plan, the models and today's limits.
//! - `POST {base}/v1/ai/messages`: an Anthropic Messages request (streamed); the reply is the
//!   provider's stream passed through. Errors before streaming are
//!   `{"error": "<code>", "message": "<text>"}`: `unauthorized` (401), `pro_required` (403),
//!   `budget_exhausted` (402), `rate_limited` (429, `Retry-After`), `invalid_request` (400),
//!   `too_large` (413), `upstream_error` (502).
//!
//! Both take `Authorization: Bearer <access token>` from the app's [`TokenSource`].

use serde::{Deserialize, Serialize};

/// Where the user's access token comes from (the app's sign-in). Never log a token.
pub trait TokenSource: Send + Sync {
    /// The current token; [`crate::Error::SignInRequired`] when signed out.
    fn token(&self) -> Result<String, crate::Error>;
    /// A fresh token after the backend refused `refused` (expired): the default gives up.
    fn refresh(&self, refused: &str) -> Result<String, crate::Error> {
        let _ = refused;
        Err(crate::Error::SignInRequired)
    }
}

impl<F: Fn() -> Result<String, crate::Error> + Send + Sync> TokenSource for F {
    fn token(&self) -> Result<String, crate::Error> {
        self()
    }
}

/// A model the backend offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountModel {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLimits {
    #[serde(default)]
    pub requests_per_day: Option<u64>,
    #[serde(default)]
    pub requests_left_today: Option<u64>,
}

/// `GET /v1/ai/status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountStatus {
    pub available: bool,
    /// `sign_in_required`, `pro_required`, `budget_exhausted`, `disabled`, or none.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub models: Vec<AccountModel>,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub limits: AccountLimits,
}

impl AccountStatus {
    /// The model to use: the one marked default, else `default_model`, else the first.
    pub fn model(&self) -> Option<&str> {
        self.models.iter().find(|m| m.default).map(|m| m.id.as_str()).or(self.default_model.as_deref()).or(self.models.first().map(|m| m.id.as_str()))
    }

    /// Why AI is not available, in words (`None` when it is).
    pub fn reason_text(&self) -> Option<String> {
        if self.available {
            return None;
        }
        Some(
            match self.reason.as_deref() {
                Some("sign_in_required") => "Sign in to use AI with your account.",
                Some("pro_required") => "AI is part of Pro. Add your own API key to use it now.",
                Some("budget_exhausted") => "The AI budget for this period is used up. Add your own API key to keep going.",
                Some("disabled") => "AI through your account is switched off for now.",
                _ => "AI isn't available for this account.",
            }
            .to_string(),
        )
    }
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use client::AccountBackend;

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
mod client {
    use std::sync::Arc;

    use super::*;
    use crate::anthropic::{BodyOptions, VERSION, read_stream, request_body};
    use crate::error::{Error, from_http};
    use crate::http::{Http, check_url};
    use crate::provider::{Cancel, Provider, Request, Response};
    use crate::types::StreamEvent;

    /// The account backend at `base_url`, as a provider.
    pub struct AccountBackend {
        base_url: String,
        name: String,
        tokens: Arc<dyn TokenSource>,
        http: Http,
    }

    impl AccountBackend {
        /// `name` is how errors refer to the server ("the account server").
        pub fn new(base_url: impl Into<String>, name: impl Into<String>, tokens: Arc<dyn TokenSource>) -> Self {
            Self { base_url: base_url.into().trim_end_matches('/').to_string(), name: name.into(), tokens, http: Http::new() }
        }

        /// `GET /v1/ai/status`.
        pub fn status(&self) -> Result<AccountStatus, Error> {
            let url = format!("{}/v1/ai/status", self.base_url);
            check_url(&url, true)?;
            let token = self.tokens.token()?;
            let mut reply = self.http.get(&url, &[("authorization", &format!("Bearer {token}")), ("accept", "application/json")], &self.name)?;
            if reply.status == 401 {
                let fresh = self.tokens.refresh(&token)?;
                reply = self.http.get(&url, &[("authorization", &format!("Bearer {fresh}")), ("accept", "application/json")], &self.name)?;
            }
            if reply.status != 200 {
                let (s, after) = (reply.status, reply.retry_after);
                return Err(from_http(s, after, &reply.error_body(), &self.name));
            }
            serde_json::from_value(reply.json()?).map_err(|e| Error::Protocol(format!("the AI status: {e}")))
        }
    }

    impl Provider for AccountBackend {
        fn id(&self) -> &str {
            "account"
        }

        fn send(&mut self, request: &Request<'_>, events: &mut dyn FnMut(StreamEvent), cancel: &Cancel) -> Result<Response, Error> {
            let url = format!("{}/v1/ai/messages", self.base_url);
            check_url(&url, true)?;
            // the backend adds refusal fallbacks itself
            let body = serde_json::to_vec(&request_body(request, BodyOptions::default())).map_err(|e| Error::Other(e.to_string()))?;
            let token = self.tokens.token()?;
            let post = |token: &str, body: Vec<u8>| {
                let auth = format!("Bearer {token}");
                self.http.post(&url, &[("authorization", &auth), ("anthropic-version", VERSION), ("accept", "text/event-stream")], body, &self.name)
            };
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let mut reply = post(&token, body.clone())?;
            if reply.status == 401 {
                // an expired access token: once more with a fresh one
                let fresh = self.tokens.refresh(&token)?;
                reply = post(&fresh, body)?;
            }
            if reply.status != 200 {
                let (s, after) = (reply.status, reply.retry_after);
                return Err(from_http(s, after, &reply.error_body(), &self.name));
            }
            read_stream(reply.body, request.model, events, cancel, &self.name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parses_the_contract() {
        let s: AccountStatus = serde_json::from_str(
            r#"{"available":false,"reason":"pro_required","plan":"free","models":[{"id":"claude-opus-5-5","label":"Opus","default":true},{"id":"claude-haiku-5-5","label":"Haiku","default":false}],"default_model":"claude-opus-5-5","limits":{"requests_per_day":100,"requests_left_today":100}}"#,
        )
        .unwrap_or_default();
        assert_eq!(s.model(), Some("claude-opus-5-5"));
        assert_eq!(s.limits.requests_left_today, Some(100));
        assert!(s.reason_text().unwrap_or_default().contains("part of Pro"));
    }
}
