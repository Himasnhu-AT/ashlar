//! Markdown (CommonMark plus GitHub tables and task lists): headings, lists, quotes, inline
//! code, emphasis, links, and fenced code blocks highlighted in their own language.

use crate::scan::{Cur, Out};
use crate::{Kind, Lang, State};

/// Inside a fence: `n` = fence length (bits 0–5), `~` fence (bit 6), language index + 1 (bits
/// 8–15); `h` = the nested lexer's state.
const FENCE: u8 = 1;
const COMMENT: u8 = 2;

fn fence_open(line: &str) -> Option<(u8, usize, &str)> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    let ch = *t.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = t.bytes().take_while(|b| *b == ch).count();
    if len < 3 {
        return None;
    }
    let info = t.get(len..).unwrap_or("").trim();
    if ch == b'`' && info.contains('`') {
        return None;
    }
    Some((ch, len, info))
}

fn lang_index(info: &str) -> u16 {
    let word = info.split([' ', '{', ',']).next().unwrap_or("");
    Lang::from_name(word).and_then(|l| Lang::ALL.iter().position(|x| *x == l)).map_or(0, |i| (i as u16).saturating_add(1))
}

pub(crate) fn lex(line: &str, st: State, out: &mut Out<'_>) -> State {
    match st.mode {
        FENCE => fence_line(line, st, out),
        COMMENT => match Cur::new(line).find(b"-->") {
            Some(end) => {
                out.tok(0, end + 3, Kind::Comment);
                inline(line, end + 3, out)
            }
            None => {
                out.tok(0, line.len(), Kind::Comment);
                st
            }
        },
        _ => block(line, out),
    }
}

fn fence_line(line: &str, st: State, out: &mut Out<'_>) -> State {
    let fence_len = usize::from(st.n & 0x3f);
    let ch = if st.n & 0x40 != 0 { b'~' } else { b'`' };
    if let Some((c2, len, info)) = fence_open(line)
        && c2 == ch
        && len >= fence_len
        && info.is_empty()
    {
        out.tok(0, line.len(), Kind::Punctuation);
        return State::START;
    }
    let lang = usize::from(st.n >> 8).checked_sub(1).and_then(|i| Lang::ALL.get(i).copied());
    match lang {
        Some(l) if l != Lang::Markdown => {
            let inner = State::unpack24(st.h);
            let next = out.embed(0, line.len(), |s, o| super::lex(l, s, inner, o));
            State { h: next.pack24(), ..st }
        }
        _ => {
            out.tok(0, line.len(), Kind::String);
            st
        }
    }
}

fn block(line: &str, out: &mut Out<'_>) -> State {
    if let Some((ch, len, info)) = fence_open(line) {
        let start = line.len() - line.trim_start_matches(' ').len();
        let marks = start + len;
        out.tok(start, marks, Kind::Punctuation);
        out.tok(marks, line.len(), Kind::Type);
        let n = (len.min(0x3f) as u16) | if ch == b'~' { 0x40 } else { 0 } | (lang_index(info) << 8);
        return State { mode: FENCE, n, h: 0 };
    }
    let mut c = Cur::new(line);
    c.eat_while(|x| x == b' ' || x == b'\t');
    let s = c.i;
    // headings
    if c.peek() == b'#' {
        c.eat_while(|x| x == b'#');
        if c.i - s <= 6 && matches!(c.peek(), 0 | b' ' | b'\t') {
            out.tok(s, line.len(), Kind::Heading);
            return State::START;
        }
        c.i = s;
    }
    // thematic breaks and setext underlines
    let rest = line.get(s..).unwrap_or("").trim_end();
    let first = rest.as_bytes().first().copied().unwrap_or(0);
    if rest.len() >= 3 && matches!(first, b'-' | b'*' | b'_' | b'=') && rest.bytes().all(|x| x == first || x == b' ') {
        out.tok(s, line.len(), Kind::Punctuation);
        return State::START;
    }
    // quotes and list markers, possibly nested (`> - [ ] item`)
    for _ in 0..8 {
        let m = c.i;
        match c.peek() {
            b'>' => {
                c.skip(1);
                out.tok(m, c.i, Kind::Punctuation);
            }
            b'-' | b'*' | b'+' if matches!(c.at(1), b' ' | b'\t' | 0) => {
                c.skip(1);
                out.tok(m, c.i, Kind::Punctuation);
            }
            b'0'..=b'9' => {
                c.eat_while(|x| x.is_ascii_digit());
                if matches!(c.peek(), b'.' | b')') && matches!(c.at(1), b' ' | b'\t' | 0) && c.i - m <= 9 {
                    c.skip(1);
                    out.tok(m, c.i, Kind::Punctuation);
                } else {
                    c.i = m;
                    break;
                }
            }
            b'[' if matches!(c.at(1), b' ' | b'x' | b'X') && c.at(2) == b']' => {
                c.skip(3);
                out.tok(m, c.i, Kind::Punctuation);
            }
            _ => break,
        }
        c.eat_while(|x| x == b' ' || x == b'\t');
    }
    inline(line, c.i, out)
}

