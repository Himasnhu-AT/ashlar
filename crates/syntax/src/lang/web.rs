//! HTML (and XML-like markup), with `<script>` and `<style>` bodies highlighted as TypeScript
//! and CSS; and CSS itself.

use super::{TS, clike};
use crate::scan::{Cur, Out, is_ident, is_ident_start};
use crate::{Kind, State};

const TEXT: u8 = 0;
/// Inside a tag: `n` is 1 for `<script>`, 2 for `<style>`, 0 otherwise.
const TAG: u8 = 1;
const COMMENT: u8 = 2;
/// Inside `<script>`: `h` holds the nested lexer's state.
const SCRIPT: u8 = 3;
const STYLE: u8 = 4;

fn guard(len: usize) -> usize {
    len.saturating_mul(2).saturating_add(4)
}

fn is_name(c: u8) -> bool {
    is_ident(c) || matches!(c, b'-' | b':' | b'.')
}

/// The next case-insensitive `</name` at or after the cursor.
fn find_close(c: &Cur<'_>, name: &[u8]) -> Option<usize> {
    let mut k = c.i;
    while let Some(off) = c.b.get(k..).and_then(|r| r.windows(2).position(|w| w == b"</")) {
        let at = k + off;
        let tail = c.b.get(at + 2..at + 2 + name.len());
        if tail.is_some_and(|t| t.eq_ignore_ascii_case(name)) {
            return Some(at);
        }
        k = at + 2;
    }
    None
}

pub(crate) fn html(line: &str, st: State, out: &mut Out<'_>) -> State {
    let mut c = Cur::new(line);
    let mut mode = st.mode;
    let mut n = st.n;
    let mut inner = st.h;
    for _ in 0..guard(line.len()) {
        if c.eof() {
            break;
        }
        let start = c.i;
        let before = mode;
        match mode {
            COMMENT => {
                match c.find(b"-->") {
                    Some(end) => {
                        c.i = end + 3;
                        mode = TEXT;
                    }
                    None => c.skip_to_end(),
                }
                out.tok(start, c.i, Kind::Comment);
            }
            SCRIPT | STYLE => {
                let name: &[u8] = if mode == SCRIPT { b"script" } else { b"style" };
                let end = find_close(&c, name);
                let to = end.unwrap_or(c.b.len());
                let st_in = State::unpack24(inner);
                let next = if mode == SCRIPT {
                    out.embed(start, to, |l, o| clike::lex(&TS, l, st_in, o))
                } else {
                    out.embed(start, to, |l, o| css(l, st_in, o))
                };
                inner = next.pack24();
                c.i = to;
                if end.is_some() {
                    mode = TEXT;
                    inner = 0;
                }
            }
            TAG => {
                let b = c.peek();
                match b {
                    b' ' | b'\t' => c.skip_ws(),
                    b'>' => {
                        c.skip(1);
                        out.tok(start, c.i, Kind::Punctuation);
                        mode = match n {
                            1 => SCRIPT,
                            2 => STYLE,
                            _ => TEXT,
                        };
                        n = 0;
                    }
                    b'/' if c.at(1) == b'>' => {
                        c.skip(2);
                        out.tok(start, c.i, Kind::Punctuation);
                        mode = TEXT;
                        n = 0;
                    }
                    b'"' | b'\'' => {
                        c.skip(1);
                        let _ = c.string_body(b, false);
                        out.tok(start, c.i, Kind::String);
                    }
                    b'=' => {
                        c.skip(1);
                        out.tok(start, c.i, Kind::Operator);
                        c.skip_ws();
                        if !matches!(c.peek(), b'"' | b'\'') {
                            let s = c.i;
                            c.eat_while(|x| !matches!(x, b' ' | b'\t' | b'>'));
                            out.tok(s, c.i, Kind::String);
                        }
                    }
                    x if is_name(x) || x >= 0x80 => {
                        c.eat_while(is_name);
                        out.tok(start, c.i, Kind::Attribute);
                    }
                    _ => c.bump(),
                }
            }
            _ => match c.peek() {
                b'<' if c.starts(b"<!--") => {
                    c.skip(4);
                    out.tok(start, c.i, Kind::Comment);
                    mode = COMMENT;
                }
                b'<' if c.at(1) == b'!' || c.at(1) == b'?' => {
                    match c.find(b">") {
                        Some(end) => c.i = end + 1,
                        None => c.skip_to_end(),
                    }
                    out.tok(start, c.i, Kind::Meta);
                }
                b'<' if is_ident_start(c.at(1)) || (c.at(1) == b'/' && is_ident_start(c.at(2))) => {
                    let closing = c.at(1) == b'/';
                    c.skip(if closing { 2 } else { 1 });
                    out.tok(start, c.i, Kind::Punctuation);
                    let s = c.i;
                    c.eat_while(is_name);
                    let name = c.since(s);
                    out.tok(s, c.i, Kind::Tag);
                    mode = TAG;
                    n = if closing {
                        0
                    } else if name.eq_ignore_ascii_case(b"script") {
                        1
                    } else if name.eq_ignore_ascii_case(b"style") {
                        2
                    } else {
                        0
                    };
                }
                b'&' => {
                    c.skip(1);
                    c.eat_while(|x| x.is_ascii_alphanumeric() || x == b'#');
                    if c.peek() == b';' {
                        c.skip(1);
                        out.tok(start, c.i, Kind::Constant);
                    }
                }
                _ => {
                    c.bump();
                    c.eat_while(|x| x != b'<' && x != b'&' && x < 0x80);
                }
            },
        }
        if c.i == start && mode == before {
            c.bump();
        }
    }
    let h = if matches!(mode, SCRIPT | STYLE) { inner } else { 0 };
    State { mode, n, h }
}

