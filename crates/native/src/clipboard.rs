//! The system clipboard: text, RGBA images, rich content (text with HTML, a PNG, file lists),
//! secrets that clear themselves, and, on macOS, custom pasteboard types so an app can copy its
//! own data alongside a plain-text fallback.
//!
//! On Linux (X11 and Wayland) the copying process serves the clipboard: one long-lived arboard
//! clipboard lives on a worker thread for the life of the process, so copied content stays
//! pasteable after the call returns. Elsewhere each call opens the clipboard briefly.
//!
//! Content read with [`rich`] is bounded ([`MAX_TEXT_BYTES`], [`MAX_HTML_BYTES`],
//! [`MAX_PNG_BYTES`], [`MAX_FILES`]) and its HTML goes through [`sanitize_html`].

use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use crate::{NativeError, Result};

/// Largest image accepted, in pixels per side.
pub const MAX_IMAGE_SIDE: u32 = 16_384;
/// Largest custom payload, in bytes.
pub const MAX_CUSTOM_BYTES: usize = 256 * 1024 * 1024;
/// Largest text copied or read, in bytes.
pub const MAX_TEXT_BYTES: usize = 64 * 1024 * 1024;
/// Largest HTML copied or read, in bytes.
pub const MAX_HTML_BYTES: usize = 16 * 1024 * 1024;
/// Largest PNG copied or read, in bytes.
pub const MAX_PNG_BYTES: usize = 64 * 1024 * 1024;
/// Most files in one copy.
pub const MAX_FILES: usize = 4096;
/// Longest a secret may stay before it is cleared.
pub const MAX_SECRET_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// An image as RGBA8, row-major, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Image {
    /// Check the size against the pixel data; refuse empty and oversized images.
    pub fn validate(&self) -> Result<()> {
        let expected = (self.width as usize).checked_mul(self.height as usize).and_then(|n| n.checked_mul(4));
        if self.width == 0 || self.height == 0 || self.width > MAX_IMAGE_SIDE || self.height > MAX_IMAGE_SIDE {
            return Err(NativeError::Invalid(format!("an image of {}×{} pixels can't be copied", self.width, self.height)));
        }
        if expected != Some(self.rgba.len()) {
            return Err(NativeError::Invalid("the pixel data does not match the image size".into()));
        }
        Ok(())
    }
}

/// One piece of content in several forms. Copying writes as many forms as the platform takes in
/// one go: macOS writes text, HTML and PNG together (files alone); elsewhere arboard writes one
/// form per copy, chosen in the order files, HTML (with the text as its plain fallback), PNG,
/// text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rich {
    pub text: String,
    pub html: Option<String>,
    /// PNG-encoded image bytes.
    pub png: Option<Vec<u8>>,
    /// Absolute paths.
    pub files: Vec<PathBuf>,
}

impl Rich {
    /// Plain text only.
    pub fn text(text: impl Into<String>) -> Self {
        Rich { text: text.into(), ..Default::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.html.is_none() && self.png.is_none() && self.files.is_empty()
    }

    /// Refuse empty and oversized content before the OS sees it.
    pub fn validate(&self) -> Result<()> {
        if self.is_empty() {
            return Err(NativeError::Invalid("there is nothing to copy".into()));
        }
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(NativeError::Invalid(format!("the text is larger than {MAX_TEXT_BYTES} bytes")));
        }
        if self.html.as_ref().is_some_and(|h| h.len() > MAX_HTML_BYTES) {
            return Err(NativeError::Invalid(format!("the HTML is larger than {MAX_HTML_BYTES} bytes")));
        }
        if let Some(png) = &self.png {
            if png.len() > MAX_PNG_BYTES {
                return Err(NativeError::Invalid(format!("the image is larger than {MAX_PNG_BYTES} bytes")));
            }
            if !png.starts_with(PNG_SIGNATURE) {
                return Err(NativeError::Invalid("the image is not a PNG".into()));
            }
        }
        if self.files.len() > MAX_FILES {
            return Err(NativeError::Invalid(format!("copy at most {MAX_FILES} files")));
        }
        if self.files.iter().any(|f| !f.is_absolute()) {
            return Err(NativeError::Invalid("file paths must be absolute".into()));
        }
        Ok(())
    }

    /// Bound and clean content read from the clipboard: oversized forms are dropped, HTML is
    /// sanitized.
    fn inbound(mut self) -> Self {
        if self.text.len() > MAX_TEXT_BYTES {
            self.text.clear();
        }
        self.html = self.html.filter(|h| h.len() <= MAX_HTML_BYTES).map(|h| sanitize_html(&h)).filter(|h| !h.trim().is_empty());
        self.png = self.png.filter(|p| p.len() <= MAX_PNG_BYTES && p.starts_with(PNG_SIGNATURE));
        self.files.retain(|f| f.is_absolute());
        self.files.truncate(MAX_FILES);
        self
    }
}

/// A pasteboard type name for custom data: a uniform type identifier on macOS
/// (`com.example.app.clip`) or a MIME type (`application/x-example-clip`). Bounded and printable.
pub fn valid_type(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128 && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-+/".contains(c))
}

