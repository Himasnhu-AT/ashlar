//! The web dashboard: server-rendered pages, inline SVG charts and release reports.
//!
//! - [`auth`]: login, logout, sessions, CSRF.
//! - [`data`]: query parameters and page loaders over the [`Store`](crate::store::Store).
//! - [`metrics`]: metric definitions (crash-free rates, percentiles) as pure functions.
//! - [`charts`]: SVG generation, pure functions with their own geometry tests.
//! - [`view`], [`pages`], [`issue`]: markup. [`report`]: the release report in four formats.
//!
//! [`routes`] is merged into the API router by [`crate::http::api_router`]. Every page requires a login and
//! redirects to `/login` otherwise; every state change is a POST carrying a CSRF token.

pub mod auth;
pub mod charts;
pub mod data;
pub mod fmt;
pub mod form;
pub mod issue;
pub mod metrics;
pub mod pages;
pub mod report;
pub mod view;

use axum::Router;
use axum::extract::{Path, RawQuery, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::Markup;

use self::auth::Session;
use self::data::{Filters, Params};
use self::view::Shell;
use crate::cli;
use crate::http::{AppState, blocking};
use crate::ingest::DAY_MS;
use crate::store::{AppRecord, Store, StoreError, StoreResult};

const CSP: &str =
    "default-src 'self'; style-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'";
const DOWNLOAD_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; sandbox";

/// The dashboard routes, still waiting for their state.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/login", get(auth::login_get).post(auth::login_post))
        .route("/logout", post(auth::logout_post))
        .route("/static/dashboard.css", get(stylesheet))
        .route("/apps/{slug}", get(overview))
        .route("/apps/{slug}/crashes", get(crashes))
        .route("/apps/{slug}/crashes/{id}", get(issue))
        .route("/apps/{slug}/crashes/{id}/status", post(issue_status))
        .route("/apps/{slug}/performance", get(performance))
        .route("/apps/{slug}/usage", get(usage))
        .route("/apps/{slug}/reports", get(reports))
        .route("/apps/{slug}/settings", get(settings))
        .route("/apps/{slug}/settings/keys", post(key_create))
        .route("/apps/{slug}/settings/keys/{key}/revoke", post(key_revoke))
        .layer(from_fn(security_headers))
}

/// Adds the security headers to every dashboard response. Handlers may set their own policy first.
async fn security_headers(request: Request, next: Next) -> Response {
    let is_static = request.uri().path().starts_with("/static/");
    let has_body = !matches!(*request.method(), Method::GET | Method::HEAD);
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    // A form handler may answer before it reads the body (not signed in, no such app, too many
    // attempts). Such a connection cannot carry another request, so the client is told to close
    // it; forms are sent by hand, so a new connection each time costs nothing.
    if has_body {
        headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
    }
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.entry(header::CONTENT_SECURITY_POLICY).or_insert(HeaderValue::from_static(CSP));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(if is_static { "public, max-age=300" } else { "no-store" }));
    response
}

// ---------------------------------------------------------------------------------------------------------
// responses

pub(crate) fn html_response(status: StatusCode, markup: Markup) -> Response {
    (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], markup.into_string()).into_response()
}

pub(crate) fn redirect(location: &str) -> Response {
    match HeaderValue::from_str(location) {
        Ok(value) => (StatusCode::SEE_OTHER, [(header::LOCATION, value)]).into_response(),
        Err(_) => StatusCode::SEE_OTHER.into_response(),
    }
}

/// A message page. With a session it keeps the navigation; without one it is bare.
pub(crate) fn error_page(status: StatusCode, title: &str, message: &str, session: Option<&Session>) -> Response {
    let body = maud::html! {
        (view::heading(title, None))
        p { (message) }
        p { a href="/" { "Back to the apps" } }
    };
    let markup = match session {
        Some(session) => view::shell(&Shell { title, user: &session.username, csrf: &session.csrf, app: None, active: "" }, body),
        None => view::plain(title, body),
    };
    html_response(status, markup)
}

fn storage_error(error: &StoreError, session: Option<&Session>) -> Response {
    // The error text may name tables or values, so it goes to the log only.
    log::error!("dashboard query failed: {error}");
    error_page(StatusCode::INTERNAL_SERVER_ERROR, "Storage error", "The server could not read its database.", session)
}

fn not_found(session: &Session, what: &str) -> Response {
    error_page(StatusCode::NOT_FOUND, "Not found", what, Some(session))
}

fn download(content_type: &'static str, filename: &str, body: String) -> Response {
    let mut response = (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], body).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(DOWNLOAD_CSP));
    response
}

/// Unwraps an `Ok` or returns the response carried by the `Err`.
macro_rules! tri {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(response) => return response,
        }
    };
}

// ---------------------------------------------------------------------------------------------------------
// request context

/// A stable, non-secret handle for a write key, used in revoke URLs.
pub fn key_id(key: &str) -> String {
    auth::sha256_hex(key).chars().take(16).collect()
}

