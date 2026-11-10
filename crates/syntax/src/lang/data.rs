//! Data formats: JSON (with comments), TOML and YAML.

use crate::scan::{Cur, Out, is_ident, is_op};
use crate::{Kind, State};

const NORMAL: u8 = 0;
const BLOCK: u8 = 1;
const TRIPLE: u8 = 2;
/// YAML block scalar: `n` is the indent of the line that opened it.
const SCALAR: u8 = 3;

fn guard(len: usize) -> usize {
    len.saturating_mul(2).saturating_add(4)
}

/// After a string or bare key: whether a `:` (JSON, YAML) or `=` (TOML) follows.
fn key_follows(c: &Cur<'_>, sep: u8) -> bool {
    c.next_non_ws() == sep
}

pub(crate) fn json(line: &str, st: State, out: &mut Out<'_>) -> State {
    let mut c = Cur::new(line);
    let mut mode = st.mode;
    for _ in 0..guard(line.len()) {
        if c.eof() {
            break;
        }
        let start = c.i;
        let before = mode;
        if mode == BLOCK {
            match c.find(b"*/") {
                Some(end) => {
                    c.i = end + 2;
                    mode = NORMAL;
                }
                None => c.skip_to_end(),
            }
            out.tok(start, c.i, Kind::Comment);
        } else {
            match c.peek() {
                b'"' | b'\'' => {
                    let q = c.peek();
                    c.skip(1);
                    let _ = c.string_body(q, true);
                    let kind = if key_follows(&c, b':') { Kind::Property } else { Kind::String };
                    out.tok(start, c.i, kind);
                }
                b'/' if c.at(1) == b'/' => {
                    c.skip_to_end();
                    out.tok(start, c.i, Kind::Comment);
                }
                b'/' if c.at(1) == b'*' => {
                    c.skip(2);
                    out.tok(start, c.i, Kind::Comment);
                    mode = BLOCK;
                }
                b'-' | b'+' | b'0'..=b'9' => {
                    c.skip(1);
                    c.eat_while(|x| x.is_ascii_alphanumeric() || matches!(x, b'.' | b'+' | b'-'));
                    out.tok(start, c.i, Kind::Number);
                }
                b'{' | b'}' | b'[' | b']' | b',' | b':' => {
                    c.skip(1);
                    out.tok(start, c.i, Kind::Punctuation);
                }
                x if x.is_ascii_alphabetic() => {
                    c.ident();
                    let kind = match c.since(start) {
                        b"true" | b"false" | b"null" => Kind::Constant,
                        _ if key_follows(&c, b':') => Kind::Property,
                        _ => Kind::Plain,
                    };
                    out.tok(start, c.i, kind);
                }
                _ => c.bump(),
            }
        }
        if c.i == start && mode == before {
            c.bump();
        }
    }
    State { mode, n: 0, h: 0 }
}

fn triple_body(c: &mut Cur<'_>, q: u8) -> bool {
    let escapes = q == b'"';
    while !c.eof() {
        let x = c.peek();
        if escapes && x == b'\\' {
            c.skip(1);
            c.bump();
        } else if x == q && c.at(1) == q && c.at(2) == q {
            c.skip(3);
            // a closing run may be four or five quotes long
            let _ = c.eat(&[q]) && c.eat(&[q]);
            return true;
        } else {
            c.bump();
        }
    }
    false
}

pub(crate) fn toml(line: &str, st: State, out: &mut Out<'_>) -> State {
    let mut c = Cur::new(line);
    let mut mode = st.mode;
    let mut q = (st.n & 0xff) as u8;
    for _ in 0..guard(line.len()) {
        if c.eof() {
            break;
        }
        let start = c.i;
        let before = mode;
        if mode == TRIPLE {
            if triple_body(&mut c, q) {
                mode = NORMAL;
            }
            out.tok(start, c.i, Kind::String);
        } else {
            match c.peek() {
                b'#' => {
                    c.skip_to_end();
                    out.tok(start, c.i, Kind::Comment);
                }
                b'[' if c.at_line_start() => {
                    let open = if c.at(1) == b'[' { 2 } else { 1 };
                    c.skip(open);
                    out.tok(start, c.i, Kind::Punctuation);
                    let s = c.i;
                    while !c.eof() && c.peek() != b']' {
                        if matches!(c.peek(), b'"' | b'\'') {
                            let qq = c.peek();
                            c.skip(1);
                            let _ = c.string_body(qq, qq == b'"');
                        } else {
                            c.bump();
                        }
                    }
                    out.tok(s, c.i, Kind::Type);
                    let s = c.i;
                    c.eat_while(|x| x == b']');
                    out.tok(s, c.i, Kind::Punctuation);
                }
                x @ (b'"' | b'\'') => {
                    if c.at(1) == x && c.at(2) == x {
                        c.skip(3);
                        if !triple_body(&mut c, x) {
                            mode = TRIPLE;
                            q = x;
                        }
                        out.tok(start, c.i, Kind::String);
                    } else {
                        c.skip(1);
                        let _ = c.string_body(x, x == b'"');
                        let kind = if key_follows(&c, b'=') || key_follows(&c, b'.') { Kind::Property } else { Kind::String };
                        out.tok(start, c.i, kind);
                    }
                }
                b'0'..=b'9' | b'+' | b'-' => {
                    c.skip(1);
                    c.eat_while(|x| x.is_ascii_alphanumeric() || matches!(x, b'_' | b':' | b'.' | b'+' | b'-'));
                    out.tok(start, c.i, Kind::Number);
                }
                x if x.is_ascii_alphabetic() || x == b'_' => {
                    c.eat_while(|x| is_ident(x) || x == b'-');
                    let kind = if key_follows(&c, b'=') || key_follows(&c, b'.') {
                        Kind::Property
                    } else {
                        match c.since(start) {
                            b"true" | b"false" | b"inf" | b"nan" => Kind::Constant,
                            _ => Kind::Plain,
                        }
                    };
                    out.tok(start, c.i, kind);
                }
                b'=' => {
                    c.skip(1);
                    out.tok(start, c.i, Kind::Operator);
                }
                b'{' | b'}' | b'[' | b']' | b',' | b'.' => {
                    c.skip(1);
                    out.tok(start, c.i, Kind::Punctuation);
                }
                _ => c.bump(),
            }
        }
        if c.i == start && mode == before {
            c.bump();
        }
    }
    let n = if mode == TRIPLE { u16::from(q) } else { 0 };
    State { mode, n, h: 0 }
}