// ------------------------------------------------------------------ backends

/// Where clipboard content lives: [`System`], or [`MemoryClipboard`] for tests.
pub trait Backend {
    /// Replace the content. `secret`: ask clipboard history and monitoring tools to skip it.
    fn set_rich(&mut self, content: &Rich, secret: bool) -> Result<()>;
    /// The content as stored, before [`read`] bounds and sanitizes it.
    fn rich(&mut self) -> Result<Rich>;
    fn clear(&mut self) -> Result<()>;
    /// A counter that changes with every write by anyone, where the OS has one (macOS).
    fn change_count(&mut self) -> Option<i64>;
}

/// Copy `content` to `backend` after checking it.
pub fn write(backend: &mut dyn Backend, content: &Rich) -> Result<()> {
    content.validate()?;
    backend.set_rich(content, false)
}

/// Read `backend`'s content, bounded and with its HTML sanitized.
pub fn read(backend: &mut dyn Backend) -> Result<Rich> {
    backend.rich().map(Rich::inbound)
}

/// An in-memory clipboard with a change counter, for tests.
#[derive(Debug, Default)]
pub struct MemoryClipboard {
    pub content: Rich,
    /// The last write asked to be kept out of history.
    pub secret: bool,
    changes: i64,
    /// Report a change counter, as macOS does (`false`: as Windows and Linux).
    pub counts_changes: bool,
}

impl Backend for MemoryClipboard {
    fn set_rich(&mut self, content: &Rich, secret: bool) -> Result<()> {
        self.content = content.clone();
        self.secret = secret;
        self.changes += 1;
        Ok(())
    }
    fn rich(&mut self) -> Result<Rich> {
        Ok(self.content.clone())
    }
    fn clear(&mut self) -> Result<()> {
        self.content = Rich::default();
        self.secret = false;
        self.changes += 1;
        Ok(())
    }
    fn change_count(&mut self) -> Option<i64> {
        self.counts_changes.then_some(self.changes)
    }
}

// ------------------------------------------------------------------ secrets

/// What a secret copy left behind, to tell later whether the clipboard still holds it. Keeps a
/// hash of the text, not the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretStamp {
    change: Option<i64>,
    hash: u64,
}

fn hash_text(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Copy `text` kept out of clipboard history where the platform allows.
pub fn copy_secret(backend: &mut dyn Backend, text: &str) -> Result<SecretStamp> {
    let content = Rich::text(text);
    content.validate()?;
    backend.set_rich(&content, true)?;
    Ok(SecretStamp { change: backend.change_count(), hash: hash_text(text) })
}

/// Clear the clipboard if it still holds the secret `stamp` describes: the change counter is
/// unchanged where there is one, otherwise the text is the same. `Ok(true)` when cleared.
pub fn clear_if_unchanged(backend: &mut dyn Backend, stamp: &SecretStamp) -> Result<bool> {
    let ours = match (stamp.change, backend.change_count()) {
        (Some(then), Some(now)) => then == now,
        _ => {
            let now = backend.rich()?;
            now.html.is_none() && now.png.is_none() && now.files.is_empty() && hash_text(&now.text) == stamp.hash
        }
    };
    if ours {
        backend.clear()?;
    }
    Ok(ours)
}

/// Copy a password or token: kept out of clipboard history and monitoring where supported
/// (macOS `org.nspasteboard.ConcealedType`, Windows history/cloud/monitoring flags, Linux
/// `x-kde-passwordManagerHint`), and cleared after `clear_after` unless something else was
/// copied since.
pub fn set_secret(text: &str, clear_after: Duration) -> Result<()> {
    if clear_after > MAX_SECRET_LIFETIME {
        return Err(NativeError::Invalid("clear secrets within a day".into()));
    }
    let stamp = copy_secret(&mut System, text)?;
    std::thread::Builder::new()
        .name("ashlar-clipboard-secret".into())
        .spawn(move || {
            std::thread::sleep(clear_after);
            if let Err(e) = clear_if_unchanged(&mut System, &stamp) {
                log::warn!("the copied secret was not cleared: {e}");
            }
        })
        .map(drop)
        .map_err(|e| NativeError::Os(crate::os_text(e)))
}

// ------------------------------------------------------------------ HTML

/// Elements dropped with everything inside them.
const DROP_WITH_CONTENT: &[&str] = &[
    "script", "style", "iframe", "object", "embed", "noscript", "noembed", "noframes", "template", "frame", "frameset", "applet", "svg", "math",
    "xml",
];
/// Elements dropped, their content kept.
const DROP_TAG: &[&str] = &[
    "base", "link", "meta", "form", "input", "button", "textarea", "select", "option", "param", "source", "track", "portal", "html", "head", "body",
];
/// Attributes holding a URL.
const URL_ATTRS: &[&str] =
    &["href", "src", "xlink:href", "action", "formaction", "background", "poster", "cite", "longdesc", "lowsrc", "dynsrc", "data", "ping"];

/// Make pasted HTML safe to show or keep: drops comments, scripts, styles, frames and embedded
/// objects (with their content), form controls, event attributes (`on…`), `srcdoc`/`srcset`,
/// risky inline styles, and URLs other than http(s), mailto, tel, relative ones and (for images)
/// `data:image/…` other than SVG. What is left is re-emitted with quoted attributes.
pub fn sanitize_html(input: &str) -> String {
    let mut end = input.len().min(MAX_HTML_BYTES);
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    let input = &input[..end];
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        let Some(lt) = rest.find('<') else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..lt]);
        i += lt;
        let rest = &input[i..];
        if rest.starts_with("<!--") {
            i += rest.find("-->").map_or(rest.len(), |e| e + 3);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            i += rest.find('>').map_or(rest.len(), |e| e + 1);
            continue;
        }
        let Some(tag) = Tag::parse(rest) else {
            out.push_str("&lt;");
            i += 1;
            continue;
        };
        i += tag.len;
        if !tag.complete {
            break;
        }
        let name = tag.name.as_str();
        if DROP_WITH_CONTENT.contains(&name) {
            if !tag.closing && !tag.self_closing {
                i += closing_tag_end(&input[i..], name).unwrap_or(input.len() - i);
            }
            continue;
        }
        if DROP_TAG.contains(&name) {
            continue;
        }
        out.push('<');
        if tag.closing {
            out.push('/');
        }
        out.push_str(name);
        if !tag.closing {
            for (attr, value) in &tag.attrs {
                if !keep_attr(attr, value.as_deref()) {
                    continue;
                }
                out.push(' ');
                out.push_str(attr);
                if let Some(v) = value {
                    out.push_str("=\"");
                    out.push_str(&v.replace('"', "&quot;"));
                    out.push('"');
                }
            }
            if tag.self_closing {
                out.push_str(" /");
            }
        }
        out.push('>');
    }
    out
}

