//! POSIX shell, bash and zsh: commands, keywords, quoting, `$` expansions and heredocs.

use crate::scan::{Cur, Out, hash, is_ident, is_ident_start, is_in};
use crate::{Kind, State};

const KEYWORDS: &[&str] =
    &["case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in", "select", "then", "time", "until", "while"];
const BUILTINS: &[&str] = &[
    "alias", "break", "cd", "continue", "declare", "echo", "eval", "exec", "exit", "export", "local", "printf", "read", "readonly", "return", "set",
    "shift", "source", "test", "trap", "unset",
];

const NORMAL: u8 = 0;
const SQ: u8 = 1;
const DQ: u8 = 2;
/// Inside a heredoc: `h` is the delimiter's hash, `n` is 1 for `<<-` (leading tabs allowed).
const HEREDOC: u8 = 3;

#[cfg(test)]
pub(crate) fn tables() -> [&'static [&'static str]; 2] {
    [KEYWORDS, BUILTINS]
}

fn is_word(c: u8) -> bool {
    !matches!(c, b' ' | b'\t' | b'|' | b'&' | b';' | b'<' | b'>' | b'(' | b')' | b'$' | b'`' | b'\'' | b'"' | 0)
}

pub(crate) fn lex(line: &str, st: State, out: &mut Out<'_>) -> State {
    let mut c = Cur::new(line);
    if st.mode == HEREDOC {
        let body = if st.n == 1 { line.trim_start_matches('\t') } else { line };
        if hash(body.trim_end().as_bytes()) == st.h {
            out.tok(0, line.len(), Kind::Keyword);
            return State::START;
        }
        out.tok(0, line.len(), Kind::String);
        return st;
    }
    let mut mode = st.mode;
    let mut cmd = true;
    let mut heredoc: Option<(u32, bool)> = None;
    let cap = line.len().saturating_mul(2).saturating_add(4);
    for _ in 0..cap {
        if c.eof() {
            break;
        }
        let start = c.i;
        let before = mode;
        match mode {
            SQ => {
                if c.string_body(b'\'', false) {
                    mode = NORMAL;
                }
                out.tok(start, c.i, Kind::String);
            }
            DQ => {
                if dq_body(&mut c, out) {
                    mode = NORMAL;
                }
            }
            _ => token(&mut c, out, &mut mode, &mut cmd, &mut heredoc),
        }
        if c.i == start && mode == before {
            c.bump();
        }
    }
    match (mode, heredoc) {
        (NORMAL, Some((h, dash))) => State { mode: HEREDOC, n: u16::from(dash), h },
        (m, _) => State { mode: m, n: 0, h: 0 },
    }
}

/// A double-quoted body with `$` expansions coloured as variables; true when it closed.
fn dq_body(c: &mut Cur<'_>, out: &mut Out<'_>) -> bool {
    let mut start = c.i;
    while !c.eof() {
        match c.peek() {
            b'\\' => {
                c.skip(1);
                c.bump();
            }
            b'"' => {
                c.skip(1);
                out.tok(start, c.i, Kind::String);
                return true;
            }
            b'$' if is_ident_start(c.at(1)) || c.at(1) == b'{' => {
                out.tok(start, c.i, Kind::String);
                let s = c.i;
                dollar(c);
                out.tok(s, c.i, Kind::Variable);
                start = c.i;
            }
            _ => c.bump(),
        }
    }
    out.tok(start, c.i, Kind::String);
    false
}

/// `$name`, `${...}`, `$1`, `$@`; the cursor is on the `$`.
fn dollar(c: &mut Cur<'_>) {
    c.skip(1);
    match c.peek() {
        b'{' => {
            let _ = c.string_body(b'}', true);
        }
        x if is_ident_start(x) => c.ident(),
        x if x.is_ascii_digit() || matches!(x, b'@' | b'*' | b'#' | b'?' | b'$' | b'!' | b'-') => c.skip(1),
        _ => {}
    }
}

