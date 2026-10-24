//! HTTPS for the providers: ureq with rustls, the RustCrypto provider and the operating system's
//! certificate verifier; no redirects. Plain HTTP only to this computer (a local model server or a
//! test server), or to another host when no secret is sent.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use crate::error::Error;

/// Longest error body read.
const MAX_ERROR_BODY: u64 = 64 * 1024;
/// Longest JSON reply read (a status or a model list).
pub(crate) const MAX_JSON_BODY: u64 = 4 * 1024 * 1024;

pub(crate) struct Http {
    agent: ureq::Agent,
}

pub(crate) struct Reply {
    pub status: u16,
    pub retry_after: Option<u64>,
    pub body: Box<dyn Read + Send>,
}

impl Reply {
    /// The body of a failed request, bounded.
    pub fn error_body(self) -> Vec<u8> {
        let mut out = Vec::new();
        let _ = self.body.take(MAX_ERROR_BODY).read_to_end(&mut out);
        out
    }

    pub fn json(self) -> Result<serde_json::Value, Error> {
        let mut out = Vec::new();
        self.body.take(MAX_JSON_BODY).read_to_end(&mut out).map_err(|e| Error::Protocol(format!("reading the reply: {e}")))?;
        serde_json::from_slice(&out).map_err(|e| Error::Protocol(format!("the reply is not JSON: {e}")))
    }
}

/// The host of `url`, lowercase, without brackets or port.
pub(crate) fn host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = authority.strip_prefix('[') { v6.split(']').next()? } else { authority.split(':').next()? };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

pub(crate) fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "::1" || host.starts_with("127.") || host.ends_with(".localhost")
}

/// Refuse a URL that is not `https://` unless it is on this computer or carries no secret.
pub(crate) fn check_url(url: &str, sends_secret: bool) -> Result<(), Error> {
    let lower = url.to_ascii_lowercase();
    let Some(h) = host(url) else { return Err(Error::Config(format!("`{url}` is not a valid URL"))) };
    if lower.starts_with("https://") {
        return Ok(());
    }
    if !lower.starts_with("http://") {
        return Err(Error::Config(format!("`{url}` must start with https://")));
    }
    if is_loopback(&h) || !sends_secret {
        return Ok(());
    }
    Err(Error::Config(format!("`{url}` must use https:// to send an API key to {h}")))
}

impl Http {
    pub fn new() -> Self {
        let provider = Arc::new(rustls_rustcrypto::provider());
        let agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::Rustls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .unversioned_rustls_crypto_provider(provider)
                    .build(),
            )
            // checked per request (`check_url`): plain HTTP to a local server is allowed
            .https_only(false)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(15)))
            // time to the response headers (the model starts streaming at once)
            .timeout_recv_response(Some(Duration::from_secs(180)))
            // a whole streamed reply
            .timeout_global(Some(Duration::from_secs(30 * 60)))
            .build()
            .into();
        Self { agent }
    }

    pub fn post(&self, url: &str, headers: &[(&str, &str)], body: Vec<u8>, service: &str) -> Result<Reply, Error> {
        let mut b = self.agent.post(url).header("content-type", "application/json");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        reply(b.send(body), service)
    }

    pub fn get(&self, url: &str, headers: &[(&str, &str)], service: &str) -> Result<Reply, Error> {
        let mut b = self.agent.get(url);
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        reply(b.call(), service)
    }
}

fn reply(r: Result<ureq::http::Response<ureq::Body>, ureq::Error>, service: &str) -> Result<Reply, Error> {
    let response = r.map_err(|e| transport_error(e, service))?;
    let status = response.status().as_u16();
    let retry_after = response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
    let body: Box<dyn Read + Send> = Box::new(response.into_body().into_reader());
    Ok(Reply { status, retry_after, body })
}

/// A transport failure in words; never built from a request's headers.
fn transport_error(e: ureq::Error, service: &str) -> Error {
    match e {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Io(_) | ureq::Error::Timeout(_) => {
            Error::Offline(service.to_string())
        }
        ureq::Error::Tls(m) => Error::Offline(format!("{service} (a secure connection failed: {m})")),
        ureq::Error::Rustls(r) => Error::Offline(format!("{service} (a secure connection failed: {r})")),
        ureq::Error::BadUri(u) => Error::Config(format!("not a valid URL: {u}")),
        other => Error::Other(format!("{service}: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_url_rules() {
        assert_eq!(host("https://api.example.com/v1").as_deref(), Some("api.example.com"));
        assert_eq!(host("http://[::1]:8080/x").as_deref(), Some("::1"));
        assert_eq!(host("http://user@LocalHost:11434").as_deref(), Some("localhost"));
        assert!(check_url("https://api.example.com", true).is_ok());
        assert!(check_url("http://127.0.0.1:1234/v1", true).is_ok());
        assert!(check_url("http://192.168.1.4:11434/v1", false).is_ok());
        assert!(check_url("http://192.168.1.4:11434/v1", true).is_err());
        assert!(check_url("ftp://x", false).is_err());
        assert!(check_url("nonsense", false).is_err());
    }
}
