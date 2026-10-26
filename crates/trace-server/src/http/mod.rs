//! HTTP API: `POST /v1/ingest`, `POST /v1/forget`, `GET /v1/health`, plus the dashboard routes.
//!
//! [`api_router`] returns the routes without state so the dashboard can merge its own routes into the same
//! router; [`router`] attaches the state. [`serve`] runs a router on a listener until a future completes.

pub mod limiter;

use std::future::Future;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ashlar_trace_proto::{ForgetRequest, HEADER_APP_KEY, MAX_ENVELOPE_BYTES, SCHEMA_VERSION, routes};
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use http_body_util::BodyExt;
use serde::Serialize;
use tokio::sync::Semaphore;
use tower_service::Service;

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

/// Password checks (argon2) running at once. Each costs tens of megabytes and a good fraction of a second.
pub const LOGIN_CONCURRENCY: usize = 4;
/// How long a measured database size is trusted.
const SIZE_CACHE: Duration = Duration::from_secs(2);

/// State shared by every handler. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<dyn Store>,
    pub config: Arc<Config>,
    pub limiters: Arc<Limiters>,
    /// Permits for concurrent password verification.
    pub login_slots: Arc<Semaphore>,
    size: Arc<Mutex<Option<(Instant, bool)>>>,
}

impl AppState {
    pub fn new(store: Arc<dyn Store>, config: Config) -> Self {
        let limiters = Arc::new(Limiters::new(config.rate_per_min, config.key_rate_per_min));
        Self { store, config: Arc::new(config), limiters, login_slots: Arc::new(Semaphore::new(LOGIN_CONCURRENCY)), size: Arc::new(Mutex::new(None)) }
    }

    /// True when the database is over `TRACER_MAX_DB_MB`. Only ingest asks: the dashboard and login keep
    /// working so the operator can still get in and fix it. A failed measurement counts as "not full".
    pub async fn storage_full(&self) -> bool {
        let limit = self.config.max_db_mb.saturating_mul(1024 * 1024);
        if limit == 0 {
            return false;
        }
        if let Some((at, full)) = *self.size.lock().unwrap_or_else(PoisonError::into_inner)
            && at.elapsed() < SIZE_CACHE
        {
            return full;
        }
        let full = match blocking(&self.store, |store| store.size_bytes()).await {
            Ok(bytes) => bytes > limit,
            Err(error) => {
                log::error!("could not measure the database: {error}");
                false
            }
        };
        *self.size.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), full));
        full
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

/// Connection-level limits against slow and idle clients.
#[derive(Debug, Clone, Copy)]
pub struct ServerLimits {
    /// A client gets this long to send a request's headers. It also bounds an idle keep-alive connection,
    /// since the clock runs while the server waits for the next request.
    pub header_read_timeout: Duration,
    /// Connections served at once. Further clients wait in the accept queue until one closes.
    pub max_connections: usize,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self { header_read_timeout: Duration::from_secs(15), max_connections: 1024 }
    }
}

/// How long shutdown waits for in-flight requests.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Serves `app` on `listener` until `shutdown` completes, then lets in-flight requests finish.
pub async fn serve(listener: tokio::net::TcpListener, app: Router, shutdown: impl Future<Output = ()> + Send + 'static) -> std::io::Result<()> {
    serve_with(listener, app, ServerLimits::default(), shutdown).await
}

