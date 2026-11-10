//! One configurable lexer for the brace-and-semicolon family (Rust, TypeScript/JavaScript, Go,
//! C/C++, Java, C#) and for Python, which shares its token shapes.

use crate::scan::{Cur, Out, is_all_caps, is_ident_start, is_in, is_op, is_punct};
use crate::{Kind, State};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Single {
    /// `'x'` is a character literal.
    Char,
    /// `'x'` is a character literal and `'a` a lifetime.
    Rust,
    /// `'...'` is a string.
    Str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tick {
    None,
    /// Go raw strings.
    Raw,
    /// JavaScript template literals with `${}`.
    Template,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Attr {
    None,
    /// `#[...]` and `#![...]`.
    Rust,
    /// `#include` and friends at the start of a line.
    Preproc,
    /// `@name` annotations and decorators.
    At,
}

pub(crate) struct Spec {
    pub keywords: &'static [&'static str],
    pub types: &'static [&'static str],
    pub constants: &'static [&'static str],
    /// Keywords after which a name is a function definition.
    pub fn_intro: &'static [&'static str],
    /// Keywords after which a name is a type.
    pub type_intro: &'static [&'static str],
    /// `//` and `/* */` comments.
    pub slash_comments: bool,
    pub nested_comments: bool,
    /// `#` line comments.
    pub hash_comment: bool,
    pub single: Single,
    pub tick: Tick,
    /// Rust string prefixes: `b"…"`, `r#"…"#`, `c"…"`, `b'x'`.
    pub rust_strings: bool,
    /// Python string prefixes (`f"…"`, `rb'…'`) and triple quotes.
    pub py_strings: bool,
    /// `"…"` may run over line ends.
    pub multiline_strings: bool,
    /// A capitalised name is a type.
    pub cap_types: bool,
    /// `name!` is a macro call.
    pub macro_bang: bool,
    pub attr: Attr,
    /// `/…/` regular expression literals.
    pub regex: bool,
}

const NORMAL: u8 = 0;
const BLOCK: u8 = 1;
const STR: u8 = 2;
const RAW: u8 = 3;
const TPL: u8 = 4;
const EXPR: u8 = 5;
const GORAW: u8 = 6;
const TRIPLE: u8 = 7;

/// What came before, for telling `/` (divide) from `/regex/`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    Value,
    Op,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intro {
    None,
    Fn,
    Type,
}

struct Lexer<'s, 'l> {
    spec: &'s Spec,
    c: Cur<'l>,
    mode: u8,
    n: u16,
    prev: Prev,
    intro: Intro,
    regex_ok: bool,
}

pub(crate) fn lex(spec: &Spec, line: &str, st: State, out: &mut Out<'_>) -> State {
    let mut lx = Lexer { spec, c: Cur::new(line), mode: st.mode, n: st.n, prev: Prev::Op, intro: Intro::None, regex_ok: true };
    let cap = line.len().saturating_mul(2).saturating_add(4);
    for _ in 0..cap {
        if lx.c.eof() {
            break;
        }
        let (start, mode) = (lx.c.i, lx.mode);
        lx.step(out);
        if lx.c.i == start && lx.mode == mode {
            lx.c.bump();
        }
    }
    let mode = match lx.mode {
        STR if !spec.multiline_strings => NORMAL,
        m => m,
    };
    State { mode, n: if mode == NORMAL { 0 } else { lx.n }, h: 0 }
}

impl Lexer<'_, '_> {
    fn step(&mut self, out: &mut Out<'_>) {
        let start = self.c.i;
        match self.mode {
            BLOCK => {
                let closed = self.block_comment();
                out.tok(start, self.c.i, Kind::Comment);
                if closed {
                    self.mode = NORMAL;
                }
            }
            STR => {
                let closed = self.c.string_body(b'"', true);
                out.tok(start, self.c.i, Kind::String);
                if closed {
                    self.mode = NORMAL;
                    self.prev = Prev::Value;
                }
            }
            RAW => {
                let closed = self.raw_body();
                out.tok(start, self.c.i, Kind::String);
                if closed {
                    self.mode = NORMAL;
                    self.prev = Prev::Value;
                }
            }
            GORAW => {
                let closed = self.c.string_body(b'`', false);
                out.tok(start, self.c.i, Kind::String);
                if closed {
                    self.mode = NORMAL;
                    self.prev = Prev::Value;
                }
            }
            TRIPLE => {
                let q = (self.n & 0xff) as u8;
                let closed = self.triple_body(q);
                out.tok(start, self.c.i, Kind::String);
                if closed {
                    self.mode = NORMAL;
                    self.prev = Prev::Value;
                }
            }
            TPL => self.template(out),
            _ => self.token(out),
        }
    }

