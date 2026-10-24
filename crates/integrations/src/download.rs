//! Downloads that resume, check the size and the sha256, and fall back to the next URL.
//!
//! A file is written to `<dest>.part` and renamed into place only once its size and sha256 match. A
//! `.part` left by an earlier attempt is resumed with a `Range` request (its bytes are hashed first);
//! a server that ignores the range and answers 200 starts the file over. A dropped connection is
//! resumed a few times on the same URL before the next URL is tried. Bytes past the expected size, or
//! a hash that doesn't match, discard the partial file.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

/// One HTTP answer: the status and the body as a stream.
pub struct HttpResponse {
    pub status: u16,
    pub body: Box<dyn Read + Send>,
}

/// HTTP GET, the only request the manager makes. `from > 0` asks for the bytes from there on
/// (`Range: bytes=<from>-`).
pub trait Http: Send + Sync {
    fn get(&self, url: &str, from: u64) -> Result<HttpResponse, String>;
}

/// Why a download failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    #[error("cancelled")]
    Cancelled,
    #[error("couldn't download {what}: {detail}")]
    Failed { what: String, detail: String },
    #[error("couldn't write {path}: {detail}")]
    Disk { path: String, detail: String },
}

/// Progress callbacks get the bytes of this file written so far.
pub type Progress<'a> = &'a (dyn Fn(u64) + Sync);

/// How often one URL is retried after a dropped connection, resuming each time.
const RETRIES_PER_URL: usize = 3;
const CHUNK: usize = 256 * 1024;

fn disk(path: &Path, e: impl std::fmt::Display) -> DownloadError {
    DownloadError::Disk { path: path.display().to_string(), detail: e.to_string() }
}

/// `<dest>.part`.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// The sha256 of a file, lower-case hex.
pub fn sha256_file(path: &Path, cancel: &AtomicBool) -> Result<String, DownloadError> {
    let mut f = File::open(path).map_err(|e| disk(path, e))?;
    let mut h = Sha256::new();
    hash_into(&mut f, &mut h, u64::MAX, cancel, path)?;
    Ok(hex(&h.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hash_into(r: &mut impl Read, h: &mut Sha256, limit: u64, cancel: &AtomicBool, path: &Path) -> Result<u64, DownloadError> {
    let mut buf = vec![0u8; CHUNK];
    let mut n = 0u64;
    let mut r = r.take(limit);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(DownloadError::Cancelled);
        }
        let got = r.read(&mut buf).map_err(|e| disk(path, e))?;
        if got == 0 {
            return Ok(n);
        }
        h.update(buf.get(..got).unwrap_or_default());
        n = n.saturating_add(got as u64);
    }
}

/// Download `size` bytes with this `sha256` from the first URL that serves them, into `dest`.
/// Returns at once when `dest` already exists with the right size (it was verified when it was put there).
pub fn download(
    http: &dyn Http,
    urls: &[String],
    dest: &Path,
    size: u64,
    sha256: &str,
    progress: Progress<'_>,
    cancel: &AtomicBool,
) -> Result<(), DownloadError> {
    let what = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if fs::metadata(dest).is_ok_and(|m| m.is_file() && m.len() == size) {
        progress(size);
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(|e| disk(dir, e))?;
    }
    let part = part_path(dest);
    let mut last = String::from("no URL");
    for url in urls {
        match fetch_one(http, url, &part, size, sha256, progress, cancel) {
            Ok(()) => {
                fs::rename(&part, dest).map_err(|e| disk(dest, e))?;
                return Ok(());
            }
            Err(Attempt::Fatal(e)) => return Err(e),
            Err(Attempt::Next(detail)) => {
                log::warn!(target: "integrations", "{what}: {detail} ({})", redact(url));
                last = detail;
            }
        }
    }
    Err(DownloadError::Failed { what, detail: last })
}

/// A URL for the log, without its query (which may carry a signature).
fn redact(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

enum Attempt {
    /// Give up (cancelled, the disk failed).
    Fatal(DownloadError),
    /// Try the next URL.
    Next(String),
}

fn fetch_one(http: &dyn Http, url: &str, part: &Path, size: u64, sha256: &str, progress: Progress<'_>, cancel: &AtomicBool) -> Result<(), Attempt> {
    let fatal = Attempt::Fatal;
    let mut file = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(part).map_err(|e| fatal(disk(part, e)))?;
    let mut have = file.metadata().map_err(|e| fatal(disk(part, e)))?.len();
    if have > size {
        file.set_len(0).map_err(|e| fatal(disk(part, e)))?;
        have = 0;
    }
    // Hash what an earlier attempt left.
    let mut hasher = Sha256::new();
    file.seek(SeekFrom::Start(0)).map_err(|e| fatal(disk(part, e)))?;
    let hashed = hash_into(&mut file, &mut hasher, have, cancel, part).map_err(fatal)?;
    if hashed != have {
        have = hashed;
        file.set_len(have).map_err(|e| fatal(disk(part, e)))?;
    }
    progress(have);
    let mut failures = 0usize;
    while have < size {
        if cancel.load(Ordering::SeqCst) {
            return Err(fatal(DownloadError::Cancelled));
        }
        let response = match http.get(url, have) {
            Ok(r) => r,
            Err(e) => {
                failures += 1;
                if failures >= RETRIES_PER_URL {
                    return Err(Attempt::Next(e));
                }
                continue;
            }
        };
        match response.status {
            206 if have > 0 => {}
            200 => {
                // The whole file, from the start.
                if have > 0 {
                    file.set_len(0).map_err(|e| fatal(disk(part, e)))?;
                    hasher = Sha256::new();
                    have = 0;
                    progress(0);
                }
            }
            416 if have > 0 => {
                // Our partial file doesn't fit what the server has: start over.
                file.set_len(0).map_err(|e| fatal(disk(part, e)))?;
                hasher = Sha256::new();
                have = 0;
                continue;
            }
            status => return Err(Attempt::Next(format!("HTTP {status}"))),
        }
        file.seek(SeekFrom::Start(have)).map_err(|e| fatal(disk(part, e)))?;
        let started_at = have;
        let mut body = response.body;
        let mut buf = vec![0u8; CHUNK];
        let dropped = loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(fatal(DownloadError::Cancelled));
            }
            let got = match body.read(&mut buf) {
                Ok(0) => break None,
                Ok(n) => n,
                Err(e) => break Some(e.to_string()),
            };
            let chunk = buf.get(..got).unwrap_or_default();
            if have.saturating_add(got as u64) > size {
                drop(file);
                let _ = fs::remove_file(part);
                return Err(Attempt::Next(format!("the server sent more than the expected {size} bytes")));
            }
            file.write_all(chunk).map_err(|e| fatal(disk(part, e)))?;
            hasher.update(chunk);
            have += got as u64;
            progress(have);
        };
        if have < size {
            // A request that moved forward resets the count: only a stalled URL is given up on.
            failures = if have > started_at { 0 } else { failures + 1 };
            if failures >= RETRIES_PER_URL {
                return Err(Attempt::Next(dropped.unwrap_or_else(|| format!("the download stopped at {have} of {size} bytes"))));
            }
        }
    }
    file.sync_all().map_err(|e| fatal(disk(part, e)))?;
    drop(file);
    let got = hex(&hasher.finalize());
    if got != sha256 {
        let _ = fs::remove_file(part);
        return Err(Attempt::Next(format!("sha256 mismatch (got {got})")));
    }
    Ok(())
}

