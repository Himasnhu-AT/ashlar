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

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
pub use http::HttpTransport;

/// Reads a `Retry-After` value given as a whole number of seconds. The date form is not supported and
/// reads as absent.
#[cfg(any(test, all(feature = "http", not(target_arch = "wasm32"))))]
fn parse_retry_after(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(all(feature = "http", not(target_arch = "wasm32")))]
mod http {
    use std::time::Duration;

    use ashlar_trace_proto::HEADER_APP_KEY;

    use super::{Reply, Request, Transport, TransportError, parse_retry_after};

    /// Longest wait for the connection, including the TLS handshake.
    const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
    /// Longest wait for one whole request, from name lookup to the end of the reply.
    const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
    /// Replies are small JSON objects; anything longer is cut off here.
    const MAX_REPLY_BYTES: u64 = 64 * 1024;

    /// The built-in transport: blocking HTTPS with `ureq` and rustls.
    pub struct HttpTransport {
        agent: ureq::Agent,
        endpoint: String,
    }

    impl HttpTransport {
        /// `endpoint` is the server base URL, such as `https://telemetry.example.com`.
        pub fn new(endpoint: &str) -> Self {
            Self::with_timeouts(endpoint, CONNECT_TIMEOUT, TOTAL_TIMEOUT)
        }

        /// As [`HttpTransport::new`], with a limit on connecting and on one whole request.
        pub fn with_timeouts(endpoint: &str, connect: Duration, total: Duration) -> Self {
            let config = ureq::Agent::config_builder()
                .timeout_connect(Some(connect))
                .timeout_global(Some(total))
                // A status such as 429 is an answer to act on, not an error.
                .http_status_as_error(false)
                .max_redirects(0)
                .build();
            Self { agent: ureq::Agent::new_with_config(config), endpoint: endpoint.trim_end_matches('/').to_owned() }
        }
    }

    impl Transport for HttpTransport {
        fn send(&mut self, request: &Request<'_>) -> Result<Reply, TransportError> {
            let url = format!("{}{}", self.endpoint, request.path);
            let mut builder = self.agent.post(&url).header(HEADER_APP_KEY, request.app_key).header("content-type", "application/json");
            if request.gzip {
                builder = builder.header("content-encoding", "gzip");
            }
            let mut response = builder.send(request.body).map_err(|error| TransportError::Network(error.to_string()))?;
            let status = response.status().as_u16();
            let retry_after = response.headers().get("retry-after").and_then(|value| value.to_str().ok()).and_then(parse_retry_after);
            // The status has arrived, so a reply body that cannot be read is treated as empty.
            let body = response.body_mut().with_config().limit(MAX_REPLY_BYTES).read_to_vec().unwrap_or_default();
            Ok(Reply { status, body, retry_after })
        }
    }

    #[cfg(test)]
    mod tests {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        use super::*;

        /// Answers one request on the loopback interface and returns what it received.
        fn serve_once(reply: &'static str) -> (String, std::thread::JoinHandle<Vec<u8>>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut received = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let read = stream.read(&mut chunk).unwrap();
                    received.extend_from_slice(&chunk[..read]);
                    let text = String::from_utf8_lossy(&received).to_ascii_lowercase();
                    let complete = text.split_once("\r\n\r\n").is_some_and(|(head, _)| {
                        let length =
                            head.lines().find_map(|line| line.strip_prefix("content-length:")).and_then(|value| value.trim().parse::<usize>().ok());
                        length.is_some_and(|length| received.len() >= head.len() + 4 + length)
                    });
                    if complete || read == 0 {
                        break;
                    }
                }
                stream.write_all(reply.as_bytes()).unwrap();
                received
            });
            (endpoint, server)
        }

        #[test]
        fn sends_the_key_and_encoding_and_reads_the_reply() {
            let (endpoint, server) =
                serve_once("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 7\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
            let mut transport = HttpTransport::with_timeouts(&endpoint, Duration::from_secs(5), Duration::from_secs(5));
            let reply = transport.send(&Request { path: "/v1/ingest", app_key: "write-key", body: b"\x1f\x8bbody", gzip: true }).unwrap();
            assert_eq!(reply, Reply { status: 429, body: b"{}".to_vec(), retry_after: Some(Duration::from_secs(7)) });
            let received = server.join().unwrap();
            let text = String::from_utf8_lossy(&received).to_ascii_lowercase();
            assert!(text.starts_with("post /v1/ingest http/1.1\r\n"), "{text}");
            assert!(text.contains("\r\nx-tracer-key: write-key\r\n"), "{text}");
            assert!(text.contains("\r\ncontent-encoding: gzip\r\n"), "{text}");
            assert!(text.contains("\r\ncontent-type: application/json\r\n"), "{text}");
            assert!(received.ends_with(b"\x1f\x8bbody"));
        }

        #[test]
        fn a_plain_body_has_no_content_encoding() {
            let (endpoint, server) = serve_once("HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"accepted\":1}");
            let mut transport = HttpTransport::with_timeouts(&endpoint, Duration::from_secs(5), Duration::from_secs(5));
            let reply = transport.send(&Request { path: "/v1/forget", app_key: "write-key", body: b"{}", gzip: false }).unwrap();
            assert_eq!(reply, Reply { status: 200, body: br#"{"accepted":1}"#.to_vec(), retry_after: None });
            let text = String::from_utf8_lossy(&server.join().unwrap()).to_ascii_lowercase();
            assert!(text.starts_with("post /v1/forget http/1.1\r\n"), "{text}");
            assert!(!text.contains("content-encoding"), "{text}");
        }

        #[test]
        fn an_unreachable_server_is_a_network_error() {
            // Bind and drop to get a loopback port that nothing listens on.
            let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
            let mut transport = HttpTransport::with_timeouts(&format!("http://127.0.0.1:{port}"), Duration::from_secs(2), Duration::from_secs(2));
            let result = transport.send(&Request { path: "/v1/ingest", app_key: "k", body: b"{}", gzip: false });
            assert!(matches!(result, Err(TransportError::Network(_))));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_is_whole_seconds() {
        assert_eq!(parse_retry_after("120"), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after(" 5 "), Some(Duration::from_secs(5)));
        assert_eq!(parse_retry_after("0"), Some(Duration::ZERO));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2026 07:28:00 GMT"), None);
        assert_eq!(parse_retry_after("-1"), None);
        assert_eq!(parse_retry_after(""), None);
    }
}
