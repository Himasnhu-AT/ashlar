//! Dashboard login: argon2 passwords, random session tokens, CSRF tokens.
//!
//! - The cookie holds a random 256-bit token; only its SHA-256 is stored in `admin_sessions`.
//! - The CSRF token of a session is derived from the token (`SHA-256("tracer-csrf:" + token)`), so it is tied
//!   to the session and needs no storage. Every POST must carry it. The login form uses the same idea with
//!   a short-lived cookie, so a page on another site cannot log a browser in.
//! - Tokens are compared in constant time. Failed logins take the same time and show the same message
//!   whether or not the user exists, and attempts are rate limited per client address.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use maud::{Markup, html};
use sha2::{Digest, Sha256};

use super::{error_page, form, html_response, redirect};
use crate::cli;
use crate::http::{AppState, blocking, client_ip};
use crate::ingest::DAY_MS;

/// Cookie names. Over HTTPS (the default) the session cookie carries the `__Host-` prefix, which makes the
/// browser refuse it unless it is `Secure`, has `Path=/` and names no `Domain`; the short-lived login-form
/// cookie lives under `/login`, so it takes `__Secure-` instead.
pub fn session_cookie_name(secure: bool) -> &'static str {
    if secure { "__Host-tracer_session" } else { "tracer_session" }
}

fn login_cookie_name(secure: bool) -> &'static str {
    if secure { "__Secure-tracer_login" } else { "tracer_login" }
}
/// How long a login lasts.
pub const SESSION_MS: i64 = 14 * DAY_MS;
const LOGIN_FORM_SECONDS: u32 = 15 * 60;
/// Every failed login waits at least this long before the reply.
const FAILURE_DELAY: Duration = Duration::from_millis(300);
const MAX_USERNAME: usize = 128;
const MAX_PASSWORD: usize = 1024;
const BAD_LOGIN: &str = "Invalid username or password.";

/// A logged-in request.
#[derive(Debug, Clone)]
pub struct Session {
    pub admin_id: i64,
    pub username: String,
    pub token_hash: String,
    pub csrf: String,
}

impl Session {
    /// True when the form carries this session's CSRF token.
    pub fn csrf_ok(&self, form: &form::Params) -> bool {
        form.get("csrf").is_some_and(|given| ct_eq(given.as_bytes(), self.csrf.as_bytes()))
    }
}

pub fn sha256_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `bytes` random bytes as lower-case hex.
pub fn random_hex(bytes: usize) -> Option<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Equality without early exit on the first differing byte. The length is not secret.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn csrf_for(token: &str) -> String {
    sha256_hex(&format!("tracer-csrf:{token}"))
}

fn login_csrf_for(cookie: &str) -> String {
    sha256_hex(&format!("tracer-login:{cookie}"))
}

fn token_shape_ok(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The value of cookie `name`, if sent.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|line| line.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_string())
}