fn token(c: &mut Cur<'_>, out: &mut Out<'_>, mode: &mut u8, cmd: &mut bool, heredoc: &mut Option<(u32, bool)>) {
    let start = c.i;
    let b = c.peek();
    match b {
        b' ' | b'\t' => c.skip_ws(),
        b'#' => {
            c.skip_to_end();
            out.tok(start, c.i, Kind::Comment);
        }
        b'\'' => {
            c.skip(1);
            if !c.string_body(b'\'', false) {
                *mode = SQ;
            }
            out.tok(start, c.i, Kind::String);
            *cmd = false;
        }
        b'"' => {
            c.skip(1);
            out.tok(start, c.i, Kind::String);
            if !dq_body(c, out) {
                *mode = DQ;
            }
            *cmd = false;
        }
        b'`' => {
            c.skip(1);
            let _ = c.string_body(b'`', true);
            out.tok(start, c.i, Kind::String);
            *cmd = false;
        }
        b'$' if c.at(1) == b'(' => {
            c.skip(if c.at(2) == b'(' { 3 } else { 2 });
            out.tok(start, c.i, Kind::Punctuation);
            *cmd = true;
        }
        b'$' => {
            dollar(c);
            out.tok(start, c.i, Kind::Variable);
            *cmd = false;
        }
        b'<' if c.at(1) == b'<' && c.at(2) != b'<' => {
            c.skip(2);
            let dash = c.eat(b"-");
            out.tok(start, c.i, Kind::Operator);
            c.skip_ws();
            let s = c.i;
            let q = c.peek();
            let word = if q == b'\'' || q == b'"' {
                c.skip(1);
                let w0 = c.i;
                let _ = c.string_body(q, false);
                c.b.get(w0..c.i.saturating_sub(1)).unwrap_or_default()
            } else {
                let w0 = c.i;
                c.eat_while(|x| is_word(x) && x != b'\\');
                c.since(w0)
            };
            if !word.is_empty() {
                *heredoc = Some((hash(word), dash));
                out.tok(s, c.i, Kind::Keyword);
            }
        }
        b'|' | b'&' | b';' | b'(' | b')' => {
            c.eat_while(|x| matches!(x, b'|' | b'&' | b';' | b'(' | b')'));
            out.tok(start, c.i, Kind::Operator);
            *cmd = true;
        }
        b'<' | b'>' => {
            c.eat_while(|x| matches!(x, b'<' | b'>' | b'&' | b'='));
            out.tok(start, c.i, Kind::Operator);
        }
        b'\\' => {
            c.skip(1);
            c.bump();
        }
        _ => word(c, out, cmd),
    }
}

fn word(c: &mut Cur<'_>, out: &mut Out<'_>, cmd: &mut bool) {
    let start = c.i;
    if *cmd && is_ident_start(c.peek()) {
        c.eat_while(is_ident);
        if c.peek() == b'=' || (c.peek() == b'+' && c.at(1) == b'=') {
            out.tok(start, c.i, Kind::Variable);
            let s = c.i;
            c.eat_while(|x| x == b'+' || x == b'=');
            out.tok(s, c.i, Kind::Operator);
            return;
        }
    }
    c.eat_while(|x| is_word(x) && x != b'\\');
    let w = c.since(start);
    if w.is_empty() {
        c.bump();
        return;
    }
    let all_digits = w.iter().all(u8::is_ascii_digit);
    let kind = if is_in(KEYWORDS, w) {
        *cmd = !matches!(w, b"for" | b"case" | b"select" | b"in" | b"function");
        out.tok(start, c.i, Kind::Keyword);
        if w == b"function" {
            c.skip_ws();
            let s = c.i;
            c.eat_while(|x| is_word(x) && x != b'\\');
            out.tok(s, c.i, Kind::Function);
        }
        return;
    } else if all_digits {
        Kind::Number
    } else if *cmd && c.next_non_ws() == b'(' && c.b.get(c.i..).is_some_and(|r| r.iter().filter(|x| **x != b' ').take(2).eq(b"()")) {
        Kind::Function
    } else if *cmd && is_in(BUILTINS, w) {
        Kind::Keyword
    } else if *cmd {
        Kind::Function
    } else if w.starts_with(b"-") {
        Kind::Attribute
    } else {
        Kind::Plain
    };
    out.tok(start, c.i, kind);
    *cmd = false;
}
