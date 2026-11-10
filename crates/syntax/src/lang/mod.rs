//! The built-in lexers. Every lexer reads one line (without its line ending) forward once, never
//! backtracks past a token it gave up on, and returns the state the next line starts in.

mod clike;
mod data;
mod diff;
mod markdown;
mod shell;
mod sql;
mod web;

use crate::scan::Out;
use crate::{Lang, State};
use clike::{Attr, Single, Spec, Tick};

pub(crate) fn lex(lang: Lang, line: &str, st: State, out: &mut Out<'_>) -> State {
    match lang {
        Lang::Rust => clike::lex(&RUST, line, st, out),
        Lang::TypeScript | Lang::JavaScript => clike::lex(&TS, line, st, out),
        Lang::Go => clike::lex(&GO, line, st, out),
        Lang::C => clike::lex(&C, line, st, out),
        Lang::Cpp => clike::lex(&CPP, line, st, out),
        Lang::Java => clike::lex(&JAVA, line, st, out),
        Lang::CSharp => clike::lex(&CSHARP, line, st, out),
        Lang::Python => clike::lex(&PYTHON, line, st, out),
        Lang::Shell => shell::lex(line, st, out),
        Lang::Sql => sql::lex(line, st, out),
        Lang::Json => data::json(line, st, out),
        Lang::Toml => data::toml(line, st, out),
        Lang::Yaml => data::yaml(line, st, out),
        Lang::Html => web::html(line, st, out),
        Lang::Css => web::css(line, st, out),
        Lang::Markdown => markdown::lex(line, st, out),
        Lang::Diff => diff::lex(line, st, out),
    }
}

const BASE: Spec = Spec {
    keywords: &[],
    types: &[],
    constants: &[],
    fn_intro: &[],
    type_intro: &[],
    slash_comments: true,
    nested_comments: false,
    hash_comment: false,
    single: Single::Char,
    tick: Tick::None,
    rust_strings: false,
    py_strings: false,
    multiline_strings: false,
    cap_types: false,
    macro_bang: false,
    attr: Attr::None,
    regex: false,
};

pub(crate) const RUST: Spec = Spec {
    keywords: &[
        "Self", "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "fn", "for", "if", "impl", "in",
        "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "type", "union",
        "unsafe", "use", "where", "while", "yield",
    ],
    types: &["bool", "char", "f32", "f64", "i128", "i16", "i32", "i64", "i8", "isize", "str", "u128", "u16", "u32", "u64", "u8", "usize"],
    constants: &["None", "false", "true"],
    fn_intro: &["fn"],
    type_intro: &["enum", "struct", "trait", "type", "union"],
    nested_comments: true,
    single: Single::Rust,
    rust_strings: true,
    multiline_strings: true,
    cap_types: true,
    macro_bang: true,
    attr: Attr::Rust,
    ..BASE
};

pub(crate) const TS: Spec = Spec {
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "constructor",
        "continue",
        "debugger",
        "declare",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "finally",
        "for",
        "from",
        "function",
        "get",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "keyof",
        "let",
        "namespace",
        "new",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "satisfies",
        "set",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "try",
        "type",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    types: &["any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "unknown"],
    constants: &["Infinity", "NaN", "false", "null", "true", "undefined"],
    fn_intro: &["function"],
    type_intro: &["class", "enum", "extends", "implements", "interface", "type"],
    single: Single::Str,
    tick: Tick::Template,
    cap_types: true,
    attr: Attr::At,
    regex: true,
    ..BASE
};

pub(crate) const GO: Spec = Spec {
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "type",
        "var",
    ],
    types: &[
        "any",
        "bool",
        "byte",
        "complex128",
        "complex64",
        "error",
        "float32",
        "float64",
        "int",
        "int16",
        "int32",
        "int64",
        "int8",
        "rune",
        "string",
        "uint",
        "uint16",
        "uint32",
        "uint64",
        "uint8",
        "uintptr",
    ],
    constants: &["false", "iota", "nil", "true"],
    fn_intro: &["func"],
    type_intro: &["type"],
    tick: Tick::Raw,
    ..BASE
};

const C_TYPES: &[&str] = &[
    "bool",
    "char",
    "double",
    "float",
    "int",
    "int16_t",
    "int32_t",
    "int64_t",
    "int8_t",
    "long",
    "ptrdiff_t",
    "short",
    "signed",
    "size_t",
    "ssize_t",
    "uint16_t",
    "uint32_t",
    "uint64_t",
    "uint8_t",
    "uintptr_t",
    "unsigned",
    "void",
    "wchar_t",
];

pub(crate) const C: Spec = Spec {
    keywords: &[
        "auto", "break", "case", "const", "continue", "default", "do", "else", "enum", "extern", "for", "goto", "if", "inline", "register",
        "restrict", "return", "sizeof", "static", "struct", "switch", "typedef", "union", "volatile", "while",
    ],
    types: C_TYPES,
    constants: &["NULL", "false", "true"],
    type_intro: &["enum", "struct", "union"],
    attr: Attr::Preproc,
    ..BASE
};

