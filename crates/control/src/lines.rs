//! The line loop shared by the TCP server and stdio serving: bounded lines, strict parsing, keys.
//!
//! A line that is not a JSON object with a string `method` (an HTTP request line from a browser's
//! cross-origin `fetch`, a stray `nc`, binary junk) gets one error reply and ends the connection,
//! so nothing after it runs: the JSON body of a browser POST can never be executed.

use std::io::{BufRead, BufReader, Read, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::key::keys_match;
use crate::protocol::Reply;

/// Default longest request line (bytes, without the newline).
pub const DEFAULT_MAX_LINE: usize = 4 * 1024 * 1024;

/// One parsed line.
#[derive(Debug, PartialEq)]
pub(crate) enum Line {
    Request {
        id: Option<Value>,
        key: Option<String>,
        method: String,
        params: Value,
    },
    Blank,
    /// Not a request: reply with this error and close the connection.
    Reject(String),
}

pub(crate) fn parse_line(bytes: &[u8]) -> Line {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Line::Reject("not a control request (invalid UTF-8); closing the connection".into());
    };
    let text = text.trim();
    if text.is_empty() {
        return Line::Blank;
    }
    let msg = match serde_json::from_str::<Value>(text) {
        Ok(v) => v,
        Err(e) => return Line::Reject(format!("bad JSON ({e}); expected one JSON request object per line; closing the connection")),
    };
    let Value::Object(mut obj) = msg else {
        return Line::Reject("not a control request (expected a JSON object); closing the connection".into());
    };
    let Some(Value::String(method)) = obj.remove("method") else {
        return Line::Reject("not a control request (missing string `method`); closing the connection".into());
    };
    let key = match obj.remove("key") {
        Some(Value::String(k)) => Some(k),
        _ => None,
    };
    let params = match obj.remove("params") {
        None | Some(Value::Null) => json!({}),
        Some(p) => p,
    };
    Line::Request { id: obj.remove("id"), key, method, params }
}

#[derive(Debug, PartialEq)]
pub(crate) enum ReadError {
    TooLong,
    Io,
}

/// Read one `\n`-terminated line of at most `max` bytes into `buf` (newline and `\r` stripped).
/// `Ok(false)` at end of stream.
pub(crate) fn read_bounded_line(reader: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Result<bool, ReadError> {
    buf.clear();
    // `take` bounds the bytes buffered for one line: a peer can't make us allocate more than `max`.
    let limit = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    let n = reader.take(limit).read_until(b'\n', buf).map_err(|_| ReadError::Io)?;
    if n == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
        return Ok(true);
    }
    if buf.len() > max {
        return Err(ReadError::TooLong);
    }
    // Last line without a newline, then end of stream.
    Ok(true)
}

/// A byte source with an optional overall deadline (TCP before the first authorised request).
pub(crate) trait Source: Read {
    fn set_deadline(&mut self, _deadline: Option<Instant>) {}
}

/// A source without deadlines (pipes).
pub(crate) struct Plain<R>(pub R);

impl<R: Read> Read for Plain<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl<R: Read> Source for Plain<R> {}

/// Read and drop the rest of an oversize line, so closing the socket does not reset the
/// connection before the client has read the error. At most 64 MiB, within 2 s.
fn discard_rest_of_line<S: Source>(r: &mut BufReader<S>) {
    r.get_mut().set_deadline(Some(Instant::now() + Duration::from_secs(2)));
    let mut left: usize = 64 << 20;
    while left > 0 {
        let (used, done) = match r.fill_buf() {
            Ok([]) | Err(_) => return,
            Ok(b) => match b.iter().position(|&c| c == b'\n') {
                Some(i) => (i.saturating_add(1), true),
                None => (b.len(), false),
            },
        };
        r.consume(used);
        if done {
            return;
        }
        left = left.saturating_sub(used);
    }
}

/// What the loop needs to know about authentication.
pub(crate) enum Auth<'a> {
    /// Private channel (a pipe to a child): no key.
    None,
    /// Every request must carry this key, at the top level or through `auth` once per connection.
    Key(&'a str),
}

