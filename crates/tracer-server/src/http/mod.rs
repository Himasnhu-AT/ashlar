//! HTTP API: `POST /v1/ingest`, `POST /v1/forget`, `GET /v1/health`, plus the dashboard routes.
//!
//! [`api_router`] returns the routes without state so the dashboard can merge its own routes into the same
//! router; [`router`] attaches the state. [`serve`] runs a router on a listener until a future completes.

pub mod limiter;

use std::future::Future;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use serde::Serialize;
use tracer_proto::{ForgetRequest, HEADER_APP_KEY, MAX_ENVELOPE_BYTES, SCHEMA_VERSION, routes};

pub use limiter::Limiters;

use crate::config::Config;
use crate::ingest;
use crate::store::{AppRecord, Store, StoreError, StoreResult};

/// How long a client gets to deliver its request body.
const BODY_TIMEOUT: Duration = Duration::from_secs(30);
/// Slack above `MAX_ENVELOPE_BYTES` for a gzip body, which can be marginally larger than its input.
const GZIP_SLACK: usize = 4096;
const MAX_KEY_LEN: usize = 200;
/// How much of an oversized body is read and thrown away before the connection is given up on.
const DRAIN_MAX: usize = 8 * 1024 * 1024;

/// State shared by every handler. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn Store>,
    pub config: Arc<Config>,
    pub limiters: Arc<Limiters>,
}

impl AppState {
    pub fn new(store: Arc<dyn Store>, config: Config) -> Self {
        let limiters = Arc::new(Limiters::new(config.rate_per_min));
        Self { store, config: Arc::new(config), limiters }
    }
}

/// The API routes, still waiting for their state.
pub fn api_router() -> Router<AppState> {
    Router::new()
        .route(routes::INGEST, post(ingest_handler))
        .route(routes::FORGET, post(forget_handler))
        .route(routes::HEALTH, get(health_handler))
        .merge(crate::dashboard::routes())
}

/// The API routes with state attached.
pub fn router(state: AppState) -> Router {
    api_router().with_state(state)
}

/// Serves `app` on `listener` until `shutdown` completes, then lets in-flight requests finish.
pub async fn serve(listener: tokio::net::TcpListener, app: Router, shutdown: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(shutdown).await
}

/// Resolves when the process receives ctrl-c.
pub async fn ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        // Signal handling is unavailable: never shut down on our own.
        std::future::pending::<()>().await;
    }
}

/// Runs a blocking store call off the async threads.
pub async fn blocking<T, F>(store: &Arc<dyn Store>, f: F) -> StoreResult<T>
where
    T: Send + 'static,
    F: FnOnce(&dyn Store) -> StoreResult<T> + Send + 'static,
{
    let store = Arc::clone(store);
    match tokio::task::spawn_blocking(move || f(store.as_ref())).await {
        Ok(result) => result,
        Err(_) => Err(StoreError::Backend("storage task failed".to_string())),
    }
}

// ---------------------------------------------------------------------------------------------------------
// responses

fn json<T: Serialize>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_vec(body) {
        Ok(bytes) => (status, [(header::CONTENT_TYPE, HeaderValue::from_static("application/json"))], bytes).into_response(),
        Err(_) => status.into_response(),
    }
}

fn failure(status: StatusCode, message: &str) -> Response {
    #[derive(Serialize)]
    struct Failure<'a> {
        error: &'a str,
    }
    json(status, &Failure { error: message })
}

fn storage_failure(error: &StoreError) -> Response {
    // The error text may name tables or values, so it goes to the log only.
    log::error!("{error}");
    failure(StatusCode::INTERNAL_SERVER_ERROR, "storage error")
}

// ---------------------------------------------------------------------------------------------------------
// handlers

async fn health_handler(State(state): State<AppState>) -> Response {
    #[derive(Serialize)]
    struct Health {
        status: &'static str,
        version: &'static str,
    }
    match blocking(&state.store, |store| store.health()).await {
        Ok(()) => json(StatusCode::OK, &Health { status: "ok", version: env!("CARGO_PKG_VERSION") }),
        Err(error) => {
            log::error!("health check failed: {error}");
            failure(StatusCode::SERVICE_UNAVAILABLE, "storage unavailable")
        }
    }
}