/// One parsed tag.
struct Tag {
    name: String,
    closing: bool,
    self_closing: bool,
    attrs: Vec<(String, Option<String>)>,
    /// Bytes of input it spans.
    len: usize,
    /// Ended by `>` (an unfinished tag at the end of the input is dropped).
    complete: bool,
}

impl Tag {
    /// `s` starts with `<`. `None` when no tag name follows (a literal `<`).
    fn parse(s: &str) -> Option<Tag> {
        let b = s.as_bytes();
        let mut i = 1;
        let closing = b.get(i) == Some(&b'/');
        if closing {
            i += 1;
        }
        if !b.get(i).is_some_and(u8::is_ascii_alphabetic) {
            return None;
        }
        let start = i;
        while b.get(i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'-' || *c == b':') {
            i += 1;
        }
        let name = s[start..i].to_ascii_lowercase();
        let mut attrs = Vec::new();
        let mut self_closing = false;
        loop {
            while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                i += 1;
            }
            match b.get(i) {
                None => return Some(Tag { name, closing, self_closing, attrs, len: s.len(), complete: false }),
                Some(b'>') => return Some(Tag { name, closing, self_closing, attrs, len: i + 1, complete: true }),
                Some(b'/') => {
                    self_closing = true;
                    i += 1;
                    continue;
                }
                Some(_) => {}
            }
            self_closing = false;
            let a = i;
            while b.get(i).is_some_and(|c| !c.is_ascii_whitespace() && !b"=>/".contains(c)) {
                i += 1;
            }
            // `=` right at the start of an attribute: skip it as junk
            let attr = if i == a {
                i += 1;
                continue;
            } else {
                s[a..i].to_ascii_lowercase()
            };
            while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                i += 1;
            }
            let mut value = None;
            if b.get(i) == Some(&b'=') {
                i += 1;
                while b.get(i).is_some_and(|c| c.is_ascii_whitespace()) {
                    i += 1;
                }
                match b.get(i) {
                    Some(&q) if q == b'"' || q == b'\'' => {
                        let v = i + 1;
                        let Some(e) = s[v..].find(q as char) else {
                            return Some(Tag { name, closing, self_closing, attrs, len: s.len(), complete: false });
                        };
                        value = Some(s[v..v + e].to_string());
                        i = v + e + 1;
                    }
                    _ => {
                        let v = i;
                        while b.get(i).is_some_and(|c| !c.is_ascii_whitespace() && *c != b'>') {
                            i += 1;
                        }
                        value = Some(s[v..i].to_string());
                    }
                }
            }
            attrs.push((attr, value));
        }
    }
}

/// Where the closing tag `</name…>` ends in `s`, matched without regard to case.
fn closing_tag_end(s: &str, name: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(p) = s[from..].find("</") {
        let at = from + p + 2;
        let tail = &s.as_bytes()[at..];
        let ends = tail.get(name.len()).is_none_or(|c| c.is_ascii_whitespace() || *c == b'>' || *c == b'/');
        if tail.len() >= name.len() && tail[..name.len()].eq_ignore_ascii_case(name.as_bytes()) && ends {
            return Some(s[at..].find('>').map_or(s.len(), |e| at + e + 1));
        }
        from = at;
    }
    None
}