    fn block_comment(&mut self) -> bool {
        while !self.c.eof() {
            if self.c.eat(b"*/") {
                self.n = self.n.saturating_sub(1);
                if self.n == 0 {
                    return true;
                }
            } else if self.spec.nested_comments && self.c.eat(b"/*") {
                self.n = self.n.saturating_add(1).min(1000);
            } else {
                self.c.bump();
            }
        }
        false
    }

    /// A Rust raw string body: up to `"` followed by `n` hashes.
    fn raw_body(&mut self) -> bool {
        let hashes = usize::from(self.n);
        while !self.c.eof() {
            if self.c.peek() == b'"' && (1..=hashes).all(|k| self.c.at(k) == b'#') {
                self.c.skip(1 + hashes);
                return true;
            }
            self.c.bump();
        }
        false
    }

    fn triple_body(&mut self, q: u8) -> bool {
        while !self.c.eof() {
            let c = self.c.peek();
            if c == b'\\' {
                self.c.skip(1);
                self.c.bump();
            } else if c == q && self.c.at(1) == q && self.c.at(2) == q {
                self.c.skip(3);
                return true;
            } else {
                self.c.bump();
            }
        }
        false
    }

    /// Inside a template literal: text up to `` ` `` (closes) or `${` (opens an expression).
    fn template(&mut self, out: &mut Out<'_>) {
        let start = self.c.i;
        while !self.c.eof() {
            match self.c.peek() {
                b'\\' => {
                    self.c.skip(1);
                    self.c.bump();
                }
                b'`' => {
                    self.c.skip(1);
                    out.tok(start, self.c.i, Kind::String);
                    self.mode = NORMAL;
                    self.prev = Prev::Value;
                    return;
                }
                b'$' if self.c.at(1) == b'{' => {
                    out.tok(start, self.c.i, Kind::String);
                    out.tok(self.c.i, self.c.i + 2, Kind::Punctuation);
                    self.c.skip(2);
                    self.mode = EXPR;
                    self.n = 1;
                    self.prev = Prev::Op;
                    return;
                }
                _ => self.c.bump(),
            }
        }
        out.tok(start, self.c.i, Kind::String);
    }

