//! What went wrong, in words a person can act on.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The account backend wants a signed-in user.
    #[error("Sign in to use the assistant with your account.")]
    SignInRequired,
    /// The account's plan does not include AI (the backend's message).
    #[error("{0}")]
    PlanRequired(String),
    /// The backend's shared AI budget is used up.
    #[error("{0}")]
    BudgetExhausted(String),
    #[error("{}", rate_limited_text(*retry_after_secs, message))]
    RateLimited { retry_after_secs: Option<u64>, message: String },
    /// The API key was refused (or none is saved).
    #[error("{0}")]
    InvalidKey(String),
    #[error("No API key is saved for this provider. Add one in the assistant's settings.")]
    MissingKey,
    /// The server could not be reached.
    #[error("Can't reach {0}. Check your internet connection and try again.")]
    Offline(String),
    /// The model provider failed or is overloaded.
    #[error("{0}")]
    Upstream(String),
    /// The request was refused as malformed (the provider's message).
    #[error("The request was refused: {0}")]
    BadRequest(String),
    #[error("The conversation is too long for this model. Start a new chat.")]
    TooLarge,
    /// The service is switched off for this account.
    #[error("{0}")]
    Unavailable(String),
    /// A setting is missing or wrong (a base URL, a model name).
    #[error("{0}")]
    Config(String),
    #[error("Stopped.")]
    Cancelled,
    /// The reply did not follow the provider's format.
    #[error("The reply could not be read: {0}")]
    Protocol(String),
    #[error("{0}")]
    Other(String),
}

fn rate_limited_text(after: Option<u64>, message: &str) -> String {
    let base = if message.is_empty() { "Too many requests right now." } else { message };
    match after {
        Some(s) if s > 0 => format!("{base} Try again in {}.", seconds_text(s)),
        _ => format!("{base} Try again in a moment."),
    }
}

fn seconds_text(s: u64) -> String {
    match s {
        0..90 => format!("{s} s"),
        90..5400 => format!("{} min", s.div_ceil(60)),
        _ => format!("{} h", s.div_ceil(3600)),
    }
}

impl Error {
    /// A short machine-readable code (for logs, telemetry and commands' replies).
    pub fn code(&self) -> &'static str {
        match self {
            Error::SignInRequired => "sign_in_required",
            Error::PlanRequired(_) => "plan_required",
            Error::BudgetExhausted(_) => "budget_exhausted",
            Error::RateLimited { .. } => "rate_limited",
            Error::InvalidKey(_) => "invalid_key",
            Error::MissingKey => "missing_key",
            Error::Offline(_) => "offline",
            Error::Upstream(_) => "upstream_error",
            Error::BadRequest(_) => "invalid_request",
            Error::TooLarge => "too_large",
            Error::Unavailable(_) => "unavailable",
            Error::Config(_) => "config",
            Error::Cancelled => "cancelled",
            Error::Protocol(_) => "protocol",
            Error::Other(_) => "other",
        }
    }

    /// The same error with `detail` added to its text where the text is the server's own (a
    /// request id for support: "Request ID: air_…").
    pub fn with_detail(self, detail: &str) -> Self {
        let add = |m: String| if m.is_empty() { detail.to_string() } else { format!("{m} ({detail})") };
        match self {
            Error::Upstream(m) => Error::Upstream(add(m)),
            Error::BadRequest(m) => Error::BadRequest(add(m)),
            Error::Protocol(m) => Error::Protocol(add(m)),
            Error::Other(m) => Error::Other(add(m)),
            Error::Unavailable(m) => Error::Unavailable(add(m)),
            Error::BudgetExhausted(m) => Error::BudgetExhausted(add(m)),
            Error::TooLarge => Error::Other(format!("{} ({detail})", Error::TooLarge)),
            other => other,
        }
    }

    /// Whether sending the same request again later may work.
    pub fn retryable(&self) -> bool {
        matches!(self, Error::RateLimited { .. } | Error::Offline(_) | Error::Upstream(_))
    }
}

