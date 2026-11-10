//! Headless syntax highlighting for ashlar: small, forward-only, linear-time lexers for common
//! languages, and a line-state API so a view can re-highlight only the lines that changed.
//!
//! No dependencies, no UI; builds for `wasm32-unknown-unknown`. Colours live in the UI layer
//! (`ashlar_ui::code_view::SyntaxTheme`); this crate only says which byte range is which [`Kind`].
//!
//! ```
//! use ashlar_syntax::{highlight, lang_for_path, Kind};
//!
//! let lang = lang_for_path("src/main.rs");
//! assert_eq!(lang, Some("rust"));
//! let spans = highlight(lang, "fn main() { let x = 42; }");
//! assert_eq!(spans.first(), Some(&(0..2, Kind::Keyword)));
//! // ordered, non-overlapping and covering the whole input
//! assert_eq!(spans.last().map(|(r, _)| r.end), Some(25));
//! ```
//!
//! Incremental use: keep the [`State`] each line starts in. After an edit, re-highlight from the
//! edited line and stop as soon as a line ends in the same state it ended in before.
//!
//! ```
//! use ashlar_syntax::{highlight_line, Lang, State};
//!
//! let mut spans = Vec::new();
//! let after_first = highlight_line(Lang::Rust, "let s = /* open", State::START, &mut spans);
//! assert_ne!(after_first, State::START); // inside a block comment
//! spans.clear();
//! let after_second = highlight_line(Lang::Rust, "still a comment */ x", after_first, &mut spans);
//! assert_eq!(after_second, State::START);
//! ```

mod lang;
mod scan;

use std::ops::Range;

/// What a span of source text is, for colouring.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Kind {
    /// Anything without a colour of its own: identifiers, whitespace, text.
    Plain,
    /// Language keywords (`fn`, `if`, `return`, `SELECT`).
    Keyword,
    /// Type names (`u32`, `String`, a class name, a TOML table header).
    Type,
    /// Function and macro names at a call or a definition.
    Function,
    /// String and character literals, heredocs, YAML plain values, inline code.
    String,
    /// Comments.
    Comment,
    /// Numeric literals (and CSS colours and dimensions).
    Number,
    /// Named constants: `true`, `null`, `None`, `ALL_CAPS` names, HTML entities.
    Constant,
    /// Brackets, separators and similar.
    Punctuation,
    /// Operators (`+`, `&&`, `=`, `|`).
    Operator,
    /// Attributes, annotations, decorators, preprocessor lines, HTML attribute names.
    Attribute,
    /// Keys and properties: JSON/TOML/YAML keys, CSS properties.
    Property,
    /// Variables with a sigil or a role: `$HOME`, lifetimes, YAML anchors, SQL parameters.
    Variable,
    /// Markup tag names (`div`, a CSS element selector).
    Tag,
    /// Markdown headings.
    Heading,
    /// Markdown emphasis and strong emphasis.
    Emphasis,
    /// Markdown links and autolinks.
    Link,
    /// Added lines in a diff.
    Inserted,
    /// Removed lines in a diff.
    Deleted,
    /// Meta lines: diff headers and hunk markers, front matter fences, doctype.
    Meta,
}

impl Kind {
    /// Every kind, in declaration order.
    pub const ALL: [Kind; 20] = [
        Kind::Plain,
        Kind::Keyword,
        Kind::Type,
        Kind::Function,
        Kind::String,
        Kind::Comment,
        Kind::Number,
        Kind::Constant,
        Kind::Punctuation,
        Kind::Operator,
        Kind::Attribute,
        Kind::Property,
        Kind::Variable,
        Kind::Tag,
        Kind::Heading,
        Kind::Emphasis,
        Kind::Link,
        Kind::Inserted,
        Kind::Deleted,
        Kind::Meta,
    ];

