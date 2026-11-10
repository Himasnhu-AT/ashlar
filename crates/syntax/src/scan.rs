//! The shared scanning core: a byte cursor that only stops on `char` boundaries, and a span sink
//! that fills gaps with plain text, merges neighbours of the same kind and snaps every offset to
//! a `char` boundary (so a lexer bug can mis-colour but never produce a bad range).

use crate::{Kind, Span};

/// Collects the spans of one line into the caller's vector.
pub(crate) struct Out<'a> {
    spans: &'a mut Vec<Span>,
    line: &'a str,
    base: usize,
    last: usize,
}

impl<'a> Out<'a> {
    pub(crate) fn new(spans: &'a mut Vec<Span>, line: &'a str, base: usize) -> Out<'a> {
        Out { spans, line, base, last: 0 }
    }

    fn snap(&self, p: usize) -> usize {
        let mut p = p.min(self.line.len());
        for _ in 0..4 {
            if self.line.is_char_boundary(p) {
                break;
            }
            p = p.saturating_add(1);
        }
        p.min(self.line.len())
    }

    /// Marks `start..end` as `kind`; anything between the previous token and `start` is plain.
    /// Tokens must come in order; overlap with an earlier token is dropped.
    pub(crate) fn tok(&mut self, start: usize, end: usize, kind: Kind) {
        let start = self.snap(start).max(self.last);
        let end = self.snap(end);
        if start > self.last {
            self.push(self.last, start, Kind::Plain);
        }
        if end > start {
            self.push(start, end, kind);
        }
    }

    fn push(&mut self, s: usize, e: usize, kind: Kind) {
        let (s, e2) = (self.base.saturating_add(s), self.base.saturating_add(e));
        self.last = e;
        if let Some((range, k)) = self.spans.last_mut()
            && *k == kind
            && range.end == s
        {
            range.end = e2;
            return;
        }
        self.spans.push((s..e2, kind));
    }

    /// Fills the rest of the line with plain text.
    pub(crate) fn finish(&mut self) {
        let n = self.line.len();
        self.tok(n, n, Kind::Plain);
    }

    /// Lexes `from..to` of this line with another lexer (a language nested in this one).
    pub(crate) fn embed<T>(&mut self, from: usize, to: usize, f: impl FnOnce(&str, &mut Out<'_>) -> T) -> T {
        let from = self.snap(from).max(self.last);
        let to = self.snap(to).max(from);
        self.tok(from, from, Kind::Plain);
        let sub_line = self.line.get(from..to).unwrap_or("");
        let mut sub = Out { spans: &mut *self.spans, line: sub_line, base: self.base.saturating_add(from), last: 0 };
        let r = f(sub_line, &mut sub);
        sub.finish();
        self.last = to;
        r
    }
}

/// A forward cursor over the bytes of a line.
pub(crate) struct Cur<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) i: usize,
}

impl<'a> Cur<'a> {
    pub(crate) fn new(line: &'a str) -> Cur<'a> {
        Cur { b: line.as_bytes(), i: 0 }
    }

    pub(crate) fn eof(&self) -> bool {
        self.i >= self.b.len()
    }

    /// The byte under the cursor, or 0 at the end.
    pub(crate) fn peek(&self) -> u8 {
        self.at(0)
    }

    /// The byte `k` ahead of the cursor, or 0 past the end.
    pub(crate) fn at(&self, k: usize) -> u8 {
        self.b.get(self.i.saturating_add(k)).copied().unwrap_or(0)
    }

    /// Steps over one whole `char`.
    pub(crate) fn bump(&mut self) {
        let n = utf8_len(self.peek());
        self.i = self.i.saturating_add(n).min(self.b.len());
    }

    /// Steps over `n` bytes the caller knows are ASCII.
    pub(crate) fn skip(&mut self, n: usize) {
        self.i = self.i.saturating_add(n).min(self.b.len());
    }

    pub(crate) fn starts(&self, s: &[u8]) -> bool {
        self.b.get(self.i..).is_some_and(|r| r.starts_with(s))
    }

    /// `starts` ignoring ASCII case.
    pub(crate) fn starts_ci(&self, s: &[u8]) -> bool {
        self.b.get(self.i..self.i.saturating_add(s.len())).is_some_and(|r| r.eq_ignore_ascii_case(s))
    }

    pub(crate) fn eat(&mut self, s: &[u8]) -> bool {
        let yes = self.starts(s);
        if yes {
            self.skip(s.len());
        }
        yes
    }

    /// Steps over ASCII bytes matching `f` (never stops inside a `char`, since `f` sees non-ASCII
    /// bytes only as themselves and every lexer predicate rejects them or accepts them all).
    pub(crate) fn eat_while(&mut self, f: impl Fn(u8) -> bool) {
        while let Some(&c) = self.b.get(self.i) {
            if !f(c) {
                break;
            }
            self.i += 1;
        }
    }

    pub(crate) fn skip_ws(&mut self) {
        self.eat_while(|c| c == b' ' || c == b'\t');
    }

    /// Moves to the end of the line.
    pub(crate) fn skip_to_end(&mut self) {
        self.i = self.b.len();
    }

    /// Steps over an identifier (`[A-Za-z0-9_]` and any non-ASCII).
    pub(crate) fn ident(&mut self) {
        self.eat_while(is_ident);
    }

    /// The text from `start` to the cursor.
    pub(crate) fn since(&self, start: usize) -> &'a [u8] {
        self.b.get(start..self.i).unwrap_or_default()
    }