/// The error for an HTTP failure from an Anthropic-format API (`{"type":"error","error":{…}}`),
/// an OpenAI-format API (`{"error":{"message",…}}`) or an account backend
/// (`{"error":"<code>","message":…}`).
pub fn from_http(status: u16, retry_after_secs: Option<u64>, body: &[u8], service: &str) -> Error {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let (code, message) = match &v["error"] {
        // account backend
        Value::String(code) => (code.clone(), v["message"].as_str().unwrap_or_default().to_string()),
        // Anthropic / OpenAI
        Value::Object(e) => (
            e.get("type").or_else(|| e.get("code")).and_then(Value::as_str).unwrap_or_default().to_string(),
            e.get("message").and_then(Value::as_str).unwrap_or_default().to_string(),
        ),
        _ => (String::new(), String::from_utf8_lossy(body.get(..body.len().min(300)).unwrap_or_default()).trim().to_string()),
    };
    let or = |m: &str, d: &str| if m.is_empty() { d.to_string() } else { m.to_string() };
    match (status, code.as_str()) {
        (_, "pro_required" | "plan_required") => Error::PlanRequired(or(&message, "AI is not part of this account's plan.")),
        (_, "budget_exhausted") => Error::BudgetExhausted(or(&message, "The AI budget for this period is used up.")),
        (_, "credits_exhausted") => Error::BudgetExhausted(or(&message, "This account's AI for this period is used up.")),
        (_, "sign_in_required") | (401, "unauthorized") => Error::SignInRequired,
        (_, "disabled") => Error::Unavailable(or(&message, "AI is switched off for now.")),
        (_, "rate_limited" | "rate_limit_error" | "rate_limit_exceeded") | (429, _) => Error::RateLimited { retry_after_secs, message },
        (_, "authentication_error" | "invalid_api_key") | (401, _) => Error::InvalidKey(format!(
            "{service} refused the API key{}. Check it in the assistant's settings.",
            if message.is_empty() { String::new() } else { format!(" ({message})") }
        )),
        (403, "permission_error") | (403, "") => Error::InvalidKey(or(&message, &format!("{service} refused the request with this API key."))),
        (_, "too_large" | "request_too_large") | (413, _) => Error::TooLarge,
        (_, "overloaded_error") | (529, _) => Error::Upstream(format!("{service} is overloaded right now. Try again in a moment.")),
        (_, "upstream_error" | "api_error") => Error::Upstream(or(&message, &format!("{service} failed. Try again in a moment."))),
        (s, _) if s >= 500 => Error::Upstream(format!("{service} failed (HTTP {s}). Try again in a moment.")),
        (404, _) => Error::BadRequest(or(&message, "not found (check the model name and the base URL)")),
        (s, _) if s >= 400 => Error::BadRequest(or(&message, &format!("HTTP {s}"))),
        (s, _) => Error::Protocol(format!("unexpected HTTP {s}")),
    }
}

/// The error for an `error` frame inside an Anthropic stream.
pub fn from_stream_error(v: &Value, service: &str) -> Error {
    let body = serde_json::to_vec(v).unwrap_or_default();
    let status = match v["error"]["type"].as_str() {
        Some("overloaded_error") => 529,
        Some("rate_limit_error") => 429,
        Some("invalid_request_error") => 400,
        Some("authentication_error") => 401,
        _ => 500,
    };
    from_http(status, None, &body, service)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_codes_become_their_own_errors() {
        let e = from_http(403, None, br#"{"error":"pro_required","message":"AI is part of Pro."}"#, "the server");
        assert_eq!(e, Error::PlanRequired("AI is part of Pro.".into()));
        assert_eq!(from_http(402, None, br#"{"error":"budget_exhausted","message":"used up"}"#, "x").code(), "budget_exhausted");
        let e = from_http(429, Some(30), br#"{"error":"rate_limited","message":"Slow down."}"#, "x");
        assert_eq!(e.to_string(), "Slow down. Try again in 30 s.");
        assert_eq!(from_http(401, None, br#"{"error":"unauthorized","message":"no"}"#, "x"), Error::SignInRequired);
        // the daily limit: Retry-After is the time to midnight
        let e = from_http(429, Some(5 * 3600 + 20), br#"{"error":"rate_limited","message":"Today's AI requests are used up."}"#, "x");
        assert_eq!(e.to_string(), "Today's AI requests are used up. Try again in 6 h.");
        let e = from_http(503, None, br#"{"error":"disabled","message":"AI is switched off on this server."}"#, "x");
        assert_eq!(e, Error::Unavailable("AI is switched off on this server.".into()));
        let e = from_http(502, None, br#"{"error":"upstream_error","message":"The AI provider failed."}"#, "x").with_detail("Request ID: air_1");
        assert_eq!(e.to_string(), "The AI provider failed. (Request ID: air_1)");
        let e = from_http(400, None, br#"{"error":"invalid_request","message":"messages.1: bad block"}"#, "x");
        assert_eq!(e.to_string(), "The request was refused: messages.1: bad block");
    }

    #[test]
    fn provider_errors() {
        let e = from_http(401, None, br#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#, "Anthropic");
        assert_eq!(e.code(), "invalid_key");
        assert!(e.to_string().contains("Anthropic refused the API key"));
        let e =
            from_http(401, None, br#"{"error":{"message":"Incorrect API key","type":"invalid_request_error","code":"invalid_api_key"}}"#, "OpenAI");
        assert_eq!(e.code(), "invalid_key");
        assert_eq!(from_http(529, None, b"", "Anthropic").code(), "upstream_error");
        assert_eq!(from_http(502, None, b"<html>bad gateway</html>", "x").code(), "upstream_error");
        assert_eq!(
            from_http(400, None, br#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#, "x"),
            Error::BadRequest("bad".into())
        );
    }
}