/// [`serve`] with explicit connection limits. Runs hyper directly (HTTP/1.1) so it can set a header read
/// timeout and cap the number of open connections, which `axum::serve` does not offer.
pub async fn serve_with(
    listener: tokio::net::TcpListener,
    app: Router,
    limits: ServerLimits,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    use hyper_util::rt::{TokioIo, TokioTimer};
    use hyper_util::server::graceful::GracefulShutdown;

    let graceful = GracefulShutdown::new();
    let slots = Arc::new(Semaphore::new(limits.max_connections.max(1)));
    tokio::pin!(shutdown);
    loop {
        let permit = tokio::select! {
            permit = Arc::clone(&slots).acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => break,
            },
            () = &mut shutdown => break,
        };
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(error) => {
                    // Usually too many open files: back off instead of spinning.
                    log::warn!("accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let _ = stream.set_nodelay(true);
        let app = app.clone();
        let service = hyper::service::service_fn(move |mut request: hyper::Request<hyper::body::Incoming>| {
            request.extensions_mut().insert(ConnectInfo(peer));
            let mut app = app.clone();
            async move { app.call(request).await }
        });
        let mut builder = hyper::server::conn::http1::Builder::new();
        builder.timer(TokioTimer::new()).header_read_timeout(limits.header_read_timeout);
        let connection = graceful.watch(builder.serve_connection(TokioIo::new(stream), service));
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                log::debug!("connection ended: {error}");
            }
            drop(permit);
        });
    }
    if tokio::time::timeout(SHUTDOWN_GRACE, graceful.shutdown()).await.is_err() {
        log::warn!("some connections were still open after {} s; closing", SHUTDOWN_GRACE.as_secs());
    }
    Ok(())
}

/// Resolves when the process is asked to stop: ctrl-c (SIGINT) or, on Unix, SIGTERM, which is what
/// `docker stop`, systemd and `kill` send.
pub async fn ctrl_c() {
    #[cfg(unix)]
    if let Ok(mut terminate) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
        return;
    }
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

/// A refusal sent before the request body was read. The connection cannot carry another request
/// (the unread body is still in it), so the client is told to close it: one that kept it in its pool
/// would see its next request fail with a dropped connection instead of an answer.
fn unread_body(mut response: Response) -> Response {
    response.headers_mut().insert(header::CONNECTION, HeaderValue::from_static("close"));
    response
}

fn failure(status: StatusCode, message: &str) -> Response {
    #[derive(Serialize)]
    struct Failure<'a> {
        error: &'a str,
    }
    json(status, &Failure { error: message })
}

fn storage_failure(error: &StoreError) -> Response {
    if matches!(error, StoreError::Busy) {
        // Shed load: the client keeps its data and retries after the delay.
        let mut response = failure(StatusCode::SERVICE_UNAVAILABLE, "busy");
        response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(1u64));
        return response;
    }
    // The error text may name tables or values, so it goes to the log only.
    log::error!("{error}");
    failure(StatusCode::INTERNAL_SERVER_ERROR, "storage error")
}

// ---------------------------------------------------------------------------------------------------------
// handlers

async fn health_handler(State(state): State<AppState>, request: Request) -> Response {
    #[derive(Serialize)]
    struct Health {
        status: &'static str,
        version: &'static str,
    }
    let peer = request.extensions().get::<ConnectInfo<SocketAddr>>().map(|info| info.0);
    if let Err(response) = limit_address(&state, request.headers(), peer) {
        return *response;
    }
    match blocking(&state.store, |store| store.health()).await {
        Ok(()) => json(StatusCode::OK, &Health { status: "ok", version: env!("CARGO_PKG_VERSION") }),
        Err(error) => {
            log::error!("health check failed: {error}");
            failure(StatusCode::SERVICE_UNAVAILABLE, "storage unavailable")
        }
    }
}

fn rate_limited(wait: u64) -> Response {
    let mut response = failure(StatusCode::TOO_MANY_REQUESTS, "rate limited");
    response.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(wait));
    response
}

/// Takes one request from the client address's bucket. Done first on every public route, before anything
/// that costs a database read, so unknown keys and the health route are limited as well.
fn limit_address(state: &AppState, headers: &HeaderMap, peer: Option<SocketAddr>) -> Result<(), Box<Response>> {
    let addr = client_ip(headers, peer, state.config.trust_proxy);
    state.limiters.check_addr(addr, Instant::now()).map_err(|wait| Box::new(rate_limited(wait)))
}

/// The first steps of the ingest procedure, shared by ingest and forget: the address limit, then the key,
/// then the per-key capacity guard. A request the address bucket refuses never touches the key bucket.
async fn authenticate(state: &AppState, headers: &HeaderMap, peer: Option<SocketAddr>) -> Result<AppRecord, Response> {
    limit_address(state, headers, peer).map_err(|response| *response)?;
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
    if let Err(wait) = state.limiters.check_key(&key, Instant::now()) {
        return Err(rate_limited(wait));
    }
    Ok(app)
}

