//! The SDK's transport.

use std::sync::Arc;
use std::time::Duration;

use ashlar_trace::transport::{Reply, Request, Transport, TransportError};

/// HTTPS with ureq, rustls and the RustCrypto provider (no `ring`, no bundled roots) and the
/// operating system's certificate verifier. Plain HTTP only to a loopback address
/// ([`crate::endpoint_host`]); no redirects.
pub struct Https {
    agent: ureq::Agent,
    endpoint: String,
}

/// Replies are small JSON objects.
const MAX_REPLY_BYTES: u64 = 64 * 1024;

impl Https {
    pub fn new(endpoint: &str) -> Https {
        let provider = Arc::new(rustls_rustcrypto::provider());
        let agent = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .provider(ureq::tls::TlsProvider::Rustls)
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .unversioned_rustls_crypto_provider(provider)
                    .build(),
            )
            .https_only(endpoint.starts_with("https://"))
            // a 429 or 507 is an answer the SDK acts on, not an error
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        Https { agent, endpoint: endpoint.trim_end_matches('/').to_string() }
    }
}

impl Transport for Https {
    fn send(&mut self, request: &Request<'_>) -> Result<Reply, TransportError> {
        let url = format!("{}{}", self.endpoint, request.path);
        let mut b = self.agent.post(&url).header(ashlar_trace::proto::HEADER_APP_KEY, request.app_key).header("content-type", "application/json");
        if request.gzip {
            b = b.header("content-encoding", "gzip");
        }
        let mut response = b.send(request.body).map_err(|e| TransportError::Network(e.to_string()))?;
        let status = response.status().as_u16();
        let retry_after =
            response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok()).map(Duration::from_secs);
        let body = response.body_mut().with_config().limit(MAX_REPLY_BYTES).read_to_vec().unwrap_or_default();
        Ok(Reply { status, body, retry_after })
    }
}
