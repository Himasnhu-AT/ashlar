//! Download links asked for at install time, when the catalog doesn't publish them: a short-lived link
//! signed for the signed-in account.
//!
//! [`SignedLinks`] asks `GET <endpoint>/<id>/download?platform=…&arch=…` with
//! `Authorization: Bearer <access token>` and expects `{id, version, url, mirror, sha256, size}`; the
//! answer must name the same build the catalog lists (id, version, sha256 and size), so a link can't
//! swap the bundle. A 401 gets one fresh token and one more try. The app provides the tokens
//! ([`Tokens`]); nothing here logs or shows one.

use std::sync::Arc;

use serde_json::Value;

use crate::catalog::{Entry, url_ok};

/// Where to download one build from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    /// A second copy, tried after `url`.
    pub mirror: Option<String>,
}

/// Why no link could be had.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// Nobody is signed in, or the sign-in ended.
    #[error("sign in to download it")]
    SignedOut,
    #[error("couldn't get a download link: {0}")]
    Failed(String),
}

/// Gives the link for one catalog entry.
pub trait Links: Send + Sync {
    fn link(&self, entry: &Entry) -> Result<Link, LinkError>;
}

/// The signed-in account's access tokens.
pub trait Tokens: Send + Sync {
    /// The current access token.
    fn token(&self) -> Result<String, LinkError>;
    /// A new token after `refused` was answered 401.
    fn fresh(&self, refused: &str) -> Result<String, LinkError>;
}

/// `GET <url>` with a bearer token: the status and the body (the HTTPS one reads at most 64 KiB).
pub type AuthorizedGet = dyn Fn(&str, &str) -> Result<(u16, Vec<u8>), String> + Send + Sync;

/// Links from an account server's download route.
pub struct SignedLinks {
    /// `https://…/v1/integrations/<app>`: the route is `<endpoint>/<id>/download`.
    endpoint: String,
    platform: String,
    arch: String,
    tokens: Arc<dyn Tokens>,
    get: Box<AuthorizedGet>,
}

impl std::fmt::Debug for SignedLinks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedLinks").field("endpoint", &self.endpoint).finish_non_exhaustive()
    }
}

/// `<endpoint>/<id>/download?platform=<p>&arch=<a>`.
pub fn link_url(endpoint: &str, id: &str, platform: &str, arch: &str) -> String {
    format!("{}/{id}/download?platform={platform}&arch={arch}", endpoint.trim_end_matches('/'))
}

/// A download route's answer, checked against the catalog entry.
pub fn parse_link(body: &[u8], entry: &Entry) -> Result<Link, LinkError> {
    let bad = |what: &str| LinkError::Failed(format!("the server's answer {what}"));
    let v: Value = serde_json::from_slice(body).map_err(|_| bad("isn't JSON"))?;
    let same = v.get("id").and_then(Value::as_str) == Some(entry.id.as_str())
        && v.get("version").and_then(Value::as_u64) == Some(u64::from(entry.version))
        && v.get("sha256").and_then(Value::as_str) == Some(entry.sha256.as_str())
        && v.get("size").and_then(Value::as_u64) == Some(entry.size);
    if !same {
        return Err(bad("is for another build (the list may have changed: refresh it)"));
    }
    let url = v.get("url").and_then(Value::as_str).filter(|u| url_ok(u)).ok_or_else(|| bad("has no https link"))?;
    let mirror = v.get("mirror").and_then(Value::as_str).filter(|u| url_ok(u)).map(str::to_string);
    Ok(Link { url: url.to_string(), mirror })
}

fn message(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("message").and_then(Value::as_str).map(|m| m.chars().take(300).collect())
}

impl SignedLinks {
    /// Links from `endpoint` through `get` (tests, or an app with its own HTTP client).
    pub fn with_get(endpoint: &str, platform: &str, arch: &str, tokens: Arc<dyn Tokens>, get: Box<AuthorizedGet>) -> SignedLinks {
        SignedLinks { endpoint: endpoint.to_string(), platform: platform.to_string(), arch: arch.to_string(), tokens, get }
    }

    /// Links from `endpoint` over HTTPS (ureq + rustls, the OS verifier).
    #[cfg(all(feature = "https", not(target_arch = "wasm32")))]
    pub fn new(endpoint: &str, platform: &str, arch: &str, tokens: Arc<dyn Tokens>, user_agent: &str) -> SignedLinks {
        SignedLinks::with_get(endpoint, platform, arch, tokens, https::getter(user_agent))
    }
}

