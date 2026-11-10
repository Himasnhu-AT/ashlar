//! SQL (the common subset of PostgreSQL, SQLite and MySQL): case-insensitive keywords.

use crate::scan::{Cur, Out, is_ident, is_ident_start, is_in_ci, is_op, is_punct};
use crate::{Kind, State};

const KEYWORDS: &[&str] = &[
    "add",
    "all",
    "alter",
    "and",
    "any",
    "as",
    "asc",
    "begin",
    "between",
    "by",
    "cascade",
    "case",
    "check",
    "column",
    "commit",
    "constraint",
    "create",
    "cross",
    "database",
    "default",
    "delete",
    "desc",
    "distinct",
    "do",
    "drop",
    "else",
    "end",
    "except",
    "exists",
    "explain",
    "foreign",
    "from",
    "full",
    "function",
    "grant",
    "group",
    "having",
    "if",
    "in",
    "index",
    "inner",
    "insert",
    "intersect",
    "into",
    "is",
    "join",
    "key",
    "left",
    "like",
    "limit",
    "not",
    "offset",
    "on",
    "or",
    "order",
    "outer",
    "over",
    "partition",
    "primary",
    "references",
    "replace",
    "returning",
    "revoke",
    "right",
    "rollback",
    "select",
    "set",
    "table",
    "then",
    "transaction",
    "trigger",
    "union",
    "unique",
    "update",
    "using",
    "values",
    "view",
    "when",
    "where",
    "with",
];
const TYPES: &[&str] = &[
    "bigint",
    "blob",
    "bool",
    "boolean",
    "bytea",
    "char",
    "date",
    "decimal",
    "double",
    "float",
    "int",
    "integer",
    "interval",
    "json",
    "jsonb",
    "numeric",
    "real",
    "serial",
    "smallint",
    "text",
    "time",
    "timestamp",
    "timestamptz",
    "uuid",
    "varchar",
];
const CONSTANTS: &[&str] = &["false", "null", "true"];

const NORMAL: u8 = 0;
const BLOCK: u8 = 1;
const STR: u8 = 2;

#[cfg(test)]
pub(crate) fn tables() -> [&'static [&'static str]; 3] {
    [KEYWORDS, TYPES, CONSTANTS]
}

pub(crate) fn lex(line: &str, st: State, out: &mut Out<'_>) -> State {
    let mut c = Cur::new(line);
    let mut mode = st.mode;
    let cap = line.len().saturating_mul(2).saturating_add(4);
    for _ in 0..cap {
        if c.eof() {
            break;
        }
        let start = c.i;
        let before = mode;
        match mode {
            BLOCK => {
                match c.find(b"*/") {
                    Some(end) => {
                        c.i = end + 2;
                        mode = NORMAL;
                    }
                    None => c.skip_to_end(),
                }
                out.tok(start, c.i, Kind::Comment);
            }
            STR => {
                if c.string_body(b'\'', false) {
                    mode = NORMAL;
                }
                out.tok(start, c.i, Kind::String);
            }
            _ => {
                let b = c.peek();
                match b {
                    b'-' if c.at(1) == b'-' => {
                        c.skip_to_end();
                        out.tok(start, c.i, Kind::Comment);
                    }
                    b'/' if c.at(1) == b'*' => {
                        c.skip(2);
                        out.tok(start, c.i, Kind::Comment);
                        mode = BLOCK;
                    }
                    b'\'' => {
                        c.skip(1);
                        if !c.string_body(b'\'', false) {
                            mode = STR;
                        }
                        out.tok(start, c.i, Kind::String);
                    }
                    b'"' | b'`' => {
                        c.skip(1);
                        let _ = c.string_body(b, false);
                        out.tok(start, c.i, Kind::Variable);
                    }
                    b'0'..=b'9' => {
                        c.number();
                        out.tok(start, c.i, Kind::Number);
                    }
                    b'@' | b':' | b'$' if is_ident(c.at(1)) => {
                        c.skip(1);
                        c.ident();
                        out.tok(start, c.i, Kind::Variable);
                    }
                    b'?' => {
                        c.skip(1);
                        out.tok(start, c.i, Kind::Variable);
                    }
                    x if is_ident_start(x) => {
                        c.ident();
                        let w = c.since(start);
                        let kind = if is_in_ci(KEYWORDS, w) {
                            Kind::Keyword
                        } else if is_in_ci(CONSTANTS, w) {
                            Kind::Constant
                        } else if is_in_ci(TYPES, w) {
                            Kind::Type
                        } else if c.peek() == b'(' {
                            Kind::Function
                        } else {
                            Kind::Plain
                        };
                        out.tok(start, c.i, kind);
                    }
                    x if is_punct(x) => {
                        c.skip(1);
                        out.tok(start, c.i, Kind::Punctuation);
                    }
                    x if is_op(x) => {
                        c.eat_while(is_op);
                        out.tok(start, c.i, Kind::Operator);
                    }
                    _ => c.bump(),
                }
            }
        }
        if c.i == start && mode == before {
            c.bump();
        }
    }
    State { mode, n: 0, h: 0 }
}