fn keep_attr(name: &str, value: Option<&str>) -> bool {
    if name.starts_with("on") || name == "srcdoc" || name == "srcset" || name == "is" {
        return false;
    }
    if !name.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_:.".contains(&c)) {
        return false;
    }
    let value = value.unwrap_or("");
    if name == "style" {
        let v: String = value.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
        return !["expression", "javascript:", "url(", "behavior", "-moz-binding", "@import", "\\"].iter().any(|bad| v.contains(bad));
    }
    if URL_ATTRS.contains(&name) {
        return safe_url(value, name == "src" || name == "poster");
    }
    true
}

/// Relative, http(s), mailto, tel, and (with `image`) non-SVG `data:image/…` URLs.
fn safe_url(value: &str, image: bool) -> bool {
    let v: String = value.chars().filter(|c| !c.is_ascii_whitespace() && !c.is_control()).collect::<String>().to_ascii_lowercase();
    let head = &v[..v.find(['/', '?', '#']).unwrap_or(v.len())];
    // an entity or escape before the path could spell a scheme (`javascript&colon;`)
    if head.contains('&') || head.contains('\\') || head.contains('%') {
        return false;
    }
    let Some(colon) = head.find(':') else { return true };
    match &head[..colon] {
        "http" | "https" | "mailto" | "tel" => true,
        "data" => image && v.starts_with("data:image/") && !v.starts_with("data:image/svg"),
        _ => false,
    }
}

// ------------------------------------------------------------------ the system clipboard

/// The system clipboard as a [`Backend`].
#[derive(Debug, Default, Clone, Copy)]
pub struct System;

/// Copy several forms of one piece of content (see [`Rich`] for what each platform keeps).
pub fn set_rich(content: &Rich) -> Result<()> {
    write(&mut System, content)
}

