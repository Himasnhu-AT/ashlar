//! One released build as a releases API describes it, and the check that compares it with the
//! running version.

use serde::{Deserialize, Serialize};

use crate::{UpdateError, Version};

/// The largest response body read (release notes are capped well below this by the server).
pub const MAX_BODY: usize = 256 * 1024;
/// Release notes longer than this are cut (at a character boundary).
pub const MAX_NOTES: usize = 64 * 1024;
/// The longest download link accepted.
pub const MAX_URL: usize = 2048;

/// One released build: the body of `GET {base}/v1/releases/{app}/latest`.
///
/// Only `version` is required; every other field has a default, so a server that adds or drops a
/// field never breaks the check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    #[serde(default)]
    pub app: String,
    pub version: String,
    /// Unix milliseconds.
    #[serde(default)]
    pub published_at: i64,
    /// Where the installer is downloaded from.
    #[serde(default)]
    pub url: String,
    /// The installer's SHA-256, lower-case hex.
    #[serde(default)]
    pub sha256: String,
    /// The installer's size in bytes.
    #[serde(default)]
    pub size: i64,
    /// Release notes for users, Markdown.
    #[serde(default)]
    pub notes: String,
    /// A second copy of the same file, when there is one.
    #[serde(default)]
    pub mirror: Option<String>,
}

/// `true` for a link that is safe to hand to the browser: `https://`, a host, no white space or
/// control characters, at most [`MAX_URL`] bytes.
pub fn is_safe_url(url: &str) -> bool {
    url.len() <= MAX_URL
        && url.strip_prefix("https://").is_some_and(|rest| !rest.is_empty() && !rest.starts_with('/'))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn cut(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

impl Release {
    /// Parses a response body. Refuses a body over [`MAX_BODY`], JSON that isn't a release and a
    /// version that isn't semantic; cuts the notes to [`MAX_NOTES`].
    pub fn parse(body: &[u8]) -> Result<Release, UpdateError> {
        if body.len() > MAX_BODY {
            return Err(UpdateError::Body(format!("the answer is over {MAX_BODY} bytes")));
        }
        let mut release: Release = serde_json::from_slice(body).map_err(|e| UpdateError::Body(e.to_string()))?;
        if Version::parse(&release.version).is_none() {
            return Err(UpdateError::Body(format!("{:?} is not a semantic version", release.version.chars().take(80).collect::<String>())));
        }
        release.version = release.version.trim().to_string();
        cut(&mut release.notes, MAX_NOTES);
        Ok(release)
    }

    /// The parsed version (`None` only for a release built by hand with a bad version).
    pub fn parsed_version(&self) -> Option<Version> {
        Version::parse(&self.version)
    }

    /// `true` when this release is newer than `current`. A running version that doesn't parse
    /// (a development build) is never offered an update.
    pub fn is_newer_than(&self, current: &str) -> bool {
        match (self.parsed_version(), Version::parse(current)) {
            (Some(latest), Some(current)) => latest > current,
            _ => false,
        }
    }

    /// Where to download it: the release's link, else its mirror, else `fallback` (each only when
    /// [`is_safe_url`]). `None` when none of the three is safe.
    pub fn download_url(&self, fallback: &str) -> Option<String> {
        [Some(self.url.as_str()), self.mirror.as_deref(), Some(fallback)].into_iter().flatten().find(|url| is_safe_url(url)).map(str::to_string)
    }
}

/// What a check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A newer release than the running one.
    Available(Release),
    /// Nothing newer. `latest` is the newest release the server knows (`None`: it knows none).
    UpToDate { latest: Option<Release> },
}

impl Outcome {
    /// Compares the server's latest release (if any) with the running `current` version.
    pub fn of(current: &str, latest: Option<Release>) -> Outcome {
        match latest {
            Some(release) if release.is_newer_than(current) => Outcome::Available(release),
            latest => Outcome::UpToDate { latest },
        }
    }

    /// The newest release the server knows, newer or not.
    pub fn latest(&self) -> Option<&Release> {
        match self {
            Outcome::Available(release) => Some(release),
            Outcome::UpToDate { latest } => latest.as_ref(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(version: &str) -> Release {
        Release {
            app: "app".into(),
            version: version.into(),
            published_at: 0,
            url: "https://example.com/app.dmg".into(),
            sha256: String::new(),
            size: 0,
            notes: String::new(),
            mirror: None,
        }
    }

    #[test]
    fn safe_urls_are_https_with_a_host() {
        assert!(is_safe_url("https://example.com/a.dmg"));
        for bad in ["http://example.com/a.dmg", "https://", "https:///a", "file:///a", "javascript:alert(1)", "https://a b", "https://a\n", ""] {
            assert!(!is_safe_url(bad), "{bad:?}");
        }
        assert!(!is_safe_url(&format!("https://example.com/{}", "a".repeat(MAX_URL))));
    }

    #[test]
    fn download_falls_back_to_the_mirror_then_the_fallback() {
        let mut r = release("1.0.0");
        assert_eq!(r.download_url("https://example.com/download").as_deref(), Some("https://example.com/app.dmg"));
        r.url = "ftp://example.com/app.dmg".into();
        r.mirror = Some("https://mirror.example.com/app.dmg".into());
        assert_eq!(r.download_url("https://example.com/download").as_deref(), Some("https://mirror.example.com/app.dmg"));
        r.mirror = None;
        assert_eq!(r.download_url("https://example.com/download").as_deref(), Some("https://example.com/download"));
        assert_eq!(r.download_url("not a url"), None);
    }

    #[test]
    fn newer_only_when_both_versions_parse() {
        assert!(release("1.2.0").is_newer_than("1.1.9"));
        assert!(!release("1.2.0").is_newer_than("1.2.0"));
        assert!(!release("1.2.0").is_newer_than("1.3.0"));
        assert!(release("1.2.0").is_newer_than("1.2.0-rc.1"));
        assert!(!release("1.2.0").is_newer_than("dev"));
        assert!(!release("bad").is_newer_than("1.0.0"));
    }

    #[test]
    fn outcome_compares_with_the_running_version() {
        assert_eq!(Outcome::of("1.0.0", Some(release("1.1.0"))), Outcome::Available(release("1.1.0")));
        assert_eq!(Outcome::of("1.1.0", Some(release("1.1.0"))), Outcome::UpToDate { latest: Some(release("1.1.0")) });
        assert_eq!(Outcome::of("1.1.0", None), Outcome::UpToDate { latest: None });
    }

    #[test]
    fn parse_cuts_long_notes_at_a_character_boundary() {
        let notes = "é".repeat(MAX_NOTES);
        let body = serde_json::json!({"version": "1.0.0", "notes": notes}).to_string();
        let r = Release::parse(body.as_bytes()).unwrap();
        assert!(r.notes.len() <= MAX_NOTES);
        assert!(r.notes.chars().all(|c| c == 'é'));
    }

    #[test]
    fn parse_refuses_oversized_bodies() {
        assert!(matches!(Release::parse(&vec![b' '; MAX_BODY + 1]), Err(UpdateError::Body(_))));
    }
}
