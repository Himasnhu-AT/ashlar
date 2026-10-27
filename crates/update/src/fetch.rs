//! Getting the latest release over HTTP: the [`Fetch`] seam (tests pass a closure), the one-shot
//! [`check`] and, with the `https` feature, [`HttpsFetch`].

use crate::{Outcome, Release, UpdateError};

/// An HTTP answer: status and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Something that GETs a URL. `Err` is a network failure (offline, DNS, TLS, timeout); an HTTP
/// error status is an `Ok` [`Response`].
pub trait Fetch: Send {
    fn get(&mut self, url: &str) -> Result<Response, String>;
}

impl<F> Fetch for F
where
    F: FnMut(&str) -> Result<Response, String> + Send,
{
    fn get(&mut self, url: &str) -> Result<Response, String> {
        self(url)
    }
}

/// `true` for a base URL a check may use: `https://`, or plain `http://` to a loopback address
/// (a local server under test).
pub fn base_allowed(base: &str) -> bool {
    if base.strip_prefix("https://").is_some_and(|rest| !rest.is_empty()) {
        return true;
    }
    let Some(rest) = base.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or_default();
    let host = if host.starts_with('[') {
        host.split(']').next().map(|h| format!("{h}]")).unwrap_or_default()
    } else {
        host.split(':').next().unwrap_or_default().to_string()
    };
    matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

/// The platform this build runs on, as the releases API names it: `macos`, `windows-x64`,
/// `linux-x86_64`, `linux-aarch64`, or `None` where no release is published.
pub const PLATFORM: Option<&str> = if cfg!(target_os = "macos") {
    Some("macos")
} else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
    Some("windows-x64")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
    Some("linux-x86_64")
} else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
    Some("linux-aarch64")
} else {
    None
};

/// `{base}/v1/releases/{app}/latest?platform={PLATFORM}`, with the base's trailing slashes dropped,
/// so each platform is offered its own build. Without a known platform the query is left out.
pub fn latest_url(base: &str, app: &str) -> String {
    latest_url_for(base, app, PLATFORM)
}

/// [`latest_url`] for a given platform.
pub fn latest_url_for(base: &str, app: &str, platform: Option<&str>) -> String {
    let base = base.trim_end_matches('/');
    match platform {
        Some(p) => format!("{base}/v1/releases/{app}/latest?platform={p}"),
        None => format!("{base}/v1/releases/{app}/latest"),
    }
}

/// `true` for an app id the releases API accepts: 1–64 of `a-z`, `0-9`, `-`, starting with a letter.
pub fn app_id_ok(app: &str) -> bool {
    (1..=64).contains(&app.len())
        && app.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && app.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// One check, blocking: GETs the latest release of `app` from `base` and compares it with
/// `current`. A 404 means the server knows no release of the app (up to date, `latest: None`).
pub fn check(fetch: &mut dyn Fetch, base: &str, app: &str, current: &str) -> Result<Outcome, UpdateError> {
    if !base_allowed(base) {
        return Err(UpdateError::Config(format!("{base:?} is not an https:// URL")));
    }
    if !app_id_ok(app) {
        return Err(UpdateError::Config(format!("{app:?} is not an app id")));
    }
    let response = fetch.get(&latest_url(base, app)).map_err(UpdateError::Network)?;
    match response.status {
        200 => {
            let release = Release::parse(&response.body)?;
            if !release.app.is_empty() && release.app != app {
                return Err(UpdateError::Body(format!("the answer is about {:?}, not {app:?}", release.app.chars().take(64).collect::<String>())));
            }
            Ok(Outcome::of(current, Some(release)))
        }
        404 => Ok(Outcome::of(current, None)),
        status => Err(UpdateError::Status(status)),
    }
}

#[cfg(all(feature = "https", not(target_arch = "wasm32")))]
pub use https::HttpsFetch;

#[cfg(all(feature = "https", not(target_arch = "wasm32")))]
mod https {
    use std::io::Read;
    use std::sync::Arc;
    use std::time::Duration;

    use super::{Fetch, Response};
    use crate::release::MAX_BODY;

    /// HTTPS GETs with ureq, rustls and the ring provider (no bundled roots) and
    /// the operating system's certificate verifier: the stack ashlar-telemetry uses. No redirects,
    /// 10 s to connect, 20 s in all.
    pub struct HttpsFetch {
        agent: ureq::Agent,
        user_agent: String,
    }

    impl HttpsFetch {
        /// `user_agent`: e.g. `"my-app/1.2.3"`.
        pub fn new(user_agent: &str) -> HttpsFetch {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let agent = ureq::Agent::config_builder()
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .provider(ureq::tls::TlsProvider::Rustls)
                        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                        .unversioned_rustls_crypto_provider(provider)
                        .build(),
                )
                // `check` refuses plain http except to loopback
                .https_only(false)
                .http_status_as_error(false)
                .max_redirects(0)
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_global(Some(Duration::from_secs(20)))
                .build()
                .into();
            HttpsFetch { agent, user_agent: user_agent.to_string() }
        }
    }

    impl Fetch for HttpsFetch {
        fn get(&mut self, url: &str) -> Result<Response, String> {
            let mut response =
                self.agent.get(url).header("user-agent", &self.user_agent).header("accept", "application/json").call().map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            let mut body = Vec::new();
            // one byte over the cap, so an oversized answer is refused rather than cut
            let limit = u64::try_from(MAX_BODY).unwrap_or(u64::MAX).saturating_add(1);
            response.body_mut().as_reader().take(limit).read_to_end(&mut body).map_err(|e| e.to_string())?;
            Ok(Response { status, body })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_urls_are_https_or_loopback() {
        assert!(base_allowed("https://api.example.com"));
        assert!(base_allowed("http://127.0.0.1:8090"));
        assert!(base_allowed("http://localhost:8090/"));
        assert!(base_allowed("http://[::1]:8090"));
        assert!(!base_allowed("http://api.example.com"));
        assert!(!base_allowed("http://127.0.0.1.example.com"));
        assert!(!base_allowed("https://"));
        assert!(!base_allowed("ftp://x"));
    }

    #[test]
    fn latest_url_joins_cleanly() {
        assert_eq!(latest_url_for("https://api.example.com/", "my-app", None), "https://api.example.com/v1/releases/my-app/latest");
        assert_eq!(
            latest_url_for("https://api.example.com", "my-app", Some("windows-x64")),
            "https://api.example.com/v1/releases/my-app/latest?platform=windows-x64"
        );
        assert!(latest_url("https://api.example.com", "my-app").starts_with("https://api.example.com/v1/releases/my-app/latest"));
        #[cfg(target_os = "macos")]
        assert_eq!(PLATFORM, Some("macos"));
    }

    #[test]
    fn app_ids_match_the_api() {
        assert!(app_id_ok("app"));
        assert!(app_id_ok("my-app2"));
        assert!(!app_id_ok("App"));
        assert!(!app_id_ok("2app"));
        assert!(!app_id_ok("a/b"));
        assert!(!app_id_ok(""));
    }
}