    /// A stable lowercase name (`"keyword"`), for themes and debugging.
    pub fn name(self) -> &'static str {
        match self {
            Kind::Plain => "plain",
            Kind::Keyword => "keyword",
            Kind::Type => "type",
            Kind::Function => "function",
            Kind::String => "string",
            Kind::Comment => "comment",
            Kind::Number => "number",
            Kind::Constant => "constant",
            Kind::Punctuation => "punctuation",
            Kind::Operator => "operator",
            Kind::Attribute => "attribute",
            Kind::Property => "property",
            Kind::Variable => "variable",
            Kind::Tag => "tag",
            Kind::Heading => "heading",
            Kind::Emphasis => "emphasis",
            Kind::Link => "link",
            Kind::Inserted => "inserted",
            Kind::Deleted => "deleted",
            Kind::Meta => "meta",
        }
    }
}

/// A highlighted byte range. Ranges from this crate are ordered, non-overlapping, non-empty, on
/// `char` boundaries, cover the input, and neighbours never share a kind.
pub type Span = (Range<usize>, Kind);

/// The lexer state at the start of a line: whether it begins inside a block comment, a
/// multi-line string, a heredoc… Opaque, `Copy` and cheap to compare; [`State::START`] is the
/// state at the top of a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct State {
    pub(crate) mode: u8,
    pub(crate) n: u16,
    pub(crate) h: u32,
}

impl State {
    /// The state at the start of a file.
    pub const START: State = State { mode: 0, n: 0, h: 0 };

    /// Packs the state into a `u64`, for storage or for another [`Highlighter`] to carry its own.
    pub fn to_bits(self) -> u64 {
        u64::from(self.mode) | (u64::from(self.n) << 8) | (u64::from(self.h) << 32)
    }

    /// The inverse of [`State::to_bits`].
    pub fn from_bits(bits: u64) -> State {
        State { mode: (bits & 0xff) as u8, n: ((bits >> 8) & 0xffff) as u16, h: (bits >> 32) as u32 }
    }

    /// The state without its 32-bit payload, packed into 24 bits (used to nest a language inside
    /// another one, such as script in HTML or a fenced block in Markdown).
    pub(crate) fn pack24(self) -> u32 {
        u32::from(self.mode) | (u32::from(self.n) << 8)
    }

    pub(crate) fn unpack24(v: u32) -> State {
        State { mode: (v & 0xff) as u8, n: ((v >> 8) & 0xffff) as u16, h: 0 }
    }
}

/// A language the built-in lexers know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Lang {
    Rust,
    TypeScript,
    JavaScript,
    Go,
    C,
    Cpp,
    Java,
    CSharp,
    Python,
    Shell,
    Sql,
    Json,
    Toml,
    Yaml,
    Html,
    Css,
    Markdown,
    Diff,
}

/// Names and aliases accepted by [`Lang::from_name`] (ASCII case-insensitive).
const ALIASES: &[(&str, Lang)] = &[
    ("rust", Lang::Rust),
    ("rs", Lang::Rust),
    ("typescript", Lang::TypeScript),
    ("ts", Lang::TypeScript),
    ("tsx", Lang::TypeScript),
    ("mts", Lang::TypeScript),
    ("cts", Lang::TypeScript),
    ("javascript", Lang::JavaScript),
    ("js", Lang::JavaScript),
    ("jsx", Lang::JavaScript),
    ("mjs", Lang::JavaScript),
    ("cjs", Lang::JavaScript),
    ("go", Lang::Go),
    ("golang", Lang::Go),
    ("c", Lang::C),
    ("h", Lang::C),
    ("cpp", Lang::Cpp),
    ("c++", Lang::Cpp),
    ("cc", Lang::Cpp),
    ("cxx", Lang::Cpp),
    ("hpp", Lang::Cpp),
    ("hh", Lang::Cpp),
    ("hxx", Lang::Cpp),
    ("java", Lang::Java),
    ("csharp", Lang::CSharp),
    ("c#", Lang::CSharp),
    ("cs", Lang::CSharp),
    ("python", Lang::Python),
    ("py", Lang::Python),
    ("pyi", Lang::Python),
    ("shell", Lang::Shell),
    ("sh", Lang::Shell),
    ("bash", Lang::Shell),
    ("zsh", Lang::Shell),
    ("console", Lang::Shell),
    ("sql", Lang::Sql),
    ("json", Lang::Json),
    ("jsonc", Lang::Json),
    ("json5", Lang::Json),
    ("toml", Lang::Toml),
    ("yaml", Lang::Yaml),
    ("yml", Lang::Yaml),
    ("html", Lang::Html),
    ("htm", Lang::Html),
    ("xml", Lang::Html),
    ("svg", Lang::Html),
    ("xhtml", Lang::Html),
    ("vue", Lang::Html),
    ("css", Lang::Css),
    ("scss", Lang::Css),
    ("less", Lang::Css),
    ("markdown", Lang::Markdown),
    ("md", Lang::Markdown),
    ("mdx", Lang::Markdown),
    ("diff", Lang::Diff),
    ("patch", Lang::Diff),
];