/// Serve requests from `reader`, answering on `out`, until the peer closes, sends something that
/// is not a request, fails authentication, or `handle` returns `None` (the app is gone).
pub(crate) fn serve_lines<S: Source, W: Write>(
    reader: &mut BufReader<S>,
    mut out: W,
    max_line: usize,
    auth: Auth<'_>,
    mut handle: impl FnMut(String, Value) -> Option<Reply>,
) {
    let mut buf = Vec::new();
    let mut authorised = false;
    // The key an `auth` request gave this connection.
    let mut conn_key: Option<String> = None;
    // One write per reply: `writeln!` with serde_json's Display would issue a syscall (and, with
    // TCP_NODELAY, a packet) per fragment — a 180 KB reply took ~180 ms instead of ~1 ms.
    let send = |out: &mut W, v: Value| {
        let mut line = v.to_string();
        line.push('\n');
        out.write_all(line.as_bytes()).and_then(|()| out.flush()).is_ok()
    };
    loop {
        let (id, key, method, params) = match read_bounded_line(reader, &mut buf, max_line) {
            Ok(false) | Err(ReadError::Io) => return,
            Err(ReadError::TooLong) => {
                let _ = send(&mut out, Reply::err(format!("request line longer than {max_line} bytes; closing the connection")).to_json(None));
                discard_rest_of_line(reader);
                return;
            }
            Ok(true) => match parse_line(&buf) {
                Line::Blank => continue,
                Line::Reject(e) => {
                    let _ = send(&mut out, Reply::err(e).to_json(None));
                    return;
                }
                Line::Request { id, key, method, params } => (id, key, method, params),
            },
        };
        if let Auth::Key(app_key) = auth {
            // The top-level `key`; `auth` may carry it in its params. For other methods `params.key`
            // is an ordinary parameter (`ui.key {"key": "Enter"}`).
            let given = key.or_else(|| if method == "auth" { params.get("key").and_then(Value::as_str).map(str::to_string) } else { None });
            let k = given.as_deref().or(conn_key.as_deref()).unwrap_or("");
            if !keys_match(k, app_key) {
                let _ = send(&mut out, Reply::err("unauthorized").to_json(id.as_ref()));
                return;
            }
            if !authorised {
                authorised = true;
                reader.get_mut().set_deadline(None);
            }
            if method == "auth" {
                conn_key = Some(k.to_string());
                if !send(&mut out, Reply::ok(json!({})).to_json(id.as_ref())) {
                    return;
                }
                continue;
            }
        } else if method == "auth" {
            if !send(&mut out, Reply::ok(json!({})).to_json(id.as_ref())) {
                return;
            }
            continue;
        }
        let Some(r) = handle(method, params) else { return };
        if !send(&mut out, r.to_json(id.as_ref())) {
            return;
        }
    }
}

