//! System notifications ("Export finished").
//!
//! A notification has an id. Showing another one with the same id replaces it, [`remove`] takes
//! it down, and [`on_click`] reports which one the user clicked, with its `info` map. Without an
//! id, [`show`] makes a fresh one and returns it.
//!
//! - macOS: UNUserNotificationCenter. It works only for an app bundle with a bundle identifier
//!   (an unbundled `cargo run` binary would be terminated by the OS), so outside a bundle this
//!   returns `Unsupported`. The first notification asks the user for permission (sound is asked
//!   for only when a notification plays one); delivery is asynchronous and a refusal is logged,
//!   not returned. A notification-center delegate is installed once, so banners also show while
//!   the app is frontmost and clicks reach [`on_click`]. Call [`on_click`] early (before the
//!   event loop starts) to also receive the click that launched the app.
//! - Windows: toasts. An unpackaged app needs an AppUserModelID registered under
//!   `HKCU\Software\Classes\AppUserModelId`; this is done on first use with the id from
//!   [`set_app_identity`] (default: the executable's name), which also becomes the process's
//!   explicit AppUserModelID. Clicks are reported while the app runs.
//! - Linux: the `org.freedesktop.Notifications` D-Bus service, with the `desktop-entry` hint
//!   from [`set_app_identity`]. A click on the notification body is reported.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, PoisonError};

use crate::{NativeError, Result, Waker};

#[cfg(target_os = "linux")]
#[path = "notify/linux.rs"]
mod imp;
#[cfg(target_os = "macos")]
#[path = "notify/macos.rs"]
mod imp;
#[cfg(target_os = "windows")]
#[path = "notify/windows.rs"]
mod imp;

/// The most `info` entries a notification carries; the rest are dropped.
pub const MAX_INFO: usize = 32;
const MAX_ID: usize = 256;
const MAX_KEY: usize = 64;
const MAX_VALUE: usize = 1024;
/// Windows rejects toast tags longer than this (UTF-16 units).
const MAX_TAG: usize = 64;

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Notification {
    /// A stable id: showing another notification with it replaces this one. `None` makes a new
    /// id each time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// No sound.
    #[serde(default)]
    pub silent: bool,
    /// Data handed back in the [`Click`] (for example a document path or URL).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub info: BTreeMap<String, String>,
}

/// The user clicked a notification.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Click {
    pub id: String,
    #[serde(default)]
    pub info: BTreeMap<String, String>,
}

/// A notification after cleaning: bounded text, no control characters, an id.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Prepared {
    id: String,
    title: String,
    body: String,
    silent: bool,
    info: BTreeMap<String, String>,
}

/// Keep at most `n` characters, dropping control characters (newlines too unless `lines`).
fn clip(s: &str, n: usize, lines: bool) -> String {
    s.chars().filter(|c| (lines && *c == '\n') || !c.is_control()).take(n).collect()
}

fn clean_id(s: &str) -> Option<String> {
    let id = clip(s, MAX_ID, false).trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// A new id, unique within this process and unlikely to repeat across runs.
fn fresh_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("ashlar-{t:x}-{n}")
}

impl Notification {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self { title: title.into(), body: body.into(), ..Self::default() }
    }

    /// Set a stable id.
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Show without a sound.
    pub fn silent(mut self) -> Self {
        self.silent = true;
        self
    }

    /// Add an `info` entry.
    pub fn with_info(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.info.insert(key.into(), value.into());
        self
    }

    fn clean(&self) -> Result<Prepared> {
        let title = clip(&self.title, 256, true);
        if title.trim().is_empty() {
            return Err(NativeError::Invalid("a notification needs a title".into()));
        }
        let id = self.id.as_deref().and_then(clean_id).unwrap_or_else(fresh_id);
        let info = self
            .info
            .iter()
            .filter_map(|(k, v)| {
                let k = clip(k, MAX_KEY, false);
                (!k.is_empty()).then(|| (k, clip(v, MAX_VALUE, true)))
            })
            .take(MAX_INFO)
            .collect();
        Ok(Prepared { id, title, body: clip(&self.body, 2048, true), silent: self.silent, info })
    }
}

/// Show a notification.
pub fn notify(n: &Notification) -> Result<()> {
    show(n).map(|_| ())
}

/// Show a notification and return its id (the one it was given, or a new one).
pub fn show(n: &Notification) -> Result<String> {
    let p = n.clean()?;
    crate::guarded("the notification", || imp::show(&p))?;
    Ok(p.id)
}

/// Take down notifications by id, shown or still pending. Unknown ids are ignored.
pub fn remove<S: AsRef<str>>(ids: &[S]) -> Result<()> {
    let ids: Vec<String> = ids.iter().filter_map(|s| clean_id(s.as_ref())).take(1024).collect();
    if ids.is_empty() {
        return Ok(());
    }
    crate::guarded("removing notifications", || imp::remove(&ids))
}