/// Whole file names (no extension rule applies) and the language they hold.
const FILE_NAMES: &[(&str, Lang)] = &[
    (".bashrc", Lang::Shell),
    (".zshrc", Lang::Shell),
    (".profile", Lang::Shell),
    (".bash_profile", Lang::Shell),
    ("Cargo.lock", Lang::Toml),
    ("PKGBUILD", Lang::Shell),
    (".gitconfig", Lang::Toml),
];

impl Lang {
    /// Every built-in language.
    pub const ALL: [Lang; 18] = [
        Lang::Rust,
        Lang::TypeScript,
        Lang::JavaScript,
        Lang::Go,
        Lang::C,
        Lang::Cpp,
        Lang::Java,
        Lang::CSharp,
        Lang::Python,
        Lang::Shell,
        Lang::Sql,
        Lang::Json,
        Lang::Toml,
        Lang::Yaml,
        Lang::Html,
        Lang::Css,
        Lang::Markdown,
        Lang::Diff,
    ];

    /// The canonical lowercase name (`"rust"`, `"typescript"`).
    pub fn name(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::TypeScript => "typescript",
            Lang::JavaScript => "javascript",
            Lang::Go => "go",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Java => "java",
            Lang::CSharp => "csharp",
            Lang::Python => "python",
            Lang::Shell => "shell",
            Lang::Sql => "sql",
            Lang::Json => "json",
            Lang::Toml => "toml",
            Lang::Yaml => "yaml",
            Lang::Html => "html",
            Lang::Css => "css",
            Lang::Markdown => "markdown",
            Lang::Diff => "diff",
        }
    }

    /// The language for a name, alias or file extension (`"rs"`, `"Rust"`, `"tsx"`, `"yml"`),
    /// ignoring ASCII case and surrounding whitespace. Unknown names give `None`.
    pub fn from_name(name: &str) -> Option<Lang> {
        let name = name.trim();
        ALIASES.iter().find(|(alias, _)| alias.eq_ignore_ascii_case(name)).map(|(_, lang)| *lang)
    }

    /// The language for a file path, from its file name or extension (`"src/lib.rs"`,
    /// `"C:\\notes\\todo.md"`, `".bashrc"`).
    pub fn for_path(path: &str) -> Option<Lang> {
        let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
        if let Some((_, lang)) = FILE_NAMES.iter().find(|(name, _)| *name == file) {
            return Some(*lang);
        }
        let (stem, ext) = file.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        Lang::from_name(ext)
    }
}

/// The canonical language name for a file path (`"rust"` for `"src/main.rs"`), or `None`.
pub fn lang_for_path(path: &str) -> Option<&'static str> {
    Lang::for_path(path).map(Lang::name)
}