    fn token(&mut self, out: &mut Out<'_>) {
        let spec = self.spec;
        let start = self.c.i;
        let b = self.c.peek();
        match b {
            b' ' | b'\t' | b'\r' => self.c.skip_ws(),
            b'/' if spec.slash_comments && self.c.at(1) == b'/' => {
                self.c.skip_to_end();
                out.tok(start, self.c.i, Kind::Comment);
            }
            b'/' if spec.slash_comments && self.c.at(1) == b'*' => {
                self.c.skip(2);
                out.tok(start, self.c.i, Kind::Comment);
                self.mode = BLOCK;
                self.n = 1;
            }
            b'#' if spec.attr == Attr::Rust && (self.c.at(1) == b'[' || (self.c.at(1) == b'!' && self.c.at(2) == b'[')) => {
                self.rust_attr();
                out.tok(start, self.c.i, Kind::Attribute);
            }
            b'#' if spec.attr == Attr::Preproc && self.c.at_line_start() => {
                self.c.skip(1);
                self.c.skip_ws();
                let word = self.c.i;
                self.c.ident();
                let include = self.c.since(word) == b"include" || self.c.since(word) == b"import";
                out.tok(start, self.c.i, Kind::Attribute);
                if include {
                    self.c.skip_ws();
                    if self.c.peek() == b'<' {
                        let s = self.c.i;
                        self.c.skip(1);
                        let _ = self.c.string_body(b'>', false);
                        out.tok(s, self.c.i, Kind::String);
                    }
                }
            }
            b'#' if spec.hash_comment => {
                self.c.skip_to_end();
                out.tok(start, self.c.i, Kind::Comment);
            }
            b'@' if spec.attr == Attr::At && is_ident_start(self.c.at(1)) => {
                self.c.skip(1);
                self.c.ident();
                while self.c.peek() == b'.' && is_ident_start(self.c.at(1)) {
                    self.c.skip(1);
                    self.c.ident();
                }
                out.tok(start, self.c.i, Kind::Attribute);
            }
            b'"' => self.quoted(start, b'"', out),
            b'\'' => match spec.single {
                Single::Rust => {
                    let kind = self.rust_quote();
                    out.tok(start, self.c.i, kind);
                    self.prev = Prev::Value;
                }
                Single::Char | Single::Str => self.quoted(start, b'\'', out),
            },
            b'`' if spec.tick == Tick::Raw => {
                self.c.skip(1);
                if !self.c.string_body(b'`', false) {
                    self.mode = GORAW;
                }
                out.tok(start, self.c.i, Kind::String);
                self.prev = Prev::Value;
            }
            b'`' if spec.tick == Tick::Template => {
                if self.mode == EXPR {
                    // a template inside `${}`: kept to this line
                    self.c.skip(1);
                    let _ = self.c.string_body(b'`', true);
                    out.tok(start, self.c.i, Kind::String);
                    self.prev = Prev::Value;
                } else {
                    self.c.skip(1);
                    out.tok(start, self.c.i, Kind::String);
                    self.mode = TPL;
                }
            }
            b'0'..=b'9' => {
                self.c.number();
                out.tok(start, self.c.i, Kind::Number);
                self.prev = Prev::Value;
            }
            b'.' if self.c.at(1).is_ascii_digit() => {
                self.c.number();
                out.tok(start, self.c.i, Kind::Number);
                self.prev = Prev::Value;
            }
            c if is_ident_start(c) => self.word(out),
            b'{' => {
                if self.mode == EXPR {
                    self.n = self.n.saturating_add(1);
                }
                self.c.skip(1);
                out.tok(start, self.c.i, Kind::Punctuation);
                self.prev = Prev::Op;
            }
            b'}' => {
                self.c.skip(1);
                out.tok(start, self.c.i, Kind::Punctuation);
                if self.mode == EXPR {
                    self.n = self.n.saturating_sub(1);
                    if self.n == 0 {
                        self.mode = TPL;
                    }
                }
                self.prev = Prev::Op;
            }
            b'/' if spec.regex && self.prev == Prev::Op && self.regex_ok => {
                if self.regex() {
                    out.tok(start, self.c.i, Kind::String);
                    self.prev = Prev::Value;
                } else {
                    self.regex_ok = false;
                    self.c.skip(1);
                    out.tok(start, self.c.i, Kind::Operator);
                }
            }
            c if is_punct(c) => {
                self.c.skip(1);
                if c == b':' {
                    let _ = self.c.eat(b":");
                }
                out.tok(start, self.c.i, Kind::Punctuation);
                self.prev = if matches!(c, b')' | b']') { Prev::Value } else { Prev::Op };
            }
            c if is_op(c) => {
                self.c.eat_while(is_op);
                out.tok(start, self.c.i, Kind::Operator);
                self.prev = Prev::Op;
            }
            _ => self.c.bump(),
        }
    }

    /// A `"…"` or `'…'` literal starting at the cursor (Python triple quotes included).
    fn quoted(&mut self, start: usize, q: u8, out: &mut Out<'_>) {
        if self.spec.py_strings && self.c.at(1) == q && self.c.at(2) == q {
            self.c.skip(3);
            if !self.triple_body(q) {
                self.mode = TRIPLE;
                self.n = u16::from(q);
            }
        } else {
            self.c.skip(1);
            let closed = self.c.string_body(q, true);
            if !closed && q == b'"' && self.spec.multiline_strings {
                self.mode = STR;
            }
        }
        out.tok(start, self.c.i, Kind::String);
        self.prev = Prev::Value;
    }