/// The clipboard's content in every form it has, bounded and with the HTML sanitized. Forms the
/// clipboard doesn't hold are empty.
pub fn rich() -> Result<Rich> {
    read(&mut System)
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::borrow::Cow;

    use super::*;

    pub(super) fn map(e: arboard::Error) -> NativeError {
        match e {
            arboard::Error::ContentNotAvailable => NativeError::Os("the clipboard holds no such content".into()),
            arboard::Error::ClipboardNotSupported => NativeError::Unsupported,
            arboard::Error::ClipboardOccupied => NativeError::Os("another application is using the clipboard; try again".into()),
            arboard::Error::ConversionFailure => NativeError::Os("the clipboard content could not be converted".into()),
            // `Unknown` carries a description from the OS; never the clipboard content.
            arboard::Error::Unknown { description } => NativeError::Os(crate::os_text(description)),
            _ => NativeError::Os("the clipboard failed".into()),
        }
    }

    /// Run `f` with a clipboard: the long-lived one on the worker thread (Linux and other X11 /
    /// Wayland systems), or one opened for this call.
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    fn with<T: Send + 'static>(f: impl FnOnce(&mut arboard::Clipboard) -> Result<T> + Send + 'static) -> Result<T> {
        crate::guarded("the clipboard", || {
            let mut c = arboard::Clipboard::new().map_err(map)?;
            f(&mut c)
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn with<T: Send + 'static>(f: impl FnOnce(&mut arboard::Clipboard) -> Result<T> + Send + 'static) -> Result<T> {
        worker::run(f)
    }

    /// One clipboard for the life of the process, on its own thread: on X11 and Wayland the
    /// copying process serves the content, so dropping the clipboard would take it away.
    #[cfg(all(unix, not(target_os = "macos")))]
    mod worker {
        use std::sync::mpsc::{Sender, channel, sync_channel};
        use std::sync::{Mutex, PoisonError};

        use super::*;

        type Job = Box<dyn FnOnce(Result<&mut arboard::Clipboard>) + Send>;

        /// How long a caller waits for the worker (another app may hold the clipboard).
        const WAIT: Duration = Duration::from_secs(5);

        static WORKER: Mutex<Option<Sender<Job>>> = Mutex::new(None);

        fn spawn() -> Result<Sender<Job>> {
            let (tx, rx) = channel::<Job>();
            std::thread::Builder::new()
                .name("ashlar-clipboard".into())
                .spawn(move || {
                    let mut clipboard: Option<arboard::Clipboard> = None;
                    for job in rx {
                        if clipboard.is_none() {
                            match arboard::Clipboard::new() {
                                Ok(c) => clipboard = Some(c),
                                Err(e) => {
                                    job(Err(map(e)));
                                    continue;
                                }
                            }
                        }
                        match clipboard.as_mut() {
                            Some(c) => job(Ok(c)),
                            None => job(Err(NativeError::Unsupported)),
                        }
                    }
                })
                .map_err(|e| NativeError::Os(crate::os_text(e)))?;
            Ok(tx)
        }

        pub(super) fn run<T: Send + 'static>(f: impl FnOnce(&mut arboard::Clipboard) -> Result<T> + Send + 'static) -> Result<T> {
            let (rtx, rrx) = sync_channel(1);
            let job: Job = Box::new(move |c| {
                let r = c.and_then(|c| crate::guarded("the clipboard", || f(c)));
                let _ = rtx.send(r);
            });
            let mut slot = WORKER.lock().unwrap_or_else(PoisonError::into_inner);
            let tx = match slot.as_ref() {
                Some(tx) => tx.clone(),
                None => {
                    let tx = spawn()?;
                    *slot = Some(tx.clone());
                    tx
                }
            };
            if let Err(e) = tx.send(job) {
                // the worker ended: start another for this job
                let tx = spawn()?;
                *slot = Some(tx.clone());
                tx.send(e.0).map_err(|_| NativeError::Os("the clipboard worker stopped".into()))?;
            }
            drop(slot);
            rrx.recv_timeout(WAIT).map_err(|_| NativeError::Os("the clipboard did not answer in time".into()))?
        }
    }

    fn absent<T>(r: std::result::Result<T, arboard::Error>) -> Result<Option<T>> {
        match r {
            Ok(v) => Ok(Some(v)),
            Err(arboard::Error::ContentNotAvailable | arboard::Error::ConversionFailure) => Ok(None),
            Err(e) => Err(map(e)),
        }
    }

    /// The clipboard's text, `None` when it holds no text.
    pub fn text() -> Result<Option<String>> {
        with(|c| absent(c.get_text()))
    }

    pub fn set_text(text: &str) -> Result<()> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(NativeError::Invalid(format!("the text is larger than {MAX_TEXT_BYTES} bytes")));
        }
        let text = text.to_string();
        with(move |c| c.set_text(text).map_err(map))
    }

    fn to_image(img: arboard::ImageData<'_>) -> Result<Image> {
        let (Ok(width), Ok(height)) = (u32::try_from(img.width), u32::try_from(img.height)) else {
            return Err(NativeError::Os("the clipboard image is too large".into()));
        };
        let image = Image { width, height, rgba: img.bytes.into_owned() };
        image.validate()?;
        Ok(image)
    }

    /// The clipboard's image as RGBA8, `None` when it holds no image.
    pub fn image() -> Result<Option<Image>> {
        with(|c| absent(c.get_image())?.map(to_image).transpose())
    }

    pub fn set_image(image: &Image) -> Result<()> {
        image.validate()?;
        let image = image.clone();
        with(move |c| {
            let data = arboard::ImageData { width: image.width as usize, height: image.height as usize, bytes: Cow::Borrowed(&image.rgba) };
            c.set_image(data).map_err(map)
        })
    }

    pub fn clear() -> Result<()> {
        with(|c| c.clear().map_err(map))
    }

    pub(super) fn png_encode(image: &Image) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, image.width, image.height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let fail = |e: png::EncodingError| NativeError::Os(crate::os_text(e));
        let mut w = enc.write_header().map_err(fail)?;
        w.write_image_data(&image.rgba).map_err(fail)?;
        w.finish().map_err(fail)?;
        Ok(out)
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub(super) fn png_decode(bytes: &[u8]) -> Result<Image> {
        let bad = |e: png::DecodingError| NativeError::Invalid(format!("the PNG can't be read: {}", crate::os_text(e)));
        let mut dec = png::Decoder::new_with_limits(std::io::Cursor::new(bytes), png::Limits { bytes: MAX_PNG_BYTES * 4 });
        dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
        let mut reader = dec.read_info().map_err(bad)?;
        let size = reader.output_buffer_size().ok_or_else(|| NativeError::Invalid("the PNG is too large".into()))?;
        let mut buf = vec![0; size];
        let info = reader.next_frame(&mut buf).map_err(bad)?;
        buf.truncate(info.buffer_size());
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect(),
            _ => return Err(NativeError::Invalid("the PNG has an unsupported color type".into())),
        };
        let image = Image { width: info.width, height: info.height, rgba };
        image.validate()?;
        Ok(image)
    }

    /// Mark a write as secret with arboard's per-platform flags.
    #[cfg(not(target_os = "macos"))]
    fn secret_set(set: arboard::Set<'_>) -> arboard::Set<'_> {
        #[cfg(target_os = "windows")]
        {
            use arboard::SetExtWindows;
            set.exclude_from_history().exclude_from_cloud().exclude_from_monitoring()
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            use arboard::SetExtLinux;
            set.exclude_from_history()
        }
        #[cfg(not(any(target_os = "windows", unix)))]
        set
    }

    impl Backend for System {
        #[cfg(target_os = "macos")]
        fn set_rich(&mut self, content: &Rich, secret: bool) -> Result<()> {
            if !content.files.is_empty() {
                let files = content.files.clone();
                return with(move |c| c.set().file_list(&files).map_err(map));
            }
            crate::guarded("the clipboard", || mac::set(content, secret))
        }

        #[cfg(not(target_os = "macos"))]
        fn set_rich(&mut self, content: &Rich, secret: bool) -> Result<()> {
            let content = content.clone();
            with(move |c| {
                if !content.files.is_empty() {
                    return c.set().file_list(&content.files).map_err(map);
                }
                if let Some(html) = content.html {
                    let alt = (!content.text.is_empty()).then_some(content.text);
                    return c.set().html(html, alt).map_err(map);
                }
                if let Some(png) = content.png {
                    let image = png_decode(&png)?;
                    let data = arboard::ImageData { width: image.width as usize, height: image.height as usize, bytes: Cow::Owned(image.rgba) };
                    return c.set().image(data).map_err(map);
                }
                let set = c.set();
                let set = if secret { secret_set(set) } else { set };
                set.text(content.text).map_err(map)
            })
        }

        fn rich(&mut self) -> Result<Rich> {
            with(|c| {
                let text = absent(c.get_text())?.unwrap_or_default();
                let html = absent(c.get().html())?;
                let png = match absent(c.get_image())? {
                    Some(img) => Some(png_encode(&to_image(img)?)?),
                    None => None,
                };
                let files = absent(c.get().file_list())?.unwrap_or_default();
                Ok(Rich { text, html, png, files })
            })
        }

        fn clear(&mut self) -> Result<()> {
            clear()
        }

        fn change_count(&mut self) -> Option<i64> {
            #[cfg(target_os = "macos")]
            return Some(mac::change_count());
            #[cfg(not(target_os = "macos"))]
            None
        }
    }

    #[cfg(target_os = "macos")]
    mod mac {
        use objc2_app_kit::NSPasteboard;
        use objc2_foundation::{NSData, NSString};

        use super::*;

        /// The nspasteboard.org marker clipboard managers skip.
        const CONCEALED: &str = "org.nspasteboard.ConcealedType";

        /// Text, HTML and PNG as representations of one item.
        pub fn set(content: &Rich, secret: bool) -> Result<()> {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            let put = |ty: &str, data: &[u8]| {
                if pb.setData_forType(Some(&NSData::with_bytes(data)), &NSString::from_str(ty)) {
                    Ok(())
                } else {
                    Err(NativeError::Os(format!("the pasteboard refused type `{ty}`")))
                }
            };
            if !content.text.is_empty() {
                put("public.utf8-plain-text", content.text.as_bytes())?;
            }
            if let Some(html) = &content.html {
                put("public.html", html.as_bytes())?;
            }
            if let Some(png) = &content.png {
                put("public.png", png)?;
            }
            if secret {
                put(CONCEALED, b"")?;
            }
            Ok(())
        }

        pub fn change_count() -> i64 {
            NSPasteboard::generalPasteboard().changeCount() as i64
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::*;

    pub fn text() -> Result<Option<String>> {
        Err(NativeError::Unsupported)
    }
    pub fn set_text(_: &str) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn image() -> Result<Option<Image>> {
        Err(NativeError::Unsupported)
    }
    pub fn set_image(_: &Image) -> Result<()> {
        Err(NativeError::Unsupported)
    }
    pub fn clear() -> Result<()> {
        Err(NativeError::Unsupported)
    }

    impl Backend for System {
        fn set_rich(&mut self, _: &Rich, _: bool) -> Result<()> {
            Err(NativeError::Unsupported)
        }
        fn rich(&mut self) -> Result<Rich> {
            Err(NativeError::Unsupported)
        }
        fn clear(&mut self) -> Result<()> {
            Err(NativeError::Unsupported)
        }
        fn change_count(&mut self) -> Option<i64> {
            None
        }
    }
}

pub use imp::{clear, image, set_image, set_text, text};

#[cfg(target_os = "macos")]
mod custom {
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSData, NSString};

    use super::*;

    /// Replace the clipboard with these representations of one piece of content, for example
    /// the app's own format plus `public.utf8-plain-text` for other apps.
    pub fn set_custom(items: &[(&str, &[u8])]) -> Result<()> {
        check(items)?;
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        for (ty, bytes) in items {
            let data = NSData::with_bytes(bytes);
            if !pb.setData_forType(Some(&data), &NSString::from_str(ty)) {
                return Err(NativeError::Os(format!("the pasteboard refused type `{ty}`")));
            }
        }
        Ok(())
    }

    /// The clipboard's data of type `ty`, `None` when it holds none.
    pub fn custom(ty: &str) -> Result<Option<Vec<u8>>> {
        if !valid_type(ty) {
            return Err(NativeError::Invalid(format!("`{}` is not a pasteboard type", ty.chars().take(64).collect::<String>())));
        }
        let pb = NSPasteboard::generalPasteboard();
        let data = pb.dataForType(&NSString::from_str(ty));
        let Some(data) = data else { return Ok(None) };
        if data.len() > MAX_CUSTOM_BYTES {
            return Err(NativeError::Os("the clipboard data is too large".into()));
        }
        Ok(Some(data.to_vec()))
    }

    /// The type names the clipboard currently holds.
    pub fn types() -> Result<Vec<String>> {
        let pb = NSPasteboard::generalPasteboard();
        Ok(pb.types().map(|a| a.iter().take(256).map(|t| t.to_string()).collect()).unwrap_or_default())
    }
}

#[cfg(not(target_os = "macos"))]
mod custom {
    use super::*;

    pub fn set_custom(items: &[(&str, &[u8])]) -> Result<()> {
        check(items)?;
        Err(NativeError::Unsupported)
    }
    pub fn custom(_: &str) -> Result<Option<Vec<u8>>> {
        Err(NativeError::Unsupported)
    }
    pub fn types() -> Result<Vec<String>> {
        Err(NativeError::Unsupported)
    }
}

/// Custom types: `set_custom(&[("com.example.clip", bytes), ("public.utf8-plain-text", text)])`,
/// `custom("com.example.clip")`, `types()`. macOS only for now; elsewhere `Unsupported`.
pub use custom::{custom, set_custom, types};

fn check(items: &[(&str, &[u8])]) -> Result<()> {
    if items.is_empty() || items.len() > 16 {
        return Err(NativeError::Invalid("copy between 1 and 16 representations".into()));
    }
    for (ty, bytes) in items {
        if !valid_type(ty) {
            return Err(NativeError::Invalid(format!("`{}` is not a pasteboard type", ty.chars().take(64).collect::<String>())));
        }
        if bytes.len() > MAX_CUSTOM_BYTES {
            return Err(NativeError::Invalid(format!("`{ty}` is larger than {MAX_CUSTOM_BYTES} bytes")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_are_checked_before_the_os_sees_them() {
        assert!(Image { width: 2, height: 2, rgba: vec![0; 16] }.validate().is_ok());
        assert!(Image { width: 2, height: 2, rgba: vec![0; 15] }.validate().is_err());
        assert!(Image { width: 0, height: 2, rgba: vec![] }.validate().is_err());
        assert!(Image { width: u32::MAX, height: u32::MAX, rgba: vec![] }.validate().is_err());
        assert!(set_image(&Image { width: 3, height: 1, rgba: vec![1, 2] }).is_err());
    }

    #[test]
    fn custom_types_are_validated() {
        assert!(valid_type("com.example.app.clip"));
        assert!(valid_type("application/x-example+json"));
        for bad in ["", "a b", "a\nb", "ü", &"x".repeat(129)] {
            assert!(!valid_type(bad), "{bad:?}");
        }
        assert!(set_custom(&[]).is_err());
        assert!(matches!(set_custom(&[("bad type", b"x")]), Err(NativeError::Invalid(_))));
        assert!(matches!(custom("bad type"), Err(NativeError::Invalid(_)) | Err(NativeError::Unsupported)));
    }

    #[test]
    fn rich_content_round_trips_through_the_memory_backend() {
        let mut m = MemoryClipboard::default();
        let content = Rich { text: "hi".into(), html: Some(r#"<b onclick="x()">hi</b>"#.into()), png: None, files: vec![] };
        write(&mut m, &content).unwrap();
        let back = read(&mut m).unwrap();
        assert_eq!(back.text, "hi");
        assert_eq!(back.html.as_deref(), Some("<b>hi</b>"), "HTML is sanitized on read");
        assert!(!m.secret);
    }

    #[test]
    fn size_caps_and_shapes_are_checked_before_writing() {
        let mut m = MemoryClipboard::default();
        assert!(matches!(write(&mut m, &Rich::default()), Err(NativeError::Invalid(_))), "empty");
        let big = Rich::text("x".repeat(MAX_TEXT_BYTES + 1));
        assert!(matches!(write(&mut m, &big), Err(NativeError::Invalid(_))));
        let html = Rich { html: Some("x".repeat(MAX_HTML_BYTES + 1)), ..Default::default() };
        assert!(matches!(write(&mut m, &html), Err(NativeError::Invalid(_))));
        let not_png = Rich { png: Some(b"GIF89a".to_vec()), ..Default::default() };
        assert!(matches!(write(&mut m, &not_png), Err(NativeError::Invalid(_))));
        let relative = Rich { files: vec!["a.txt".into()], ..Default::default() };
        assert!(matches!(write(&mut m, &relative), Err(NativeError::Invalid(_))));
        let many = Rich { files: vec![std::env::temp_dir(); MAX_FILES + 1], ..Default::default() };
        assert!(matches!(write(&mut m, &many), Err(NativeError::Invalid(_))));
        assert_eq!(m.content, Rich::default(), "nothing was written");
        // what another app put there is bounded on read
        m.content =
            Rich { text: "ok".into(), png: Some(b"not a png".to_vec()), files: vec!["rel".into(), std::env::temp_dir()], html: Some("  ".into()) };
        let r = read(&mut m).unwrap();
        assert_eq!((r.png, r.files, r.html), (None, vec![std::env::temp_dir()], None));
    }

    #[test]
    fn a_secret_clears_only_while_the_clipboard_still_holds_it() {
        for counts_changes in [true, false] {
            let mut m = MemoryClipboard { counts_changes, ..Default::default() };
            let stamp = copy_secret(&mut m, "hunter2").unwrap();
            assert!(m.secret, "kept out of history");
            assert!(clear_if_unchanged(&mut m, &stamp).unwrap());
            assert!(m.content.is_empty());

            let stamp = copy_secret(&mut m, "hunter2").unwrap();
            write(&mut m, &Rich::text("something else")).unwrap();
            assert!(!clear_if_unchanged(&mut m, &stamp).unwrap(), "the user copied since: {counts_changes}");
            assert_eq!(m.content.text, "something else");
        }
        // with a change counter, even the same text copied again is not ours
        let mut m = MemoryClipboard { counts_changes: true, ..Default::default() };
        let stamp = copy_secret(&mut m, "hunter2").unwrap();
        write(&mut m, &Rich::text("hunter2")).unwrap();
        assert!(!clear_if_unchanged(&mut m, &stamp).unwrap());
        assert!(matches!(set_secret("x", MAX_SECRET_LIFETIME * 2), Err(NativeError::Invalid(_))));
    }

    #[test]
    fn the_sanitizer_drops_active_content() {
        let cases = [
            ("<p>a</p><script>alert(1)</script><p>b</p>", "<p>a</p><p>b</p>"),
            ("<SCRIPT type=x>bad</ScRiPt >ok", "ok"),
            ("<style>p{}</style><i>x</i>", "<i>x</i>"),
            (r#"<b onclick="x()" OnMouseOver='y' class=k>t</b>"#, r#"<b class="k">t</b>"#),
            (r#"<a href="javascript:alert(1)">x</a>"#, "<a>x</a>"),
            ("<a href=\" JaVa\tScRiPt:alert(1)\">x</a>", "<a>x</a>"),
            (r#"<a href="jav&#x61;script:x">x</a>"#, "<a>x</a>"),
            (r#"<a href="javascript&colon;x">x</a>"#, "<a>x</a>"),
            (r#"<a href="https://e.com/?q=a&b" title='say "hi"'>x</a>"#, r#"<a href="https://e.com/?q=a&b" title="say &quot;hi&quot;">x</a>"#),
            (r#"<a href="docs/a.html">x</a>"#, r#"<a href="docs/a.html">x</a>"#),
            (
                r#"<img src="data:image/png;base64,AA"><img src="data:image/svg+xml,x"><img src=data:text/html,x>"#,
                r#"<img src="data:image/png;base64,AA"><img><img>"#,
            ),
            (r#"<a href="data:image/png;base64,AA">x</a>"#, "<a>x</a>"),
            ("<iframe src=x>inside</iframe><object>o</object>k", "k"),
            ("<!-- <script>x</script> -->c", "c"),
            ("<!DOCTYPE html><html><body>t</body></html>", "t"),
            (
                r#"<span style="color:red">r</span><span style="background:url(javascript:x)">s</span>"#,
                r#"<span style="color:red">r</span><span>s</span>"#,
            ),
            ("<div srcdoc=x srcset=y>d</div>", "<div>d</div>"),
            ("a < b && c > d", "a &lt; b && c > d"),
            ("<br/><br />", "<br /><br />"),
            ("x<script>never closed", "x"),
            ("x<b class=\"open", "x"),
            ("<form action=x><input value=1>t</form>", "t"),
        ];
        for (input, want) in cases {
            assert_eq!(sanitize_html(input), want, "{input}");
        }
        assert_eq!(sanitize_html("plain ünïcode"), "plain ünïcode");
        let nested = sanitize_html("<scr<script>ipt>alert(1)</script>");
        assert!(!nested.to_ascii_lowercase().contains("<script"), "{nested}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn png_bytes_round_trip() {
        let img = Image { width: 2, height: 1, rgba: vec![255, 0, 0, 255, 0, 0, 255, 128] };
        let png = imp::png_encode(&img).unwrap();
        assert!(png.starts_with(PNG_SIGNATURE));
        assert_eq!(imp::png_decode(&png).unwrap(), img);
        assert!(imp::png_decode(b"\x89PNG\r\n\x1a\njunk").is_err());
    }

    /// Writes to the real clipboard: `cargo test -p ashlar-native clipboard -- --ignored`.
    #[test]
    #[ignore = "replaces the user's clipboard"]
    fn real_clipboard_round_trip() {
        set_text("ashlar clipboard test").unwrap();
        assert_eq!(text().unwrap().as_deref(), Some("ashlar clipboard test"));
        let img = Image { width: 2, height: 1, rgba: vec![255, 0, 0, 255, 0, 0, 255, 255] };
        set_image(&img).unwrap();
        assert_eq!(image().unwrap().map(|i| (i.width, i.height)), Some((2, 1)));
        if cfg!(target_os = "macos") {
            set_custom(&[("com.example.ashlar.test", b"\x00\x01binary"), ("public.utf8-plain-text", b"fallback")]).unwrap();
            assert_eq!(custom("com.example.ashlar.test").unwrap().as_deref(), Some(&b"\x00\x01binary"[..]));
            assert_eq!(text().unwrap().as_deref(), Some("fallback"));
            assert!(types().unwrap().iter().any(|t| t == "com.example.ashlar.test"));
        }
        let html = Rich { text: "plain".into(), html: Some("<b>bold</b><script>x</script>".into()), png: None, files: vec![] };
        set_rich(&html).unwrap();
        let back = rich().unwrap();
        assert_eq!(back.text, "plain");
        assert_eq!(back.html.as_deref(), Some("<b>bold</b>"));
        let files = Rich { files: vec![std::env::temp_dir()], ..Default::default() };
        set_rich(&files).unwrap();
        assert!(!rich().unwrap().files.is_empty());
        set_secret("ashlar secret", Duration::from_millis(100)).unwrap();
        std::thread::sleep(Duration::from_millis(400));
        assert_ne!(text().unwrap().as_deref(), Some("ashlar secret"), "the secret cleared itself");
        clear().unwrap();
    }
}