/// Read a small body (a catalog) whole, refusing more than `max` bytes.
pub fn get_small(http: &dyn Http, url: &str, max: usize) -> Result<(u16, Vec<u8>), String> {
    let r = http.get(url, 0)?;
    let mut body = Vec::new();
    r.body.take(max as u64 + 1).read_to_end(&mut body).map_err(|e| e.to_string())?;
    if body.len() > max {
        return Err(format!("the answer is over {max} bytes"));
    }
    Ok((r.status, body))
}

#[cfg(all(feature = "https", not(target_arch = "wasm32")))]
pub use https::HttpsClient;

#[cfg(all(feature = "https", not(target_arch = "wasm32")))]
mod https {
    use std::sync::Arc;
    use std::time::Duration;

    use super::{Http, HttpResponse};

    /// [`Http`] over ureq + rustls (RustCrypto provider, the OS certificate verifier). Follows up to five
    /// redirects (a downloads host may send a client to a fallback copy). One request may take at most 30
    /// minutes; a longer download continues with the next request, where it stopped.
    pub struct HttpsClient {
        agent: ureq::Agent,
        user_agent: String,
    }

    impl HttpsClient {
        pub fn new(user_agent: &str) -> HttpsClient {
            let provider = Arc::new(rustls_rustcrypto::provider());
            let agent = ureq::Agent::config_builder()
                .tls_config(
                    ureq::tls::TlsConfig::builder()
                        .provider(ureq::tls::TlsProvider::Rustls)
                        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                        .unversioned_rustls_crypto_provider(provider)
                        .build(),
                )
                .https_only(false)
                .http_status_as_error(false)
                .max_redirects(5)
                .timeout_connect(Some(Duration::from_secs(15)))
                .timeout_recv_response(Some(Duration::from_secs(30)))
                .timeout_global(Some(Duration::from_secs(30 * 60)))
                .build()
                .into();
            HttpsClient { agent, user_agent: user_agent.to_string() }
        }
    }