    fn rust_attr(&mut self) {
        let mut depth = 0u32;
        while !self.c.eof() {
            match self.c.peek() {
                b'[' => depth += 1,
                b']' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        self.c.skip(1);
                        return;
                    }
                }
                b'"' => {
                    self.c.skip(1);
                    let _ = self.c.string_body(b'"', true);
                    continue;
                }
                _ => {}
            }
            self.c.bump();
        }
    }

    /// `'x'`, `'\n'` (characters) or `'a` (a lifetime or label).
    fn rust_quote(&mut self) -> Kind {
        if self.c.at(1) == b'\\' {
            self.c.skip(1);
            let _ = self.c.string_body(b'\'', true);
            return Kind::String;
        }
        let len = crate::scan::utf8_len(self.c.at(1));
        if self.c.at(1) != b'\'' && self.c.at(1) != 0 && self.c.at(1 + len) == b'\'' {
            self.c.skip(1);
            self.c.bump();
            self.c.skip(1);
            return Kind::String;
        }
        self.c.skip(1);
        if is_ident_start(self.c.peek()) {
            self.c.ident();
            return Kind::Variable;
        }
        Kind::Punctuation
    }

    /// A `/regex/flags` literal; false (cursor unmoved) if none closes on this line.
    fn regex(&mut self) -> bool {
        let save = self.c.i;
        self.c.skip(1);
        if matches!(self.c.peek(), b'/' | b'*' | b' ' | 0) {
            self.c.i = save;
            return false;
        }
        let mut class = false;
        while !self.c.eof() {
            match self.c.peek() {
                b'\\' => {
                    self.c.skip(1);
                    self.c.bump();
                    continue;
                }
                b'[' => class = true,
                b']' => class = false,
                b'/' if !class => {
                    self.c.skip(1);
                    self.c.eat_while(|c| c.is_ascii_alphabetic());
                    return true;
                }
                _ => {}
            }
            self.c.bump();
        }
        self.c.i = save;
        false
    }

    fn word(&mut self, out: &mut Out<'_>) {
        let spec = self.spec;
        let start = self.c.i;
        self.c.ident();
        let w = self.c.since(start);
        if spec.rust_strings && self.rust_prefixed(w) {
            out.tok(start, self.c.i, Kind::String);
            self.prev = Prev::Value;
            return;
        }
        if spec.py_strings && matches!(self.c.peek(), b'"' | b'\'') && w.len() <= 2 && w.iter().all(|c| b"rRbBfFuU".contains(c)) {
            let q = self.c.peek();
            let s = self.c.i;
            out.tok(start, s, Kind::String);
            self.quoted(s, q, out);
            return;
        }
        let intro = self.intro;
        self.intro = Intro::None;
        let kind = if is_in(spec.keywords, w) {
            if is_in(spec.fn_intro, w) {
                self.intro = Intro::Fn;
            } else if is_in(spec.type_intro, w) {
                self.intro = Intro::Type;
            }
            Kind::Keyword
        } else if is_in(spec.constants, w) {
            Kind::Constant
        } else if is_in(spec.types, w) {
            Kind::Type
        } else if intro == Intro::Fn {
            Kind::Function
        } else if intro == Intro::Type {
            Kind::Type
        } else if spec.macro_bang && self.c.peek() == b'!' && self.c.at(1) != b'=' {
            self.c.skip(1);
            Kind::Function
        } else if self.c.peek() == b'(' {
            Kind::Function
        } else if is_all_caps(w) {
            Kind::Constant
        } else if spec.cap_types && w.first().is_some_and(u8::is_ascii_uppercase) {
            Kind::Type
        } else {
            Kind::Plain
        };
        out.tok(start, self.c.i, kind);
        // `return /re/` and `typeof /re/` start a regex; other keywords and names end a value
        self.prev = if kind == Kind::Keyword && !matches!(w, b"this" | b"self" | b"super") { Prev::Op } else { Prev::Value };
    }

    /// After a Rust prefix word (`b`, `r`, `br`, `c`, `cr`): the rest of a prefixed literal.
    fn rust_prefixed(&mut self, w: &[u8]) -> bool {
        let raw = matches!(w, b"r" | b"br" | b"cr");
        if raw && matches!(self.c.peek(), b'"' | b'#') {
            let save = self.c.i;
            let h0 = self.c.i;
            self.c.eat_while(|c| c == b'#');
            let hashes = self.c.i - h0;
            if self.c.peek() != b'"' || hashes > 255 {
                self.c.i = save;
                return false;
            }
            self.c.skip(1);
            self.n = hashes as u16;
            if !self.raw_body() {
                self.mode = RAW;
            }
            return true;
        }
        match (w, self.c.peek()) {
            (b"b" | b"c", b'"') => {
                self.c.skip(1);
                if !self.c.string_body(b'"', true) {
                    self.mode = STR;
                }
                true
            }
            (b"b", b'\'') => {
                self.c.skip(1);
                let _ = self.c.string_body(b'\'', true);
                true
            }
            _ => false,
        }
    }
}