fn indent_of(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

pub(crate) fn yaml(line: &str, st: State, out: &mut Out<'_>) -> State {
    if st.mode == SCALAR && (line.trim().is_empty() || indent_of(line) > usize::from(st.n)) {
        out.tok(0, line.len(), Kind::String);
        return st;
    }
    let mut c = Cur::new(line);
    let indent = indent_of(line);
    if (c.starts(b"---") || c.starts(b"...")) && matches!(c.at(3), 0 | b' ' | b'\t') {
        c.skip(3);
        out.tok(0, 3, Kind::Meta);
    }
    let mut flow = 0u32;
    let mut next = State::START;
    for _ in 0..guard(line.len()) {
        if c.eof() {
            break;
        }
        let start = c.i;
        let b = c.peek();
        let after_ws = start == 0 || matches!(c.b.get(start - 1), Some(b' ' | b'\t'));
        match b {
            b' ' | b'\t' => c.skip_ws(),
            b'#' if after_ws => {
                c.skip_to_end();
                out.tok(start, c.i, Kind::Comment);
            }
            b'-' if matches!(c.at(1), 0 | b' ' | b'\t') => {
                c.skip(1);
                out.tok(start, c.i, Kind::Punctuation);
            }
            b'"' | b'\'' => {
                c.skip(1);
                let _ = c.string_body(b, b == b'"');
                let kind = if c.peek() == b':' || (flow == 0 && key_follows(&c, b':')) { Kind::Property } else { Kind::String };
                out.tok(start, c.i, kind);
            }
            b'&' | b'*' if c.at(1) != b' ' && c.at(1) != 0 => {
                c.skip(1);
                c.eat_while(|x| !matches!(x, b' ' | b'\t' | b',' | b']' | b'}'));
                out.tok(start, c.i, Kind::Variable);
            }
            b'!' => {
                c.eat_while(|x| !matches!(x, b' ' | b'\t'));
                out.tok(start, c.i, Kind::Type);
            }
            b'|' | b'>' if flow == 0 => {
                c.skip(1);
                c.eat_while(|x| x.is_ascii_digit() || x == b'+' || x == b'-');
                out.tok(start, c.i, Kind::Operator);
                let rest = c.b.get(c.i..).unwrap_or_default();
                let trimmed = rest.iter().skip_while(|x| **x == b' ' || **x == b'\t').copied().next();
                if matches!(trimmed, None | Some(b'#')) {
                    next = State { mode: SCALAR, n: indent.min(usize::from(u16::MAX)) as u16, h: 0 };
                }
            }
            b'[' | b'{' => {
                flow = flow.saturating_add(1);
                c.skip(1);
                out.tok(start, c.i, Kind::Punctuation);
            }
            b']' | b'}' => {
                flow = flow.saturating_sub(1);
                c.skip(1);
                out.tok(start, c.i, Kind::Punctuation);
            }
            b',' if flow > 0 => {
                c.skip(1);
                out.tok(start, c.i, Kind::Punctuation);
            }
            b':' if matches!(c.at(1), 0 | b' ' | b'\t') => {
                c.skip(1);
                out.tok(start, c.i, Kind::Punctuation);
            }
            _ => plain_scalar(&mut c, out, flow > 0),
        }
        if c.i == start {
            c.bump();
        }
    }
    next
}

/// A plain scalar: up to `: `, ` #`, the line end or (in flow) a flow indicator. A key when a
/// `:` ends it, otherwise a value.
fn plain_scalar(c: &mut Cur<'_>, out: &mut Out<'_>, flow: bool) {
    let start = c.i;
    let mut end = c.i;
    let mut key = false;
    while !c.eof() {
        let x = c.peek();
        if x == b':' && matches!(c.at(1), 0 | b' ' | b'\t' | b',' | b']' | b'}') {
            key = true;
            break;
        }
        if x == b'#' && matches!(c.b.get(c.i.wrapping_sub(1)), Some(b' ' | b'\t')) {
            break;
        }
        if flow && matches!(x, b',' | b'[' | b']' | b'{' | b'}') {
            break;
        }
        c.bump();
        if x != b' ' && x != b'\t' {
            end = c.i;
        }
    }
    c.i = end.max(start);
    let w = c.since(start);
    let kind = if key {
        Kind::Property
    } else if matches!(w, b"true" | b"false" | b"null" | b"~" | b"True" | b"False" | b"Null" | b"yes" | b"no" | b"on" | b"off") {
        Kind::Constant
    } else if w.first().is_some_and(|x| x.is_ascii_digit() || matches!(x, b'-' | b'+' | b'.'))
        && w.iter().all(|x| x.is_ascii_alphanumeric() || matches!(x, b'.' | b'_' | b'-' | b'+'))
        && w.iter().any(u8::is_ascii_digit)
        && !w.iter().any(|x| is_op(*x) && *x != b'-' && *x != b'+')
    {
        Kind::Number
    } else {
        Kind::String
    };
    out.tok(start, c.i, kind);
}
