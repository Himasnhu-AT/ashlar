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
    /// The account's AI credits this period, when the backend meters them.
    #[serde(default)]
    pub credits: Option<AccountCredits>,
}

/// AI credits in the current period (`credits` in the status).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountCredits {
    #[serde(default)]
    pub granted: Option<i64>,
    #[serde(default)]
    pub used: i64,
    #[serde(default)]
    pub reserved: i64,
    #[serde(default)]
    pub remaining: i64,
    /// RFC 3339.
    #[serde(default)]
    pub period_start: Option<String>,
    /// RFC 3339: when the credits renew.
    #[serde(default)]
    pub period_end: Option<String>,
}

/// Which billing page to open ([`AccountBackend::billing_link`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingPage {
    /// Start a subscription (`POST /v1/billing/checkout`).
    Checkout,
    /// Manage the subscription (`POST /v1/billing/portal`).
    Portal,
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
        // plan-neutral: an app that sells plans says so itself
        Some(
            match self.reason.as_deref() {
                Some("sign_in_required") => "Sign in to use AI with your account.",
                Some("pro_required" | "plan_required") => "Not available for your account yet. Add your own API key to use AI now.",
                Some("budget_exhausted" | "credits_exhausted") => "AI through your account is used up for now. Add your own API key to keep going.",
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

    /// The longest `Retry-After` (seconds) waited out and retried once by itself.
    pub const SHORT_WAIT_S: u64 = 15;

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

        /// `GET /v1/config` (no sign-in): whether the app may show plans and prices. `false` when
        /// the server says so, lacks the field, or can't be reached.
        pub fn pricing_enabled(&self) -> bool {
            let url = format!("{}/v1/config", self.base_url);
            if check_url(&url, false).is_err() {
                return false;
            }
            match self.http.get(&url, &[("accept", "application/json")], &self.name) {
                Ok(r) if r.status == 200 => r.json().ok().and_then(|v| v["pricing_enabled"].as_bool()).unwrap_or(false),
                _ => false,
            }
        }

        /// The web page to start (`Checkout`) or manage (`Portal`) a subscription: `POST
        /// /v1/billing/checkout|portal` with the sign-in, answering `{"url"}` (or `{"link"}`).
        /// Only an `https://` link is returned.
        pub fn billing_link(&self, page: BillingPage) -> Result<String, Error> {
            let path = match page {
                BillingPage::Checkout => "checkout",
                BillingPage::Portal => "portal",
            };
            let url = format!("{}/v1/billing/{path}", self.base_url);
            check_url(&url, true)?;
            let token = self.tokens.token()?;
            let post = |t: &str| {
                self.http.post(&url, &[("authorization", &format!("Bearer {t}")), ("accept", "application/json")], b"{}".to_vec(), &self.name)
            };
            let mut reply = post(&token)?;
            if reply.status == 401 {
                reply = post(&self.tokens.refresh(&token)?)?;
            }
            if reply.status != 200 {
                let (s, after) = (reply.status, reply.retry_after);
                let e = from_http(s, after, &reply.error_body(), &self.name);
                return Err(reply.detail(e));
            }
            let v = reply.json()?;
            let link = v["url"].as_str().or(v["link"].as_str()).unwrap_or_default().to_string();
            if !link.starts_with("https://") {
                return Err(Error::Protocol("the billing page's link is missing".into()));
            }
            Ok(link)
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
                let e = from_http(s, after, &reply.error_body(), &self.name);
                return Err(reply.detail(e));
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
            let mut token = token;
            let mut reply = post(&token, body.clone())?;
            if reply.status == 401 {
                // an expired access token: once more with a fresh one
                token = self.tokens.refresh(&token)?;
                reply = post(&token, body.clone())?;
            }
            // the shared budget is held by requests in flight: the server says to wait a few
            // seconds (a daily limit says hours); wait once and try again
            if reply.status == 429
                && let Some(wait) = reply.retry_after.filter(|s| (1..=SHORT_WAIT_S).contains(s))
            {
                let until = std::time::Instant::now() + std::time::Duration::from_secs(wait);
                while std::time::Instant::now() < until {
                    if cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                reply = post(&token, body)?;
            }
            if reply.status != 200 {
                let (s, after) = (reply.status, reply.retry_after);
                let e = from_http(s, after, &reply.error_body(), &self.name);
                return Err(reply.detail(e));
            }
            let id = reply.request_id.clone();
            read_stream(reply.body, request.model, events, cancel, &self.name).map_err(|e| match &id {
                Some(id) if !matches!(e, Error::Cancelled) => e.with_detail(&format!("Request ID: {id}")),
                _ => e,
            })
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
        assert_eq!(s.reason_text().as_deref(), Some("Not available for your account yet. Add your own API key to use AI now."));
        assert!(s.credits.is_none());
        let s: AccountStatus = serde_json::from_str(
            r#"{"available":true,"reason":null,"credits":{"granted":1900,"used":12,"reserved":0,"remaining":1888,"period_start":"2026-10-01T00:00:00Z","period_end":"2026-11-01T00:00:00Z"}}"#,
        )
        .unwrap_or_default();
        let c = s.credits.unwrap_or_default();
        assert_eq!((c.remaining, c.period_end.as_deref()), (1888, Some("2026-11-01T00:00:00Z")));
    }
}