/// Steps 1 and 2 of the ingest procedure, shared by ingest and forget: the key, then the rate limits.
async fn authenticate(state: &AppState, headers: &HeaderMap, peer: Option<SocketAddr>) -> Result<AppRecord, Response> {
    let key = headers
        .get(HEADER_APP_KEY)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|key| !key.is_empty() && key.len() <= MAX_KEY_LEN)
        .map(str::to_string)
        .ok_or_else(|| failure(StatusCode::UNAUTHORIZED, "missing or unknown key"))?;
    let lookup = key.clone();
    let app = match blocking(&state.store, move |store| store.lookup_key(&lookup)).await {
        Ok(Some(app)) => app,
        Ok(None) => return Err(failure(StatusCode::UNAUTHORIZED, "missing or unknown key")),
        Err(error) => return Err(storage_failure(&error)),
    };
    let addr = client_ip(headers, peer, state.config.trust_proxy);
    if let Err(wait) = state.limiters.check(&key, addr, Instant::now()) {
        let mut response = failure(StatusCode::TOO_MANY_REQUESTS, "rate limited");
        response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(wait));
        return Err(response);
    }
    Ok(app)
}

/// The address used for rate limiting only. With a trusted proxy it is the last `X-Forwarded-For` entry,
/// the one the proxy itself appended; earlier entries are client-supplied and can be forged.
pub(crate) fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>, trust_proxy: bool) -> IpAddr {
    if trust_proxy
        && let Some(addr) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit(',').next())
            .and_then(|last| last.trim().parse::<IpAddr>().ok())
    {
        return addr;
    }
    peer.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |peer| peer.ip())
}

async fn ingest_handler(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|info| info.0);
    let app = match authenticate(&state, &parts.headers, peer).await {
        Ok(app) => app,
        Err(response) => return response,
    };
    let bytes = match read_body(&parts.headers, body).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    if let Err(status) = check_schema(&bytes) {
        return failure(status, if status == StatusCode::UNPROCESSABLE_ENTITY { "unsupported schema version" } else { "malformed envelope" });
    }
    let envelope: tracer_proto::Envelope = match serde_json::from_slice(&bytes) {
        Ok(envelope) => envelope,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "malformed envelope"),
    };
    let prepared = match ingest::prepare(envelope, crate::now_ms()) {
        Ok(prepared) => prepared,
        Err(error) => return failure(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    match blocking(&state.store, move |store| store.ingest(app.id, &prepared)).await {
        Ok(response) => json(StatusCode::OK, &response),
        Err(error) => storage_failure(&error),
    }
}

async fn forget_handler(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|info| info.0);
    let app = match authenticate(&state, &parts.headers, peer).await {
        Ok(app) => app,
        Err(response) => return response,
    };
    let bytes = match read_body(&parts.headers, body).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    if let Err(status) = check_schema(&bytes) {
        return failure(status, if status == StatusCode::UNPROCESSABLE_ENTITY { "unsupported schema version" } else { "malformed envelope" });
    }
    let forget: ForgetRequest = match serde_json::from_slice(&bytes) {
        Ok(forget) => forget,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "malformed request"),
    };
    if forget.install_id.is_empty() || forget.install_id.len() > 64 {
        return failure(StatusCode::BAD_REQUEST, "install_id is missing or malformed");
    }
    match blocking(&state.store, move |store| store.forget(app.id, &forget.install_id)).await {
        Ok(deleted) => {
            #[derive(Serialize)]
            struct Forgotten {
                deleted: u64,
            }
            json(StatusCode::OK, &Forgotten { deleted })
        }
        Err(error) => storage_failure(&error),
    }
}