pub(crate) const CPP: Spec = Spec {
    keywords: &[
        "alignas",
        "alignof",
        "auto",
        "break",
        "case",
        "catch",
        "class",
        "co_await",
        "co_return",
        "co_yield",
        "concept",
        "const",
        "consteval",
        "constexpr",
        "constinit",
        "continue",
        "decltype",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "explicit",
        "export",
        "extern",
        "final",
        "for",
        "friend",
        "goto",
        "if",
        "inline",
        "mutable",
        "namespace",
        "new",
        "noexcept",
        "operator",
        "override",
        "private",
        "protected",
        "public",
        "register",
        "requires",
        "return",
        "sizeof",
        "static",
        "static_assert",
        "struct",
        "switch",
        "template",
        "this",
        "throw",
        "try",
        "typedef",
        "typeid",
        "typename",
        "union",
        "using",
        "virtual",
        "volatile",
        "while",
    ],
    types: C_TYPES,
    constants: &["NULL", "false", "nullptr", "true"],
    type_intro: &["class", "enum", "struct", "union"],
    cap_types: true,
    attr: Attr::Preproc,
    ..BASE
};

pub(crate) const JAVA: Spec = Spec {
    keywords: &[
        "abstract",
        "assert",
        "break",
        "case",
        "catch",
        "class",
        "continue",
        "default",
        "do",
        "else",
        "enum",
        "extends",
        "final",
        "finally",
        "for",
        "if",
        "implements",
        "import",
        "instanceof",
        "interface",
        "native",
        "new",
        "package",
        "private",
        "protected",
        "public",
        "record",
        "return",
        "static",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "throws",
        "transient",
        "try",
        "var",
        "void",
        "volatile",
        "while",
        "yield",
    ],
    types: &["boolean", "byte", "char", "double", "float", "int", "long", "short"],
    constants: &["false", "null", "true"],
    type_intro: &["class", "enum", "extends", "implements", "interface", "record"],
    cap_types: true,
    attr: Attr::At,
    ..BASE
};

pub(crate) const CSHARP: Spec = Spec {
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "base",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "delegate",
        "do",
        "else",
        "enum",
        "event",
        "explicit",
        "extern",
        "finally",
        "fixed",
        "for",
        "foreach",
        "get",
        "goto",
        "if",
        "implicit",
        "in",
        "interface",
        "internal",
        "is",
        "lock",
        "namespace",
        "new",
        "operator",
        "out",
        "override",
        "params",
        "private",
        "protected",
        "public",
        "readonly",
        "record",
        "ref",
        "return",
        "sealed",
        "set",
        "sizeof",
        "stackalloc",
        "static",
        "struct",
        "switch",
        "this",
        "throw",
        "try",
        "typeof",
        "unchecked",
        "unsafe",
        "using",
        "var",
        "virtual",
        "void",
        "volatile",
        "when",
        "where",
        "while",
        "yield",
    ],
    types: &[
        "bool", "byte", "char", "decimal", "double", "dynamic", "float", "int", "long", "object", "sbyte", "short", "string", "uint", "ulong",
        "ushort",
    ],
    constants: &["false", "null", "true"],
    type_intro: &["class", "enum", "interface", "record", "struct"],
    cap_types: true,
    attr: Attr::Preproc,
    ..BASE
};

pub(crate) const PYTHON: Spec = Spec {
    keywords: &[
        "and", "as", "assert", "async", "await", "break", "case", "class", "cls", "continue", "def", "del", "elif", "else", "except", "finally",
        "for", "from", "global", "if", "import", "in", "is", "lambda", "match", "nonlocal", "not", "or", "pass", "raise", "return", "self", "try",
        "while", "with", "yield",
    ],
    types: &["bool", "bytes", "complex", "dict", "float", "frozenset", "int", "list", "object", "set", "str", "tuple", "type"],
    constants: &["False", "None", "NotImplemented", "True"],
    fn_intro: &["def"],
    type_intro: &["class"],
    slash_comments: false,
    hash_comment: true,
    single: Single::Str,
    py_strings: true,
    cap_types: true,
    attr: Attr::At,
    ..BASE
};

/// Every keyword table, for the sortedness test.
#[cfg(test)]
pub(crate) fn tables() -> Vec<&'static [&'static str]> {
    let mut t = Vec::new();
    for s in [&RUST, &TS, &GO, &C, &CPP, &JAVA, &CSHARP, &PYTHON] {
        t.extend([s.keywords, s.types, s.constants, s.fn_intro, s.type_intro]);
    }
    t.extend(shell::tables());
    t.extend(sql::tables());
    t
}

#[cfg(test)]
mod tests {
    #[test]
    fn keyword_tables_are_sorted_and_unique() {
        for t in super::tables() {
            for w in t.windows(2) {
                assert!(w[0] < w[1], "not sorted: {:?} >= {:?}", w[0], w[1]);
            }
        }
    }

    #[test]
    fn intro_words_are_keywords() {
        for s in [&super::RUST, &super::TS, &super::GO, &super::C, &super::CPP, &super::JAVA, &super::CSHARP, &super::PYTHON] {
            for w in s.fn_intro.iter().chain(s.type_intro) {
                assert!(s.keywords.contains(w), "{w}");
            }
        }
    }
}