fn set_cookie(response: &mut Response, name: &str, value: &str, max_age: u32, path: &str, same_site: &str, secure: bool) {
    let secure = if secure { "; Secure" } else { "" };
    let text = format!("{name}={value}; Path={path}; Max-Age={max_age}; HttpOnly; SameSite={same_site}{secure}");
    if let Ok(value) = HeaderValue::from_str(&text) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn clear_session_cookie(response: &mut Response, secure: bool) {
    set_cookie(response, session_cookie_name(secure), "", 0, "/", "Lax", secure);
}

/// The logged-in user for this request, or the response to send instead: a redirect to the login page.
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Session, Response> {
    let secure = state.config.secure_cookies;
    let Some(token) = cookie_value(headers, session_cookie_name(secure)) else { return Err(redirect("/login")) };
    if !token_shape_ok(&token) {
        let mut response = redirect("/login");
        clear_session_cookie(&mut response, secure);
        return Err(response);
    }
    let hash = sha256_hex(&token);
    let lookup = hash.clone();
    match blocking(&state.store, move |store| store.admin_session_lookup(&lookup, crate::now_ms())).await {
        Ok(Some(admin)) => Ok(Session { admin_id: admin.admin_id, username: admin.username, token_hash: hash, csrf: csrf_for(&token) }),
        Ok(None) => {
            let mut response = redirect("/login");
            clear_session_cookie(&mut response, secure);
            Err(response)
        }
        Err(error) => {
            log::error!("session lookup failed: {error}");
            Err(error_page(StatusCode::INTERNAL_SERVER_ERROR, "Storage error", "The server could not read its database.", None))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------
// login

fn login_markup(message: Option<&str>, csrf: Option<&str>, no_admin: bool) -> Markup {
    html! {
        h1 { "Tracer" }
        p class="sub" { "Sign in to the dashboard." }
        @if let Some(message) = message {
            div class="banner error" role="alert" { (message) }
        }
        @if no_admin {
            div class="banner" role="status" {
                strong { "No admin account exists yet." }
                p { "Create one on the server, then reload this page:" }
                p { code { "ashlar-trace-server admin create <username>" } }
            }
        } @else if let Some(csrf) = csrf {
            form method="post" action="/login" {
                input type="hidden" name="csrf" value=(csrf);
                label { "Username" input type="text" name="username" autocomplete="username" autocapitalize="none" spellcheck="false" required maxlength="128"; }
                label { "Password" input type="password" name="password" autocomplete="current-password" required maxlength="1024"; }
                button type="submit" class="primary" { "Sign in" }
            }
        }
    }
}

/// The login page with a fresh form token. `status` is 200 for a plain visit, or an error status.
fn login_page(state: &AppState, status: StatusCode, message: Option<&str>, no_admin: bool) -> Response {
    let cookie = random_hex(16);
    let csrf = cookie.as_deref().map(login_csrf_for);
    let markup = super::view::plain("Sign in", login_markup(message, csrf.as_deref(), no_admin));
    let mut response = html_response(status, markup);
    if let Some(cookie) = cookie {
        set_cookie(
            &mut response,
            login_cookie_name(state.config.secure_cookies),
            &cookie,
            LOGIN_FORM_SECONDS,
            "/login",
            "Strict",
            state.config.secure_cookies,
        );
    }
    response
}

pub async fn login_get(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if authenticate(&state, &headers).await.is_ok() {
        return redirect("/");
    }
    match blocking(&state.store, |store| store.admin_count()).await {
        Ok(count) => login_page(&state, StatusCode::OK, None, count == 0),
        Err(error) => {
            log::error!("admin count failed: {error}");
            error_page(StatusCode::INTERNAL_SERVER_ERROR, "Storage error", "The server could not read its database.", None)
        }
    }
}

/// A valid hash to check unknown users against, so a missing user costs as much as a wrong password.
fn dummy_hash() -> Option<&'static str> {
    static HASH: OnceLock<Option<String>> = OnceLock::new();
    HASH.get_or_init(|| cli::hash_password("tracer-timing-equaliser").ok()).as_deref()
}

pub async fn login_post(State(state): State<AppState>, request: Request) -> Response {
    let started = Instant::now();
    let (parts, body) = request.into_parts();
    let peer = parts.extensions.get::<ConnectInfo<std::net::SocketAddr>>().map(|info| info.0);
    let addr = client_ip(&parts.headers, peer, state.config.trust_proxy);
    if let Err(wait) = state.limiters.check_login(addr, started) {
        let mut response = login_page(&state, StatusCode::TOO_MANY_REQUESTS, Some("Too many sign-in attempts. Wait a moment and try again."), false);
        if let Ok(value) = HeaderValue::from_str(&wait.to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        return response;
    }
    let Some(form) = form::read_body(body).await else {
        return login_page(&state, StatusCode::BAD_REQUEST, Some("The form could not be read."), false);
    };
    let cookie_ok = cookie_value(&parts.headers, login_cookie_name(state.config.secure_cookies))
        .zip(form.get("csrf"))
        .is_some_and(|(cookie, given)| ct_eq(login_csrf_for(&cookie).as_bytes(), given.as_bytes()));
    if !cookie_ok {
        return login_page(&state, StatusCode::FORBIDDEN, Some("The sign-in form expired. Try again."), false);
    }
    let username = form.get("username").cloned().unwrap_or_default();
    let password = form.get("password").cloned().unwrap_or_default();
    // Recent failures for this user name, and across all of them, make every attempt wait, right or wrong:
    // the delay depends only on the text of the user name, so it reveals nothing about the account.
    let tracked: String = username.chars().take(MAX_USERNAME).collect();
    let extra = state.limiters.login_delay(&tracked, Instant::now());
    if !extra.is_zero() {
        tokio::time::sleep(extra).await;
    }
    let within_limits = !username.is_empty() && username.len() <= MAX_USERNAME && password.len() <= MAX_PASSWORD;

    let lookup_name = username.clone();
    let found = match blocking(&state.store, move |store| store.admin_password_hash(&lookup_name)).await {
        Ok(found) => found,
        Err(error) => {
            log::error!("login lookup failed: {error}");
            return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Storage error", "The server could not read its database.", None);
        }
    };
    let admin_id = found.as_ref().map(|(id, _)| *id);
    let hash = found.map(|(_, hash)| hash);
    // Only a few password checks run at once; the rest queue. Each costs memory and CPU.
    let slot = state.login_slots.acquire().await;
    let verified = tokio::task::spawn_blocking(move || match (&hash, within_limits) {
        (Some(hash), true) => cli::verify_password(&password, hash),
        (None, true) => {
            if let Some(dummy) = dummy_hash() {
                let _ = cli::verify_password(&password, dummy);
            }
            false
        }
        _ => false,
    })
    .await
    .unwrap_or(false);
    drop(slot);

    let Some(admin_id) = admin_id.filter(|_| verified) else {
        state.limiters.login_failed(&tracked, Instant::now());
        tokio::time::sleep(FAILURE_DELAY.saturating_sub(started.elapsed())).await;
        return login_page(&state, StatusCode::UNAUTHORIZED, Some(BAD_LOGIN), false);
    };
    state.limiters.login_succeeded(&tracked);
    let Some(token) = random_hex(32) else {
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, "No random source", "The server could not create a session token.", None);
    };
    let hash = sha256_hex(&token);
    let now = crate::now_ms();
    let created = blocking(&state.store, move |store| {
        // Expired sessions are dropped whenever someone signs in.
        let _ = store.admin_session_purge(now);
        store.admin_session_create(&hash, admin_id, now, now + SESSION_MS)
    })
    .await;
    if let Err(error) = created {
        log::error!("could not create a session: {error}");
        return error_page(StatusCode::INTERNAL_SERVER_ERROR, "Storage error", "The server could not save the session.", None);
    }
    let mut response = redirect("/");
    let secure = state.config.secure_cookies;
    set_cookie(&mut response, session_cookie_name(secure), &token, (SESSION_MS / 1000) as u32, "/", "Lax", secure);
    set_cookie(&mut response, login_cookie_name(secure), "", 0, "/login", "Strict", secure);
    response
}

pub async fn logout_post(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let session = match authenticate(&state, &parts.headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let Some(form) = form::read_body(body).await else {
        return error_page(StatusCode::BAD_REQUEST, "Bad request", "The form could not be read.", Some(&session));
    };
    if !session.csrf_ok(&form) {
        return error_page(
            StatusCode::FORBIDDEN,
            "Request refused",
            "The form token is missing or wrong. Reload the page and try again.",
            Some(&session),
        );
    }
    let hash = session.token_hash.clone();
    if let Err(error) = blocking(&state.store, move |store| store.admin_session_delete(&hash)).await {
        log::error!("could not delete a session: {error}");
    }
    let mut response = redirect("/login");
    clear_session_cookie(&mut response, state.config.secure_cookies);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_compare_matches_normal_equality() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn tokens_are_random_hex_and_hashes_are_stable() {
        let a = random_hex(32).unwrap();
        let b = random_hex(32).unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(token_shape_ok(&a));
        assert!(!token_shape_ok("short"));
        assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_ne!(csrf_for(&a), sha256_hex(&a));
    }

    #[test]
    fn cookies_are_found_among_several() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("a=1; tracer_session=abc; b=2"));
        assert_eq!(cookie_value(&headers, "tracer_session").as_deref(), Some("abc"));
        assert_eq!(cookie_value(&headers, "missing"), None);
        assert_eq!(cookie_value(&headers, "tracer"), None);
    }

    #[test]
    fn cookie_attributes() {
        let mut response = Response::new(axum::body::Body::empty());
        set_cookie(&mut response, "n", "v", 60, "/", "Lax", true);
        set_cookie(&mut response, "m", "v", 60, "/", "Lax", false);
        let cookies: Vec<&str> = response.headers().get_all(header::SET_COOKIE).iter().filter_map(|v| v.to_str().ok()).collect();
        assert_eq!(cookies[0], "n=v; Path=/; Max-Age=60; HttpOnly; SameSite=Lax; Secure");
        assert_eq!(cookies[1], "m=v; Path=/; Max-Age=60; HttpOnly; SameSite=Lax");
    }
}