/// A `schema` above ours is 422; a body without one is malformed.
fn check_schema(bytes: &[u8]) -> Result<(), StatusCode> {
    #[derive(serde::Deserialize)]
    struct Probe {
        schema: u32,
    }
    match serde_json::from_slice::<Probe>(bytes) {
        Ok(probe) if probe.schema > SCHEMA_VERSION => Err(StatusCode::UNPROCESSABLE_ENTITY),
        Ok(probe) if probe.schema == 0 => Err(StatusCode::BAD_REQUEST),
        Ok(_) => Ok(()),
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}

/// Reads the body, un-gzipping if asked, holding at most `MAX_ENVELOPE_BYTES` of decompressed data.
async fn read_body(headers: &HeaderMap, body: Body) -> Result<Vec<u8>, Response> {
    let gzip = match headers.get(header::CONTENT_ENCODING).map(|value| value.to_str().unwrap_or("?").trim().to_ascii_lowercase()) {
        None => false,
        Some(encoding) if encoding.is_empty() || encoding == "identity" => false,
        Some(encoding) if encoding == "gzip" || encoding == "x-gzip" => true,
        Some(_) => return Err(failure(StatusCode::BAD_REQUEST, "unsupported content encoding")),
    };
    let limit = if gzip { MAX_ENVELOPE_BYTES + GZIP_SLACK } else { MAX_ENVELOPE_BYTES };
    let declared = headers.get(header::CONTENT_LENGTH).and_then(|value| value.to_str().ok()).and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > limit + DRAIN_MAX) {
        return Err(too_large());
    }
    let raw = match tokio::time::timeout(BODY_TIMEOUT, collect(body, limit)).await {
        Ok(Ok(raw)) => raw,
        Ok(Err(response)) => return Err(response),
        Err(_) => return Err(failure(StatusCode::REQUEST_TIMEOUT, "request body timed out")),
    };
    if !gzip {
        return Ok(raw);
    }
    // `take` bounds the output however well the input compresses.
    let mut decoder = flate2::read::GzDecoder::new(raw.as_slice()).take(MAX_ENVELOPE_BYTES as u64 + 1);
    let mut out = Vec::new();
    if decoder.read_to_end(&mut out).is_err() {
        return Err(failure(StatusCode::BAD_REQUEST, "malformed gzip body"));
    }
    if out.len() > MAX_ENVELOPE_BYTES {
        return Err(too_large());
    }
    Ok(out)
}

fn too_large() -> Response {
    failure(StatusCode::PAYLOAD_TOO_LARGE, "body too large")
}

/// Collects a body frame by frame. Past `limit` nothing more is kept; a little more is read and discarded
/// (up to `DRAIN_MAX`) so the client sees the 413 rather than a reset connection, then it is refused.
async fn collect(mut body: Body, limit: usize) -> Result<Vec<u8>, Response> {
    let mut out = Vec::new();
    let mut total = 0usize;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| failure(StatusCode::BAD_REQUEST, "body could not be read"))?;
        if let Ok(data) = frame.into_data() {
            total = total.saturating_add(data.len());
            if total > limit + DRAIN_MAX {
                return Err(too_large());
            }
            if total <= limit {
                out.extend_from_slice(&data);
            }
        }
    }
    if total > limit {
        return Err(too_large());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ip_ignores_forwarded_for_unless_trusted() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("1.1.1.1, 9.9.9.9"));
        let peer: SocketAddr = "10.0.0.1:5555".parse().unwrap();
        assert_eq!(client_ip(&headers, Some(peer), false), peer.ip());
        assert_eq!(client_ip(&headers, Some(peer), true), "9.9.9.9".parse::<IpAddr>().unwrap());
        headers.insert("x-forwarded-for", HeaderValue::from_static("garbage"));
        assert_eq!(client_ip(&headers, Some(peer), true), peer.ip());
    }

    #[test]
    fn schema_probe() {
        assert!(check_schema(br#"{"schema":1}"#).is_ok());
        assert_eq!(check_schema(br#"{"schema":2}"#).unwrap_err(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(check_schema(b"nope").unwrap_err(), StatusCode::BAD_REQUEST);
        assert_eq!(check_schema(br#"{"schema":0}"#).unwrap_err(), StatusCode::BAD_REQUEST);
    }
}