/// Inline markup from `from` to the end of the line. Each kind of delimiter that once failed to
/// find its closer is not looked for again on this line, so the scan stays linear.
fn inline(line: &str, from: usize, out: &mut Out<'_>) -> State {
    let mut c = Cur::new(line);
    c.i = from;
    // closers that are known to be missing from here on: backtick runs 1–3, `*`/`_` runs 1–2, `](`
    let mut no_tick = [false; 17];
    let mut no_em = [[false; 3]; 2];
    let mut no_link = false;
    let cap = line.len().saturating_mul(2).saturating_add(4);
    for _ in 0..cap {
        if c.eof() {
            break;
        }
        let start = c.i;
        match c.peek() {
            b'\\' => {
                c.skip(1);
                c.bump();
            }
            b'`' => {
                c.eat_while(|x| x == b'`');
                let run = c.i - start;
                let known = no_tick.get(run).copied().unwrap_or(true);
                let close = if known { None } else { find_run(&c, b'`', run) };
                match close {
                    Some(end) => {
                        c.i = end + run;
                        out.tok(start, c.i, Kind::String);
                    }
                    None => {
                        if let Some(f) = no_tick.get_mut(run) {
                            *f = true;
                        }
                    }
                }
            }
            x @ (b'*' | b'_') => {
                c.eat_while(|y| y == x);
                let run = (c.i - start).min(2);
                let side = usize::from(x == b'_');
                let flag = no_em.get(side).and_then(|r| r.get(run)).copied().unwrap_or(true);
                let word_before = start > 0 && line.as_bytes().get(start - 1).is_some_and(u8::is_ascii_alphanumeric);
                let opens = !matches!(c.peek(), b' ' | b'\t' | 0) && !(x == b'_' && word_before);
                if opens && !flag {
                    match find_run(&c, x, run) {
                        Some(end) if end > c.i => {
                            c.i = end + run;
                            c.eat_while(|y| y == x);
                            out.tok(start, c.i, Kind::Emphasis);
                        }
                        _ => {
                            if let Some(f) = no_em.get_mut(side).and_then(|r| r.get_mut(run)) {
                                *f = true;
                            }
                        }
                    }
                }
            }
            b'!' if c.at(1) == b'[' => c.skip(1),
            b'[' if !no_link => match link(&c) {
                Link::Found(text_end, end) => {
                    out.tok(start, text_end, Kind::Link);
                    out.tok(text_end, end, Kind::String);
                    c.i = end;
                }
                Link::Skip(to) => c.i = to,
                Link::Never => {
                    no_link = true;
                    c.skip(1);
                }
            },
            b'<' if c.starts(b"<!--") => match c.find(b"-->") {
                Some(end) => {
                    c.i = end + 3;
                    out.tok(start, c.i, Kind::Comment);
                }
                None => {
                    c.skip_to_end();
                    out.tok(start, c.i, Kind::Comment);
                    return State { mode: COMMENT, n: 0, h: 0 };
                }
            },
            b'<' if c.starts_ci(b"<http") || c.starts_ci(b"<mailto:") => {
                match c.find(b">") {
                    Some(end) => c.i = end + 1,
                    None => c.skip(1),
                }
                out.tok(start, c.i, Kind::Link);
            }
            b'|' => {
                c.skip(1);
                out.tok(start, c.i, Kind::Punctuation);
            }
            _ => {
                c.bump();
                c.eat_while(|x| x.is_ascii_alphanumeric() || x == b' ');
            }
        }
        if c.i == start {
            c.bump();
        }
    }
    State::START
}

/// The start of the next run of exactly `run` (or, for emphasis, at least `run`) `ch` bytes.
fn find_run(c: &Cur<'_>, ch: u8, run: usize) -> Option<usize> {
    let rest = c.b.get(c.i..)?;
    let mut k = 0usize;
    while k < rest.len() {
        if rest.get(k) == Some(&ch) {
            let n = rest.get(k..).map_or(0, |r| r.iter().take_while(|x| **x == ch).count());
            if n == run || (ch != b'`' && n >= run) {
                return Some(c.i + k);
            }
            k += n.max(1);
        } else {
            k += 1;
        }
    }
    None
}

enum Link {
    /// `[text]` ends at the first index and the target at the second.
    Found(usize, usize),
    /// `[text]` with no target: carry on after it.
    Skip(usize),
    /// No closer on this line: stop looking for links.
    Never,
}

/// `[text](target)` or `[text][ref]` at the cursor.
fn link(c: &Cur<'_>) -> Link {
    let rest = c.b.get(c.i..).unwrap_or_default();
    let Some(close) = rest.iter().position(|x| *x == b']') else { return Link::Never };
    let text_end = c.i + close + 1;
    let shut = match rest.get(close + 1) {
        Some(b'(') => b')',
        Some(b'[') => b']',
        _ => return Link::Skip(text_end),
    };
    let tail = rest.get(close + 2..).unwrap_or_default();
    match tail.iter().position(|x| *x == shut) {
        Some(end) => Link::Found(text_end, text_end + 1 + end + 1),
        None => Link::Never,
    }
}