    /// Whether only spaces and tabs come before the cursor on this line.
    pub(crate) fn at_line_start(&self) -> bool {
        self.b.get(..self.i).is_some_and(|r| r.iter().all(|c| *c == b' ' || *c == b'\t'))
    }

    /// The first byte after spaces and tabs from the cursor, without moving.
    pub(crate) fn next_non_ws(&self) -> u8 {
        let rest = self.b.get(self.i..).unwrap_or_default();
        rest.iter().copied().find(|c| *c != b' ' && *c != b'\t').unwrap_or(0)
    }

    /// The index of the next `pat` at or after the cursor.
    pub(crate) fn find(&self, pat: &[u8]) -> Option<usize> {
        let rest = self.b.get(self.i..)?;
        if pat.is_empty() || rest.len() < pat.len() {
            return None;
        }
        let first = *pat.first()?;
        let mut k = 0usize;
        while k + pat.len() <= rest.len() {
            k += rest.get(k..).and_then(|r| r.iter().position(|c| *c == first))?;
            if rest.get(k..k + pat.len()) == Some(pat) {
                return Some(self.i + k);
            }
            k += 1;
        }
        None
    }

    /// Steps over a string body (after its opening quote) up to and including the closing `q`.
    /// With `escapes`, a backslash hides the next char. Returns whether the string closed.
    pub(crate) fn string_body(&mut self, q: u8, escapes: bool) -> bool {
        while let Some(&c) = self.b.get(self.i) {
            if escapes && c == b'\\' {
                self.i += 1;
                self.bump();
            } else if c == q {
                self.i += 1;
                return true;
            } else {
                self.bump();
            }
        }
        false
    }

    /// Steps over a number starting at the cursor: digits, letters (suffixes, hex, exponents),
    /// `_`, a `.` followed by a digit, and a sign after a decimal exponent.
    pub(crate) fn number(&mut self) {
        let hex = self.starts(b"0x") || self.starts(b"0X");
        self.skip(1);
        while let Some(&c) = self.b.get(self.i) {
            if (c == b'e' || c == b'E') && !hex && matches!(self.at(1), b'+' | b'-') && self.at(2).is_ascii_digit() {
                self.skip(2);
            } else if c.is_ascii_alphanumeric() || c == b'_' || (c == b'.' && self.at(1).is_ascii_digit()) {
                self.i += 1;
            } else {
                break;
            }
        }
    }
}

pub(crate) fn utf8_len(c: u8) -> usize {
    match c {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xFF => 4,
        _ => 1,
    }
}

pub(crate) fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

pub(crate) fn is_ident(c: u8) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

pub(crate) fn is_op(c: u8) -> bool {
    matches!(c, b'+' | b'-' | b'*' | b'/' | b'%' | b'=' | b'<' | b'>' | b'!' | b'&' | b'|' | b'^' | b'~' | b'?')
}

pub(crate) fn is_punct(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b';' | b'.' | b':')
}

/// Whether `w` is in the sorted list `list` (keyword tables are kept sorted; a test checks).
pub(crate) fn is_in(list: &[&str], w: &[u8]) -> bool {
    list.binary_search_by(|k| k.as_bytes().cmp(w)).is_ok()
}

/// `is_in` for a lowercase list, ignoring the case of `w`.
pub(crate) fn is_in_ci(list: &[&str], w: &[u8]) -> bool {
    let mut buf = [0u8; 24];
    let Some(dst) = buf.get_mut(..w.len()) else { return false };
    for (d, s) in dst.iter_mut().zip(w) {
        *d = s.to_ascii_lowercase();
    }
    is_in(list, dst)
}

/// `ALL_CAPS` (two or more bytes, at least one letter, no lowercase).
pub(crate) fn is_all_caps(w: &[u8]) -> bool {
    w.len() > 1 && w.iter().any(u8::is_ascii_uppercase) && w.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// FNV-1a, for remembering a heredoc delimiter in a [`crate::State`].
pub(crate) fn hash(w: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in w {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h | 1
}