/// Serve the control protocol on a private byte stream (a child's stdin/stdout) without a key:
/// `handle` answers each request synchronously. Returns when `input` ends or sends junk.
pub fn serve_stream(input: impl Read, output: impl Write, max_line: usize, mut handle: impl FnMut(&str, Value) -> Reply) {
    let mut reader = BufReader::new(Plain(input));
    serve_lines(&mut reader, output, max_line, Auth::None, |m, p| Some(handle(&m, p)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8], auth: Auth<'_>) -> (Vec<Value>, Vec<String>) {
        let mut out = Vec::new();
        let mut calls = Vec::new();
        let mut reader = BufReader::new(Plain(input));
        serve_lines(&mut reader, &mut out, 64, auth, |m, _p| {
            calls.push(m);
            Some(Reply::ok(Value::Null))
        });
        let replies = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str::<Value>(l).unwrap()).collect();
        (replies, calls)
    }

    #[test]
    fn requests_are_answered_in_order_with_ids() {
        let (r, calls) = run(b"{\"id\":1,\"method\":\"a\"}\n\n{\"id\":\"x\",\"method\":\"b\",\"params\":{}}\r\n{\"method\":\"c\"}", Auth::None);
        assert_eq!(calls, ["a", "b", "c"]);
        assert_eq!(
            r,
            [json!({"id": 1, "ok": true, "result": null}), json!({"id": "x", "ok": true, "result": null}), json!({"ok": true, "result": null})]
        );
    }

    #[test]
    fn non_requests_close_the_connection() {
        for junk in [
            &b"[1,2]\n{\"method\":\"a\"}\n"[..],
            b"\"method\"\n{\"method\":\"a\"}\n",
            b"42\n{\"method\":\"a\"}\n",
            b"null\n{\"method\":\"a\"}\n",
            b"{\"id\":1}\n{\"method\":\"a\"}\n",
            b"{\"method\":5}\n{\"method\":\"a\"}\n",
            b"\xff\xfe\x00\x01\n{\"method\":\"a\"}\n",
            b"GET / HTTP/1.1\r\n{\"method\":\"a\"}\n",
            b"{\"method\":\"a\"\n{\"method\":\"a\"}\n",
        ] {
            let (r, calls) = run(junk, Auth::None);
            assert!(calls.is_empty(), "{calls:?}");
            assert_eq!(r.len(), 1);
            assert_eq!(r[0]["ok"], false);
            assert!(r[0]["error"].as_str().unwrap().contains("closing the connection"));
        }
    }

    #[test]
    fn a_request_before_junk_still_runs() {
        let (r, calls) = run(b"{\"method\":\"a\"}\nGET / HTTP/1.1\n{\"method\":\"b\"}\n", Auth::None);
        assert_eq!(calls, ["a"]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1]["ok"], false);
    }

    #[test]
    fn overlong_line_is_refused() {
        let mut input = vec![b'x'; 65];
        input.extend_from_slice(b"\n{\"method\":\"a\"}\n");
        let (r, calls) = run(&input, Auth::None);
        assert!(calls.is_empty());
        assert_eq!(r.len(), 1);
        assert!(r[0]["error"].as_str().unwrap().contains("longer than 64"));
    }

    #[test]
    fn line_at_the_limit_is_accepted() {
        let mut buf = Vec::new();
        let mut input = vec![b'a'; 10];
        input.push(b'\n');
        assert_eq!(read_bounded_line(&mut &input[..], &mut buf, 10), Ok(true));
        assert_eq!(buf, [b'a'; 10]);
        assert_eq!(read_bounded_line(&mut &[b'a'; 11][..], &mut buf, 10), Err(ReadError::TooLong));
    }

    #[test]
    fn keys_are_checked_on_every_request() {
        let (r, calls) = run(b"{\"id\":1,\"key\":\"k\",\"method\":\"a\"}\n{\"id\":2,\"method\":\"b\"}\n{\"method\":\"c\"}\n", Auth::Key("k"));
        assert_eq!(calls, ["a"]);
        assert_eq!(r[1], json!({"id": 2, "ok": false, "error": "unauthorized"}));
        assert_eq!(r.len(), 2);
        // `auth` once per connection; `params.key` authenticates only `auth`.
        let (r, calls) = run(b"{\"method\":\"auth\",\"params\":{\"key\":\"k\"}}\n{\"method\":\"b\",\"params\":{\"key\":\"x\"}}\n", Auth::Key("k"));
        assert_eq!(calls, ["b"]);
        assert_eq!(r[0]["ok"], true);
        let (_, calls) = run(b"{\"method\":\"b\",\"params\":{\"key\":\"k\"}}\n", Auth::Key("k"));
        assert!(calls.is_empty());
        // A wrong key after auth is refused.
        let (r, calls) = run(b"{\"method\":\"auth\",\"key\":\"k\"}\n{\"key\":\"bad\",\"method\":\"b\"}\n{\"method\":\"c\"}\n", Auth::Key("k"));
        assert!(calls.is_empty());
        assert_eq!(r[1]["error"], "unauthorized");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn stream_serving_answers_synchronously() {
        let mut out = Vec::new();
        serve_stream(&b"{\"id\":1,\"method\":\"app.ping\"}\n"[..], &mut out, 1024, |m, _| Reply::ok(json!(m)));
        assert_eq!(String::from_utf8(out).unwrap(), "{\"id\":1,\"ok\":true,\"result\":\"app.ping\"}\n");
    }
}