impl Links for SignedLinks {
    fn link(&self, entry: &Entry) -> Result<Link, LinkError> {
        let url = link_url(&self.endpoint, &entry.id, &self.platform, &self.arch);
        if !url_ok(&url) {
            return Err(LinkError::Failed("the account server's address isn't https://".into()));
        }
        let mut token = self.tokens.token()?;
        for attempt in 0..2 {
            let (status, body) = (self.get)(&url, &token).map_err(LinkError::Failed)?;
            match status {
                200 => return parse_link(&body, entry),
                401 if attempt == 0 => token = self.tokens.fresh(&token)?,
                401 => return Err(LinkError::SignedOut),
                404 => return Err(LinkError::Failed("this build isn't offered any more (refresh the list)".into())),
                other => return Err(LinkError::Failed(message(&body).unwrap_or_else(|| format!("HTTP {other}")))),
            }
        }
        Err(LinkError::SignedOut)
    }
}

#[cfg(all(feature = "https", not(target_arch = "wasm32")))]
mod https {
    use std::io::Read;
    use std::sync::Arc;
    use std::time::Duration;

    use super::AuthorizedGet;

    /// Largest answer read.
    const MAX_ANSWER: usize = 64 * 1024;

    pub fn getter(user_agent: &str) -> Box<AuthorizedGet> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::Rustls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .unversioned_rustls_crypto_provider(provider)
                    .build(),
            )
            .https_only(false)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let user_agent = user_agent.to_string();
        Box::new(move |url: &str, token: &str| {
            let mut response = agent
                .get(url)
                .header("user-agent", &user_agent)
                .header("accept", "application/json")
                .header("authorization", &format!("Bearer {token}"))
                .call()
                .map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            let mut body = Vec::new();
            response.body_mut().as_reader().take(MAX_ANSWER as u64).read_to_end(&mut body).map_err(|e| e.to_string())?;
            Ok((status, body))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    fn entry() -> Entry {
        serde_json::from_value(json!({
            "id": "tool", "title": "Tool", "version": 3, "url": "", "sha256": "a".repeat(64), "size": 10, "entry": "tool/tool"
        }))
        .unwrap()
    }

    struct FakeTokens(Mutex<Vec<String>>);

    impl Tokens for FakeTokens {
        fn token(&self) -> Result<String, LinkError> {
            self.0.lock().unwrap().first().cloned().ok_or(LinkError::SignedOut)
        }
        fn fresh(&self, refused: &str) -> Result<String, LinkError> {
            let mut t = self.0.lock().unwrap();
            t.retain(|x| x != refused);
            t.first().cloned().ok_or(LinkError::SignedOut)
        }
    }

    fn answer() -> Vec<u8> {
        json!({"id": "tool", "version": 3, "sha256": "a".repeat(64), "size": 10,
               "url": "https://dl.example.com/tool-3.zip?exp=1&uid=2&sig=3", "mirror": null, "expires_at": 1})
        .to_string()
        .into_bytes()
    }

    #[test]
    fn a_link_is_asked_with_the_token_and_checked_against_the_entry() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s = Arc::clone(&seen);
        let links = SignedLinks::with_get(
            "https://api.example.com/v1/integrations/app/",
            "macos",
            "arm64",
            Arc::new(FakeTokens(Mutex::new(vec!["old".into(), "new".into()]))),
            Box::new(move |url, token| {
                s.lock().unwrap().push((url.to_string(), token.to_string()));
                Ok(if token == "old" { (401, Vec::new()) } else { (200, answer()) })
            }),
        );
        let link = links.link(&entry()).unwrap();
        assert_eq!(link, Link { url: "https://dl.example.com/tool-3.zip?exp=1&uid=2&sig=3".into(), mirror: None });
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0].0, "https://api.example.com/v1/integrations/app/tool/download?platform=macos&arch=arm64");
        assert_eq!(seen.iter().map(|s| s.1.as_str()).collect::<Vec<_>>(), ["old", "new"]);
    }

    #[test]
    fn signed_out_and_other_builds_are_refused() {
        let out = SignedLinks::with_get(
            "https://a.example.com/x",
            "macos",
            "arm64",
            Arc::new(FakeTokens(Mutex::new(vec![]))),
            Box::new(|_, _| Ok((200, answer()))),
        );
        assert_eq!(out.link(&entry()).unwrap_err(), LinkError::SignedOut);
        let mut other = entry();
        other.size = 11;
        assert!(matches!(parse_link(&answer(), &other), Err(LinkError::Failed(m)) if m.contains("another build")));
        let gone = SignedLinks::with_get(
            "https://a.example.com/x",
            "macos",
            "arm64",
            Arc::new(FakeTokens(Mutex::new(vec!["t".into()]))),
            Box::new(|_, _| Ok((429, br#"{"error":"rate_limited","message":"Too many downloads."}"#.to_vec()))),
        );
        assert_eq!(gone.link(&entry()).unwrap_err(), LinkError::Failed("Too many downloads.".into()));
        let plain = SignedLinks::with_get(
            "http://a.example.com/x",
            "macos",
            "arm64",
            Arc::new(FakeTokens(Mutex::new(vec!["t".into()]))),
            Box::new(|_, _| Ok((200, answer()))),
        );
        assert!(plain.link(&entry()).is_err());
    }
}
