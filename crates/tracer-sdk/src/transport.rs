//! How batches leave the machine. Public so a host or a test can supply its own.

use std::time::Duration;

/// A request the SDK wants delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request<'a> {
    /// Route path such as `/v1/ingest`, relative to the configured endpoint.
    pub path: &'a str,
    /// The app write key, sent in the `x-tracer-key` header.
    pub app_key: &'a str,
    /// The JSON body, gzip-compressed when `gzip` is true.
    pub body: &'a [u8],
    pub gzip: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    /// The server's `Retry-After`, when it sent one.
    pub retry_after: Option<Duration>,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The request did not reach the server. Retried later.
    #[error("network error: {0}")]
    Network(String),
}

/// Delivers one request and reports what the server said. Called only from the SDK's worker thread.
pub trait Transport: Send + 'static {
    fn send(&mut self, request: &Request<'_>) -> Result<Reply, TransportError>;
}