/// Receive clicks on this app's notifications; `waker` runs after each one is queued. A later
/// call replaces the earlier receiver. Where clicks can't be reported, the receiver stays empty.
pub fn on_click(waker: Waker) -> Receiver<Click> {
    let (tx, rx) = channel();
    *SINK.lock().unwrap_or_else(PoisonError::into_inner) = Some((tx, waker));
    if let Err(e) = crate::guarded("listening for notification clicks", imp::listen) {
        log::info!("notifications: clicks are not reported here ({e})");
    }
    rx
}

static SINK: Mutex<Option<(Sender<Click>, Waker)>> = Mutex::new(None);

/// Hand a click to the [`on_click`] receiver, if any.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows", target_os = "linux")), allow(dead_code))]
fn deliver(click: Click) {
    let sink = SINK.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some((tx, waker)) = sink
        && tx.send(click).is_ok()
    {
        waker();
    }
}

static APP: Mutex<Option<(String, String)>> = Mutex::new(None);

/// Name this app for the notification service: `id` is the Windows AppUserModelID and the Linux
/// desktop entry (the `.desktop` file name without its suffix), `name` is shown next to toasts on
/// Windows. Call it before the first notification. Default: the executable's name for both.
pub fn set_app_identity(id: &str, name: &str) {
    let id = sanitize_app_id(id);
    let name = clip(name, 128, false).trim().to_string();
    if id.is_empty() {
        return;
    }
    let name = if name.is_empty() { id.clone() } else { name };
    *APP.lock().unwrap_or_else(PoisonError::into_inner) = Some((id, name));
}

fn sanitize_app_id(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).take(128).collect()
}

/// `(id, display name)` for the notification service.
#[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
fn app_identity() -> (String, String) {
    if let Some(app) = APP.lock().unwrap_or_else(PoisonError::into_inner).clone() {
        return app;
    }
    let stem = std::env::current_exe().ok().and_then(|p| p.file_stem().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
    let id = sanitize_app_id(&stem);
    let id = if id.is_empty() { "ashlar.app".to_string() } else { id };
    let name = if stem.trim().is_empty() { id.clone() } else { clip(&stem, 128, false) };
    (id, name)
}

fn pct(s: &str, out: &mut String) {
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
}

fn unpct(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The id and info as one URL-like string, `id?key=value&...`, all percent-encoded. It travels
/// with the notification (macOS user info, Windows toast `launch`) and comes back on a click.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn encode_launch(id: &str, info: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    pct(id, &mut out);
    for (i, (k, v)) in info.iter().enumerate() {
        out.push(if i == 0 { '?' } else { '&' });
        pct(k, &mut out);
        out.push('=');
        pct(v, &mut out);
    }
    out
}

/// The inverse of [`encode_launch`]; `None` for text it did not make.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn decode_launch(s: &str) -> Option<Click> {
    let (id, query) = s.split_once('?').unwrap_or((s, ""));
    let id = clean_id(&unpct(id)?)?;
    let mut info = BTreeMap::new();
    for pair in query.split('&').filter(|p| !p.is_empty()).take(MAX_INFO) {
        let (k, v) = pair.split_once('=')?;
        info.insert(clip(&unpct(k)?, MAX_KEY, false), clip(&unpct(v)?, MAX_VALUE, true));
    }
    Some(Click { id, info })
}