const CSS_COMMENT: u8 = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Unknown,
    Selector,
    Property,
    Value,
}

/// Whether the statement starting at the cursor is a selector (`{` comes before `;` or `}`).
fn selector_ahead(c: &Cur<'_>) -> Ctx {
    let rest = c.b.get(c.i..).unwrap_or_default();
    match rest.iter().find(|x| matches!(x, b'{' | b';' | b'}')) {
        Some(b'{') => Ctx::Selector,
        Some(_) => Ctx::Property,
        // runs on past the line: a declaration if there's a `:` with a space after it
        None if rest.windows(2).any(|w| w == b": ") || rest.ends_with(b":") => Ctx::Property,
        None => Ctx::Selector,
    }
}

/// CSS (and the shared syntax of SCSS and Less). `n` is the brace depth.
pub(crate) fn css(line: &str, st: State, out: &mut Out<'_>) -> State {
    let mut c = Cur::new(line);
    let mut mode = st.mode;
    let mut depth = st.n;
    let mut ctx = Ctx::Unknown;
    for _ in 0..guard(line.len()) {
        if c.eof() {
            break;
        }
        let start = c.i;
        let before = mode;
        if mode == CSS_COMMENT {
            match c.find(b"*/") {
                Some(end) => {
                    c.i = end + 2;
                    mode = 0;
                }
                None => c.skip_to_end(),
            }
            out.tok(start, c.i, Kind::Comment);
        } else {
            let b = c.peek();
            if ctx == Ctx::Unknown && !matches!(b, b' ' | b'\t' | b'/') {
                ctx = if depth == 0 { Ctx::Selector } else { selector_ahead(&c) };
            }
            match b {
                b' ' | b'\t' => c.skip_ws(),
                b'/' if c.at(1) == b'*' => {
                    c.skip(2);
                    out.tok(start, c.i, Kind::Comment);
                    mode = CSS_COMMENT;
                }
                b'/' if c.at(1) == b'/' => {
                    c.skip_to_end();
                    out.tok(start, c.i, Kind::Comment);
                }
                b'{' => {
                    depth = depth.saturating_add(1);
                    c.skip(1);
                    out.tok(start, c.i, Kind::Punctuation);
                    ctx = Ctx::Unknown;
                }
                b'}' => {
                    depth = depth.saturating_sub(1);
                    c.skip(1);
                    out.tok(start, c.i, Kind::Punctuation);
                    ctx = Ctx::Unknown;
                }
                b';' => {
                    c.skip(1);
                    out.tok(start, c.i, Kind::Punctuation);
                    ctx = Ctx::Unknown;
                }
                b':' if ctx == Ctx::Property => {
                    c.skip(1);
                    out.tok(start, c.i, Kind::Punctuation);
                    ctx = Ctx::Value;
                }
                b'"' | b'\'' => {
                    c.skip(1);
                    let _ = c.string_body(b, true);
                    out.tok(start, c.i, Kind::String);
                }
                b'@' if is_ident_start(c.at(1)) => {
                    c.skip(1);
                    c.eat_while(|x| is_ident(x) || x == b'-');
                    out.tok(start, c.i, Kind::Keyword);
                    // `@media (max-width: 600px)`: conditions read like declarations
                    ctx = Ctx::Property;
                }
                b'#' if ctx == Ctx::Value => {
                    c.skip(1);
                    c.eat_while(|x| x.is_ascii_alphanumeric());
                    out.tok(start, c.i, Kind::Number);
                }
                b'0'..=b'9' => {
                    number_with_unit(&mut c);
                    out.tok(start, c.i, Kind::Number);
                }
                b'.' | b'-' | b'+' if c.at(1).is_ascii_digit() && ctx == Ctx::Value => {
                    c.skip(1);
                    number_with_unit(&mut c);
                    out.tok(start, c.i, Kind::Number);
                }
                b'!' if ctx == Ctx::Value => {
                    c.skip(1);
                    c.ident();
                    out.tok(start, c.i, Kind::Keyword);
                }
                b'.' | b'#' if ctx == Ctx::Selector && is_ident_start(c.at(1)) => {
                    c.skip(1);
                    c.eat_while(|x| is_ident(x) || x == b'-');
                    out.tok(start, c.i, if b == b'.' { Kind::Type } else { Kind::Constant });
                }
                b':' if ctx == Ctx::Selector => {
                    c.eat_while(|x| x == b':');
                    c.eat_while(|x| is_ident(x) || x == b'-');
                    out.tok(start, c.i, Kind::Attribute);
                }
                b'[' if ctx == Ctx::Selector => {
                    let _ = c.string_body(b']', true);
                    out.tok(start, c.i, Kind::Attribute);
                }
                x if is_ident_start(x) || (x == b'-' && (is_ident_start(c.at(1)) || c.at(1) == b'-')) => {
                    c.eat_while(|x| is_ident(x) || x == b'-');
                    let kind = match ctx {
                        _ if c.peek() == b'(' => Kind::Function,
                        Ctx::Selector => Kind::Tag,
                        Ctx::Property | Ctx::Unknown => Kind::Property,
                        Ctx::Value => Kind::Constant,
                    };
                    out.tok(start, c.i, kind);
                }
                b'(' | b')' | b',' | b':' => {
                    c.skip(1);
                    out.tok(start, c.i, Kind::Punctuation);
                }
                b'>' | b'+' | b'~' | b'*' | b'&' | b'=' => {
                    c.skip(1);
                    out.tok(start, c.i, Kind::Operator);
                }
                _ => c.bump(),
            }
        }
        if c.i == start && mode == before {
            c.bump();
        }
    }
    State { mode, n: depth, h: 0 }
}

fn number_with_unit(c: &mut Cur<'_>) {
    c.eat_while(|x| x.is_ascii_digit() || x == b'.');
    c.eat_while(|x| x.is_ascii_alphabetic() || x == b'%');
}