/// Highlights `text` as `lang` (a name, alias or extension; see [`Lang::from_name`]).
///
/// The spans are ordered, non-overlapping, non-empty, on `char` boundaries and cover the whole
/// input; neighbours never share a kind. An unknown or missing language gives one
/// [`Kind::Plain`] span. Runs in time linear in the input and never panics.
pub fn highlight(lang: Option<&str>, text: &str) -> Vec<Span> {
    let mut out = Vec::new();
    highlight_into(lang.and_then(Lang::from_name), text, &mut out);
    out
}

/// [`highlight`] with a known language, appending to `out` (reuse it to save allocations).
/// Offsets are relative to `text`; a span is merged into the last one of `out` when they touch
/// and share a kind, so clear `out` first unless you mean to continue it.
pub fn highlight_into(lang: Option<Lang>, text: &str, out: &mut Vec<Span>) {
    if text.is_empty() {
        return;
    }
    let Some(lang) = lang else {
        out.push((0..text.len(), Kind::Plain));
        return;
    };
    let mut state = State::START;
    let mut base = 0usize;
    for line in text.split_inclusive('\n') {
        state = line_into(lang, line, state, base, out);
        base = base.saturating_add(line.len());
    }
}

/// Highlights one line (with or without its line ending), starting in `state`, appending spans
/// relative to the line's start to `out`. Returns the state the next line starts in.
pub fn highlight_line(lang: Lang, line: &str, state: State, out: &mut Vec<Span>) -> State {
    line_into(lang, line, state, 0, out)
}

fn line_into(lang: Lang, line: &str, state: State, base: usize, out: &mut Vec<Span>) -> State {
    let body = line.strip_suffix('\n').unwrap_or(line);
    let body = body.strip_suffix('\r').unwrap_or(body);
    let mut o = scan::Out::new(out, line, base);
    let next = lang::lex(lang, body, state, &mut o);
    o.finish();
    next
}

/// A syntax highlighting engine. [`Builtin`] is the lexers of this crate; a bigger engine (a
/// grammar-based one, say) can implement this and be used by the same views.
pub trait Highlighter {
    /// Whether this engine highlights `lang` (a name, alias or extension).
    fn supports(&self, lang: &str) -> bool;

    /// Highlights one line starting in `state`, appending line-relative spans to `out`, and
    /// returns the state the next line starts in. Spans must follow the rules of [`Span`]; an
    /// engine without line states may return `state` unchanged.
    fn highlight_line(&self, lang: &str, line: &str, state: State, out: &mut Vec<Span>) -> State;

    /// Highlights a whole text, appending spans relative to `text` to `out`.
    fn highlight(&self, lang: &str, text: &str, out: &mut Vec<Span>) {
        let mut state = State::START;
        let mut base = 0usize;
        let mut line_spans = Vec::new();
        for line in text.split_inclusive('\n') {
            line_spans.clear();
            state = self.highlight_line(lang, line, state, &mut line_spans);
            for (range, kind) in line_spans.drain(..) {
                let range = range.start.saturating_add(base)..range.end.saturating_add(base);
                match out.last_mut() {
                    Some((last, k)) if *k == kind && last.end == range.start => last.end = range.end,
                    _ => out.push((range, kind)),
                }
            }
            base = base.saturating_add(line.len());
        }
    }
}

/// The built-in lexers as a [`Highlighter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Builtin;

impl Highlighter for Builtin {
    fn supports(&self, lang: &str) -> bool {
        Lang::from_name(lang).is_some()
    }

    fn highlight_line(&self, lang: &str, line: &str, state: State, out: &mut Vec<Span>) -> State {
        match Lang::from_name(lang) {
            Some(l) => highlight_line(l, line, state, out),
            None => {
                if !line.is_empty() {
                    out.push((0..line.len(), Kind::Plain));
                }
                state
            }
        }
    }

    fn highlight(&self, lang: &str, text: &str, out: &mut Vec<Span>) {
        highlight_into(Lang::from_name(lang), text, out);
    }
}
