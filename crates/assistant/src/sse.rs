//! Server-sent events: `event:` / `data:` lines, dispatched on a blank line. Bounded: a line
//! longer than [`MAX_LINE`] or a stream longer than [`MAX_STREAM`] is an error, not an
//! allocation.

use std::io::Read;

use crate::error::Error;
use crate::provider::Cancel;

/// Longest line accepted.
pub const MAX_LINE: usize = 4 * 1024 * 1024;
/// Most bytes read from one stream.
pub const MAX_STREAM: usize = 256 * 1024 * 1024;

/// One event: its `event:` name (empty when none was given) and its `data:` lines joined by `\n`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub event: String,
    pub data: String,
}

/// Reads events from a byte stream.
pub struct Reader<R> {
    inner: R,
    buf: Vec<u8>,
    start: usize,
    read_total: usize,
    eof: bool,
    event: String,
    data: Vec<String>,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, buf: Vec::with_capacity(16 * 1024), start: 0, read_total: 0, eof: false, event: String::new(), data: Vec::new() }
    }

    /// The next line without its end (`\n`, `\r\n` or `\r`), `None` at the end of the stream.
    fn line(&mut self) -> Result<Option<String>, Error> {
        loop {
            let pending = self.buf.get(self.start..).unwrap_or_default();
            if let Some(i) = pending.iter().position(|&b| b == b'\n') {
                let mut line = pending.get(..i).unwrap_or_default();
                if line.last() == Some(&b'\r') {
                    line = line.get(..line.len().saturating_sub(1)).unwrap_or_default();
                }
                let text = String::from_utf8_lossy(line).into_owned();
                self.start = self.start.saturating_add(i).saturating_add(1);
                return Ok(Some(text));
            }
            if pending.len() > MAX_LINE {
                return Err(Error::Protocol("a stream line is too long".into()));
            }
            if self.eof {
                if pending.is_empty() {
                    return Ok(None);
                }
                let text = String::from_utf8_lossy(pending).trim_end_matches('\r').to_string();
                self.start = self.buf.len();
                return Ok(Some(text));
            }
            // compact, then read more
            if self.start > 0 {
                self.buf.drain(..self.start.min(self.buf.len()));
                self.start = 0;
            }
            let mut chunk = [0u8; 16 * 1024];
            let n = match self.inner.read(&mut chunk) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Offline(format!("the server (the connection dropped: {e})"))),
            };
            if n == 0 {
                self.eof = true;
                continue;
            }
            self.read_total = self.read_total.saturating_add(n);
            if self.read_total > MAX_STREAM {
                return Err(Error::Protocol("the reply is too long".into()));
            }
            self.buf.extend_from_slice(chunk.get(..n).unwrap_or_default());
        }
    }

    /// The next event, `None` at the end of the stream. Checks `cancel` before each line.
    pub fn next_event(&mut self, cancel: &Cancel) -> Result<Option<Event>, Error> {
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Some(line) = self.line()? else {
                // a last event without its blank line
                if self.data.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(self.take()));
            };
            if line.is_empty() {
                if self.data.is_empty() {
                    self.event.clear();
                    continue;
                }
                return Ok(Some(self.take()));
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_str(), ""),
            };
            match field {
                "event" => self.event = value.to_string(),
                "data" => self.data.push(value.to_string()),
                _ => {}
            }
        }
    }

    fn take(&mut self) -> Event {
        let e = Event { event: std::mem::take(&mut self.event), data: self.data.join("\n") };
        self.data.clear();
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(text: &str) -> Vec<Event> {
        let mut r = Reader::new(text.as_bytes());
        let mut out = Vec::new();
        while let Ok(Some(e)) = r.next_event(&Cancel::new()) {
            out.push(e);
        }
        out
    }

    #[test]
    fn events_lines_and_comments() {
        let ev = all(": ping\n\nevent: a\ndata: {\"x\":1}\n\ndata: one\r\ndata: two\r\n\r\nevent: last\ndata: end");
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0], Event { event: "a".into(), data: "{\"x\":1}".into() });
        assert_eq!(ev[1].data, "one\ntwo");
        assert_eq!(ev[2], Event { event: "last".into(), data: "end".into() });
    }

    #[test]
    fn cancel_stops_reading() {
        let c = Cancel::new();
        c.cancel();
        let mut r = Reader::new("data: x\n\n".as_bytes());
        assert_eq!(r.next_event(&c), Err(Error::Cancelled));
    }

    /// Reads one byte at a time: events split across reads still come out whole.
    struct Trickle<'a>(&'a [u8]);
    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match (self.0.split_first(), buf.first_mut()) {
                (Some((b, rest)), Some(slot)) => {
                    *slot = *b;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Ok(0),
            }
        }
    }

    #[test]
    fn split_reads() {
        let mut r = Reader::new(Trickle(b"event: e\ndata: abc\n\n"));
        assert_eq!(r.next_event(&Cancel::new()), Ok(Some(Event { event: "e".into(), data: "abc".into() })));
        assert_eq!(r.next_event(&Cancel::new()), Ok(None));
    }
}