fn ingest_url(state: &AppState, headers: &HeaderMap) -> String {
    let host = headers.get(header::HOST).and_then(|value| value.to_str().ok()).filter(|host| {
        !host.is_empty() && host.len() <= 255 && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
    });
    let scheme = if state.config.secure_cookies { "https" } else { "http" };
    let authority = host.map_or_else(|| state.config.bind.to_string(), str::to_string);
    format!("{scheme}://{authority}{}", ashlar_trace_proto::routes::INGEST)
}

/// Everything an app page needs before it loads its own data.
struct Ctx {
    session: Session,
    app: AppRecord,
    filters: Filters,
    params: Params,
    now: i64,
    ingest_url: String,
}

async fn context(state: &AppState, headers: &HeaderMap, slug: &str, query: Option<&str>) -> Result<Ctx, Response> {
    let session = auth::authenticate(state, headers).await?;
    let wanted = slug.to_string();
    let app = match blocking(&state.store, move |store| store.find_app(&wanted)).await {
        Ok(Some(app)) => app,
        Ok(None) => return Err(not_found(&session, "There is no app with that name.")),
        Err(error) => return Err(storage_error(&error, Some(&session))),
    };
    let params = form::parse(query.unwrap_or(""));
    Ok(Ctx { filters: Filters::from_params(&params), params, now: crate::now_ms(), ingest_url: ingest_url(state, headers), session, app })
}

/// Runs `load` on the store and turns a failure into an error page.
async fn load<T, F>(state: &AppState, session: &Session, load: F) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(&dyn Store) -> StoreResult<T> + Send + 'static,
{
    blocking(&state.store, load).await.map_err(|error| storage_error(&error, Some(session)))
}

fn page(ctx: &Ctx, title: &str, active: &str, body: Markup) -> Response {
    let shell = Shell { title, user: &ctx.session.username, csrf: &ctx.session.csrf, app: Some((&ctx.app.slug, &ctx.app.name)), active };
    html_response(StatusCode::OK, view::shell(&shell, body))
}

/// Parses a numeric path segment. A bad one is a plain 404.
fn parse_id(text: &str) -> Option<i64> {
    text.parse::<i64>().ok().filter(|id| *id > 0)
}

// ---------------------------------------------------------------------------------------------------------
// handlers

async fn stylesheet() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], view::STYLESHEET).into_response()
}

async fn index(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let session = tri!(auth::authenticate(&state, &headers).await);
    let today = crate::now_ms().div_euclid(DAY_MS);
    let apps = tri!(
        load(&state, &session, move |store| {
            let mut rows = Vec::new();
            for app in store.list_apps()? {
                let glance = store.app_glance(app.id, today - 6, today)?;
                rows.push((app, glance));
            }
            Ok(rows)
        })
        .await
    );
    let shell = Shell { title: "Apps", user: &session.username, csrf: &session.csrf, app: None, active: "" };
    html_response(StatusCode::OK, view::shell(&shell, pages::app_picker(&apps)))
}

async fn overview(State(state): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, RawQuery(query): RawQuery) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, query.as_deref()).await);
    let (app, filters, now) = (ctx.app.clone(), ctx.filters.clone(), ctx.now);
    let data = tri!(load(&state, &ctx.session, move |store| data::load_overview(store, &app, &filters, now)).await);
    page(&ctx, "Overview", "overview", pages::overview(&ctx.app, &ctx.filters, &data, &ctx.ingest_url))
}

async fn crashes(State(state): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, RawQuery(query): RawQuery) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, query.as_deref()).await);
    let (app, filters, now) = (ctx.app.clone(), ctx.filters.clone(), ctx.now);
    let list = tri!(load(&state, &ctx.session, move |store| data::load_crashes(store, &app, &filters, now)).await);
    page(&ctx, "Crashes", "crashes", pages::crashes(&ctx.app, &ctx.filters, &list, &ctx.ingest_url))
}

async fn issue(State(state): State<AppState>, headers: HeaderMap, Path((slug, id)): Path<(String, String)>) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, None).await);
    let Some(id) = parse_id(&id) else { return not_found(&ctx.session, "There is no such issue.") };
    let app = ctx.app.clone();
    match tri!(load(&state, &ctx.session, move |store| data::load_issue(store, &app, id)).await) {
        Some(detail) => page(&ctx, "Issue", "crashes", issue::issue_page(&ctx.app, &detail, &ctx.session.csrf)),
        None => not_found(&ctx.session, "There is no such issue in this app."),
    }
}

async fn issue_status(State(state): State<AppState>, headers: HeaderMap, Path((slug, id)): Path<(String, String)>, request: Request) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, None).await);
    let form = tri!(read_checked_form(&ctx.session, request).await);
    let Some(id) = parse_id(&id) else { return not_found(&ctx.session, "There is no such issue.") };
    let Some(status) = form.get("status").filter(|status| data::STATUSES.contains(&status.as_str())).cloned() else {
        return error_page(StatusCode::BAD_REQUEST, "Bad request", "The status must be open, resolved or ignored.", Some(&ctx.session));
    };
    let app_id = ctx.app.id;
    let changed = tri!(load(&state, &ctx.session, move |store| store.set_issue_status(app_id, id, &status)).await);
    if changed { redirect(&format!("/apps/{}/crashes/{id}", ctx.app.slug)) } else { not_found(&ctx.session, "There is no such issue in this app.") }
}