/// FNV-1a: a hash that stays the same across runs and Rust versions.
fn fnv1a(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// The Windows toast tag for an id: the id itself when short enough, otherwise a prefix of it
/// plus a hash of the whole id, so different long ids get different tags.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn toast_tag(id: &str) -> String {
    if id.encode_utf16().count() <= MAX_TAG {
        return id.to_string();
    }
    let mut prefix = String::new();
    let mut units = 0;
    for c in id.chars() {
        units += c.len_utf16();
        if units > MAX_TAG - 17 {
            break;
        }
        prefix.push(c);
    }
    format!("{prefix}~{:016x}", fnv1a(id))
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// The toast's XML. All text is escaped; `launch` carries the id and info.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn toast_xml(p: &Prepared) -> String {
    let body = if p.body.is_empty() { String::new() } else { format!("<text>{}</text>", xml_escape(&p.body)) };
    let audio = if p.silent { r#"<audio silent="true"/>"# } else { "" };
    format!(
        r#"<toast launch="{}"><visual><binding template="ToastGeneric"><text>{}</text>{body}</binding></visual>{audio}</toast>"#,
        xml_escape(&encode_launch(&p.id, &p.info)),
        xml_escape(&p.title),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod imp {
    use super::*;

    pub fn show(_: &Prepared) -> Result<()> {
        Err(NativeError::Unsupported)
    }

    pub fn remove(_: &[String]) -> Result<()> {
        Err(NativeError::Unsupported)
    }

    pub fn listen() -> Result<()> {
        Err(NativeError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_required_and_text_is_bounded() {
        assert!(matches!(notify(&Notification::new("  ", "body")), Err(NativeError::Invalid(_))));
        let p = Notification::new("x".repeat(1000), "line\none\u{7}").clean().unwrap();
        assert_eq!(p.title.len(), 256);
        assert_eq!(p.body, "line\none");
    }

    #[test]
    fn ids_are_kept_cleaned_or_made_fresh() {
        let n = Notification::new("T", "").with_id(" export\n-1 ");
        assert_eq!(n.clean().unwrap().id, "export-1");
        assert_eq!(n.clean().unwrap().id, n.clean().unwrap().id);
        let fresh = Notification::new("T", "");
        assert_ne!(fresh.clean().unwrap().id, fresh.clean().unwrap().id);
        assert_ne!(Notification::new("T", "").with_id("  ").clean().unwrap().id, "");
    }

    #[test]
    fn info_is_bounded() {
        let mut n = Notification::new("T", "").with_info("", "dropped").with_info("k\u{1}", "v\u{7}");
        for i in 0..100 {
            n = n.with_info(format!("key{i:03}"), "v");
        }
        let p = n.clean().unwrap();
        assert_eq!(p.info.len(), MAX_INFO);
        assert_eq!(p.info.get("k").map(String::as_str), Some("v"));
    }

    #[test]
    fn xml_is_escaped() {
        assert_eq!(xml_escape(r#"<a href="x">Tom & 'Jerry'</a>"#), "&lt;a href=&quot;x&quot;&gt;Tom &amp; &apos;Jerry&apos;&lt;/a&gt;");
        let p = Notification::new("</text><evil/>", "a&b").with_id("i\"d").silent().clean().unwrap();
        let xml = toast_xml(&p);
        assert!(!xml.contains("<evil/>"));
        assert!(xml.contains("<text>&lt;/text&gt;&lt;evil/&gt;</text><text>a&amp;b</text>"));
        assert!(xml.contains(r#"launch="i%22d""#));
        assert!(xml.ends_with(r#"<audio silent="true"/></toast>"#));
        let loud = toast_xml(&Notification::new("T", "").clean().unwrap());
        assert!(!loud.contains("<audio") && !loud.contains("<text></text>"));
    }

    #[test]
    fn long_tags_are_hashed_stably() {
        assert_eq!(toast_tag("short"), "short");
        assert_eq!(toast_tag(&"a".repeat(64)), "a".repeat(64));
        let long = format!("{}-1", "doc/".repeat(30));
        let other = format!("{}-2", "doc/".repeat(30));
        let tag = toast_tag(&long);
        assert!(tag.encode_utf16().count() <= MAX_TAG);
        assert_eq!(tag, toast_tag(&long));
        assert_ne!(tag, toast_tag(&other));
        assert_eq!(fnv1a(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a("a"), 0xaf63_dc4c_8601_ec8c);
        // Multi-unit characters never push the tag over the limit.
        assert!(toast_tag(&"😀".repeat(50)).encode_utf16().count() <= MAX_TAG);
    }

    #[test]
    fn info_round_trips_through_the_launch_string() {
        let info: BTreeMap<String, String> = [("url", "https://example.com/a?b=c&d=é#f"), ("path", "/tmp/a b/%20.txt"), ("=&?", "")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        let s = encode_launch("doc?1&x", &info);
        assert!(!s[s.find('?').unwrap() + 1..].contains('?'));
        assert_eq!(decode_launch(&s), Some(Click { id: "doc?1&x".into(), info }));
        assert_eq!(decode_launch("plain"), Some(Click { id: "plain".into(), info: BTreeMap::new() }));
        assert_eq!(decode_launch("id?k=%zz"), None);
        assert_eq!(decode_launch("id?k=%C3"), None);
        assert_eq!(decode_launch("id?novalue"), None);
    }

    #[test]
    fn clicks_reach_the_latest_receiver() {
        let woke = std::sync::Arc::new(AtomicU64::new(0));
        let w = woke.clone();
        let rx = on_click(std::sync::Arc::new(move || {
            w.fetch_add(1, Ordering::Relaxed);
        }));
        deliver(Click { id: "a".into(), info: BTreeMap::new() });
        assert_eq!(rx.try_recv().map(|c| c.id), Ok("a".into()));
        assert_eq!(woke.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn app_ids_are_sanitized() {
        assert_eq!(sanitize_app_id("Com.Example App/1"), "Com.ExampleApp1");
        assert!(!app_identity().0.is_empty());
    }

    #[test]
    fn notifications_serialize_compactly_and_old_json_still_reads() {
        let old: Notification = serde_json::from_str(r#"{"title":"T","body":"B"}"#).unwrap();
        assert_eq!(old, Notification::new("T", "B"));
        assert_eq!(serde_json::to_string(&old).unwrap(), r#"{"title":"T","body":"B","silent":false}"#);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_unbundled_binary_gets_unsupported_instead_of_being_killed() {
        // `cargo test` binaries are not app bundles.
        assert_eq!(notify(&Notification::new("Test", "")), Err(NativeError::Unsupported));
        assert_eq!(remove(&["x"]), Err(NativeError::Unsupported));
    }
}