    impl Http for HttpsClient {
        fn get(&self, url: &str, from: u64) -> Result<HttpResponse, String> {
            let mut req = self.agent.get(url).header("user-agent", &self.user_agent);
            if from > 0 {
                req = req.header("range", &format!("bytes={from}-"));
            }
            let response = req.call().map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            Ok(HttpResponse { status, body: Box::new(response.into_body().into_reader()) })
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashMap;
    use std::io::Cursor;
    use std::sync::Mutex;

    use super::*;

    /// Serves files from memory; can cut a body short, ignore ranges or fail.
    #[derive(Default)]
    pub struct FakeHttp {
        pub files: Mutex<HashMap<String, Vec<u8>>>,
        /// Serve at most this many bytes per request.
        pub cut: Mutex<Option<usize>>,
        pub ignore_range: bool,
        pub requests: Mutex<Vec<(String, u64)>>,
    }

    impl FakeHttp {
        pub fn with(files: &[(&str, &[u8])]) -> FakeHttp {
            let f = FakeHttp::default();
            for (k, v) in files {
                f.files.lock().unwrap().insert((*k).to_string(), v.to_vec());
            }
            f
        }
    }

    impl Http for FakeHttp {
        fn get(&self, url: &str, from: u64) -> Result<HttpResponse, String> {
            self.requests.lock().unwrap().push((url.to_string(), from));
            let Some(data) = self.files.lock().unwrap().get(url).cloned() else {
                return Ok(HttpResponse { status: 404, body: Box::new(Cursor::new(Vec::new())) });
            };
            let (status, mut bytes) = if from > 0 && !self.ignore_range { (206, data[from as usize..].to_vec()) } else { (200, data) };
            if let Some(n) = *self.cut.lock().unwrap() {
                bytes.truncate(n);
            }
            Ok(HttpResponse { status, body: Box::new(Cursor::new(bytes)) })
        }
    }

    pub fn sha(data: &[u8]) -> String {
        hex(&Sha256::digest(data))
    }

    fn no_progress(_: u64) {}

    #[test]
    fn downloads_verify_and_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"hello integration".repeat(1000);
        let http = FakeHttp::with(&[("https://b.example.com/f", &data)]);
        let dest = dir.path().join("sub/f.bin");
        let urls = vec!["https://a.example.com/f".to_string(), "https://b.example.com/f".to_string()];
        let cancel = AtomicBool::new(false);
        download(&http, &urls, &dest, data.len() as u64, &sha(&data), &no_progress, &cancel).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert!(!part_path(&dest).exists());
        // Already there: no request.
        let before = http.requests.lock().unwrap().len();
        download(&http, &urls, &dest, data.len() as u64, &sha(&data), &no_progress, &cancel).unwrap();
        assert_eq!(http.requests.lock().unwrap().len(), before);
    }

    #[test]
    fn dropped_connections_resume_with_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let http = FakeHttp::with(&[("https://a.example.com/f", &data)]);
        *http.cut.lock().unwrap() = Some(40_000);
        let dest = dir.path().join("f");
        let urls = vec!["https://a.example.com/f".to_string()];
        download(&http, &urls, &dest, data.len() as u64, &sha(&data), &no_progress, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        let starts: Vec<u64> = http.requests.lock().unwrap().iter().map(|r| r.1).collect();
        assert_eq!(starts, [0, 40_000, 80_000]);
    }

    #[test]
    fn a_partial_file_from_an_earlier_run_is_resumed_or_restarted() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"0123456789".repeat(100);
        let dest = dir.path().join("f");
        fs::write(part_path(&dest), &data[..300]).unwrap();
        let http = FakeHttp::with(&[("https://a.example.com/f", &data)]);
        let urls = vec!["https://a.example.com/f".to_string()];
        download(&http, &urls, &dest, 1000, &sha(&data), &no_progress, &AtomicBool::new(false)).unwrap();
        assert_eq!(http.requests.lock().unwrap()[0].1, 300);
        assert_eq!(fs::read(&dest).unwrap(), data);

        // A server that ignores Range: the file starts over and still verifies.
        let dest2 = dir.path().join("g");
        fs::write(part_path(&dest2), &data[..300]).unwrap();
        let http = FakeHttp { ignore_range: true, ..FakeHttp::with(&[("https://a.example.com/f", &data)]) };
        download(&http, &urls, &dest2, 1000, &sha(&data), &no_progress, &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(&dest2).unwrap(), data);
    }

    #[test]
    fn wrong_bytes_are_refused_and_cancel_stops() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"abc".repeat(10);
        let http = FakeHttp::with(&[("https://a.example.com/f", &data)]);
        let urls = vec!["https://a.example.com/f".to_string()];
        let dest = dir.path().join("f");
        let err = download(&http, &urls, &dest, 30, &sha(b"other"), &no_progress, &AtomicBool::new(false)).unwrap_err();
        assert!(matches!(err, DownloadError::Failed { ref detail, .. } if detail.contains("sha256")), "{err}");
        assert!(!dest.exists() && !part_path(&dest).exists());
        let err = download(&http, &urls, &dest, 10, &sha(&data), &no_progress, &AtomicBool::new(false)).unwrap_err();
        assert!(matches!(err, DownloadError::Failed { ref detail, .. } if detail.contains("more than")), "{err}");
        let err = download(&http, &urls, &dest, 30, &sha(&data), &no_progress, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(err, DownloadError::Cancelled);
    }
}