/// The address used for rate limiting only. With a trusted proxy it is the last `X-Forwarded-For` entry,
/// the one the proxy itself appended; earlier entries are client-supplied and can be forged.
pub(crate) fn client_ip(headers: &HeaderMap, peer: Option<SocketAddr>, trust_proxy: bool) -> IpAddr {
    // The proxy appends to the last header line it sees, so only the last line's last entry is trustworthy
    // even when a client sent several `X-Forwarded-For` lines of its own.
    if trust_proxy
        && let Some(addr) = headers
            .get_all("x-forwarded-for")
            .iter()
            .next_back()
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
        Err(response) => return unread_body(response),
    };
    if state.storage_full().await {
        return unread_body(failure(StatusCode::INSUFFICIENT_STORAGE, "storage full"));
    }
    let bytes = match read_body(&parts.headers, body).await {
        Ok(bytes) => bytes,
        Err(response) => return response,
    };
    if let Err(status) = check_schema(&bytes) {
        return failure(status, if status == StatusCode::UNPROCESSABLE_ENTITY { "unsupported schema version" } else { "malformed envelope" });
    }
    let envelope: ashlar_trace_proto::Envelope = match serde_json::from_slice(&bytes) {
        Ok(envelope) => envelope,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "malformed envelope"),
    };
    let prepared = match ingest::prepare_with(envelope, crate::now_ms(), state.config.retention_days) {
        Ok(prepared) => prepared,
        Err(error) => return failure(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    match blocking(&state.store, move |store| store.ingest_owned(app.id, prepared)).await {
        Ok(response) => json(StatusCode::OK, &response),
        Err(error) => storage_failure(&error),
    }
}

async fn forget_handler(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|info| info.0);
    let app = match authenticate(&state, &parts.headers, peer).await {
        Ok(app) => app,
        Err(response) => return unread_body(response),
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
    // The reply is the same whether or not the install existed, so it cannot be used to probe for install ids.
    match blocking(&state.store, move |store| store.forget(app.id, &forget.install_id)).await {
        Ok(_) => {
            #[derive(Serialize)]
            struct Forgotten {
                ok: bool,
            }
            json(StatusCode::OK, &Forgotten { ok: true })
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
    fn several_forwarded_for_lines_use_the_last_line_and_its_last_entry() {
        let mut headers = HeaderMap::new();
        headers.append("x-forwarded-for", HeaderValue::from_static("6.6.6.6"));
        headers.append("x-forwarded-for", HeaderValue::from_static("5.5.5.5, 9.9.9.9"));
        let peer: SocketAddr = "10.0.0.1:5555".parse().unwrap();
        assert_eq!(client_ip(&headers, Some(peer), true), "9.9.9.9".parse::<IpAddr>().unwrap());
        headers.append("x-forwarded-for", HeaderValue::from_static("not an address"));
        assert_eq!(client_ip(&headers, Some(peer), true), peer.ip());
    }

    #[test]
    fn a_full_write_queue_answers_503_with_retry_after_and_other_failures_500() {
        let busy = storage_failure(&StoreError::Busy);
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(busy.headers().get(header::RETRY_AFTER).and_then(|v| v.to_str().ok()), Some("1"));
        let broken = storage_failure(&StoreError::Backend("disk I/O error".into()));
        assert_eq!(broken.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(broken.headers().get(header::RETRY_AFTER).is_none());
    }

    #[test]
    fn schema_probe() {
        assert!(check_schema(br#"{"schema":1}"#).is_ok());
        assert_eq!(check_schema(br#"{"schema":2}"#).unwrap_err(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(check_schema(b"nope").unwrap_err(), StatusCode::BAD_REQUEST);
        assert_eq!(check_schema(br#"{"schema":0}"#).unwrap_err(), StatusCode::BAD_REQUEST);
    }
}