async fn performance(State(state): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, RawQuery(query): RawQuery) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, query.as_deref()).await);
    let (app, filters, now) = (ctx.app.clone(), ctx.filters.clone(), ctx.now);
    let data = tri!(load(&state, &ctx.session, move |store| data::load_performance(store, &app, &filters, now)).await);
    page(&ctx, "Performance", "performance", pages::performance(&ctx.app, &ctx.filters, &data, &ctx.ingest_url))
}

async fn usage(State(state): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, RawQuery(query): RawQuery) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, query.as_deref()).await);
    let (app, filters, now) = (ctx.app.clone(), ctx.filters.clone(), ctx.now);
    let data = tri!(load(&state, &ctx.session, move |store| data::load_usage(store, &app, &filters, now)).await);
    page(&ctx, "Usage", "usage", pages::usage(&ctx.app, &ctx.filters, &data, &ctx.ingest_url))
}

async fn reports(State(state): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, RawQuery(query): RawQuery) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, query.as_deref()).await);
    let app = ctx.app.clone();
    let Some(version) = ctx.filters.version.clone() else {
        let versions = tri!(load(&state, &ctx.session, move |store| store.versions(app.id)).await);
        return page(&ctx, "Reports", "reports", pages::reports_index(&ctx.app, &versions, &ctx.ingest_url));
    };
    let (wanted, now) = (version.clone(), ctx.now);
    let Some(report) = tri!(load(&state, &ctx.session, move |store| report::load_report(store, &app, &wanted, now)).await) else {
        return not_found(&ctx.session, "No data has been received for that version.");
    };
    let base = format!("tracer-report-{}-{}", fmt::file_safe(&ctx.app.slug), fmt::file_safe(&version));
    match ctx.params.get("format").map(String::as_str) {
        Some("html") => download("text/html; charset=utf-8", &format!("{base}.html"), report::standalone_html(&report)),
        Some("md") | Some("markdown") => download("text/markdown; charset=utf-8", &format!("{base}.md"), report::markdown(&report)),
        Some("json") => download("application/json", &format!("{base}.json"), report::json(&report)),
        _ => page(&ctx, "Report", "reports", pages::report_page(&ctx.app, &report)),
    }
}

async fn settings(State(state): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, RawQuery(query): RawQuery) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, query.as_deref()).await);
    settings_response(&state, &ctx, None).await
}

async fn settings_response(state: &AppState, ctx: &Ctx, new_key: Option<&str>) -> Response {
    let (app, filters, now) = (ctx.app.clone(), ctx.filters.clone(), ctx.now);
    let data = tri!(load(state, &ctx.session, move |store| data::load_settings(store, &app, &filters, now)).await);
    page(ctx, "Settings", "settings", pages::settings(&ctx.app, &data, &ctx.session.csrf, &ctx.ingest_url, new_key))
}

/// Reads a POST body and checks its CSRF token.
async fn read_checked_form(session: &Session, request: Request) -> Result<Params, Response> {
    let form = form::read_body(request.into_body()).await;
    let Some(form) = form else {
        return Err(error_page(StatusCode::BAD_REQUEST, "Bad request", "The form could not be read.", Some(session)));
    };
    if !session.csrf_ok(&form) {
        return Err(error_page(
            StatusCode::FORBIDDEN,
            "Request refused",
            "The form token is missing or wrong. Reload the page and try again.",
            Some(session),
        ));
    }
    Ok(form)
}

async fn key_create(State(state): State<AppState>, headers: HeaderMap, Path(slug): Path<String>, request: Request) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, None).await);
    let _form = tri!(read_checked_form(&ctx.session, request).await);
    let key = match cli::new_write_key() {
        Ok(key) => key,
        Err(error) => {
            log::error!("could not create a write key: {error}");
            return error_page(StatusCode::INTERNAL_SERVER_ERROR, "No random source", "The server could not generate a key.", Some(&ctx.session));
        }
    };
    let (app_id, now, stored) = (ctx.app.id, crate::now_ms(), key.clone());
    tri!(load(&state, &ctx.session, move |store| store.create_key(app_id, &stored, now)).await);
    settings_response(&state, &ctx, Some(&key)).await
}

async fn key_revoke(State(state): State<AppState>, headers: HeaderMap, Path((slug, id)): Path<(String, String)>, request: Request) -> Response {
    let ctx = tri!(context(&state, &headers, &slug, None).await);
    let _form = tri!(read_checked_form(&ctx.session, request).await);
    let (app_id, now) = (ctx.app.id, crate::now_ms());
    let revoked = tri!(
        load(&state, &ctx.session, move |store| {
            // Only keys of this app can be revoked from this app's page.
            match store.list_keys(app_id)?.into_iter().find(|k| key_id(&k.key) == id) {
                Some(key) => store.revoke_key(&key.key, now),
                None => Ok(false),
            }
        })
        .await
    );
    if revoked {
        redirect(&format!("/apps/{}/settings", ctx.app.slug))
    } else {
        not_found(&ctx.session, "There is no active key with that id in this app.")
    }
}
