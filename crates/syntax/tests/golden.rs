#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Per-language token checks on small snippets: each needle (its first occurrence, or the n-th
//! with `needle@n`) must lie inside one span of the expected kind.

use ashlar_syntax::{Kind, Lang, State, highlight, highlight_line, lang_for_path};

fn kind_at(lang: &str, src: &str, needle: &str) -> Kind {
    let (needle, nth) = match needle.rsplit_once('@') {
        Some((n, k)) if k.chars().all(|c| c.is_ascii_digit()) && !k.is_empty() => (n, k.parse::<usize>().unwrap()),
        _ => (needle, 1),
    };
    let at = src.match_indices(needle).nth(nth - 1).unwrap_or_else(|| panic!("{needle:?} not in {src:?}")).0;
    let spans = highlight(Some(lang), src);
    let (range, kind) = spans.iter().find(|(r, _)| r.contains(&at)).unwrap();
    assert!(range.end >= at + needle.len(), "{lang}: {needle:?} split: {:?}", spans_text(src, &spans));
    *kind
}

fn spans_text<'a>(src: &'a str, spans: &[(std::ops::Range<usize>, Kind)]) -> Vec<(&'a str, &'static str)> {
    spans.iter().map(|(r, k)| (&src[r.clone()], k.name())).collect()
}

fn check(lang: &str, src: &str, want: &[(&str, Kind)]) {
    for (needle, kind) in want {
        let got = kind_at(lang, src, needle);
        assert_eq!(got, *kind, "{lang}: {needle:?} in {:?}", spans_text(src, &highlight(Some(lang), src)));
    }
}

use Kind::*;

#[test]
fn rust() {
    let src = r##"#[derive(Debug)]
pub struct Point<'a> { x: u32, name: &'a str }
/* outer /* nested */ still */
fn main() {
    let s = r#"raw "quoted""#; // done
    let c = 'x';
    let b = b"bytes";
    println!("{}", MAX_LEN + 0x1F + 1.5e-3);
    let none: Option<Vec<u8>> = None;
    x.method();
}"##;
    check(
        "rust",
        src,
        &[
            ("#[derive(Debug)]", Attribute),
            ("pub", Keyword),
            ("Point", Type),
            ("'a", Variable),
            ("u32", Type),
            ("/* outer /* nested */ still */", Comment),
            ("main", Function),
            ("r#\"raw \"quoted\"\"#", String),
            ("// done", Comment),
            ("'x'", String),
            ("b\"bytes\"", String),
            ("println!", Function),
            ("MAX_LEN", Constant),
            ("0x1F", Number),
            ("1.5e-3", Number),
            ("Option", Type),
            ("None", Constant),
            ("method", Function),
            ("=", Operator),
            ("{", Punctuation),
        ],
    );
}

#[test]
fn rust_multiline_string_and_comment() {
    let src = "let s = \"one\ntwo\";\n/* a\nb */ let";
    check("rust", src, &[("two", String), ("b */", Comment), ("let@2", Keyword)]);
}

#[test]
fn typescript() {
    let src = r#"import { x } from "mod";
@Component({})
export class Foo extends Bar implements Baz {
  private n: number = 42;
  greet(name: string): void { return `hi ${name.trim()} and ${ {a: 1}.a }!`; }
}
const re = /ab+c/gi, half = total / 2;
const tpl = `line one
line two ${value}`;
let u = undefined;"#;
    check(
        "ts",
        src,
        &[
            ("import", Keyword),
            ("\"mod\"", String),
            ("@Component", Attribute),
            ("Foo", Type),
            ("Bar", Type),
            ("number", Type),
            ("42", Number),
            ("greet", Function),
            ("`hi ", String),
            ("trim", Function),
            ("!`", String),
            ("/ab+c/gi", String),
            ("/@3", Operator),
            ("line two ", String),
            ("value", Plain),
            ("undefined", Constant),
        ],
    );
}

#[test]
fn json() {
    let src = "{\n  \"name\": \"ashlar\", // jsonc\n  \"n\": -1.5e3, \"ok\": true, \"list\": [null]\n}";
    check(
        "json",
        src,
        &[("\"name\"", Property), ("\"ashlar\"", String), ("// jsonc", Comment), ("-1.5e3", Number), ("true", Constant), ("null", Constant)],
    );
}

#[test]
fn toml() {
    let src = "[package]\nname = \"ashlar\" # comment\nversion.workspace = true\n[[bin]]\ndesc = \"\"\"\nmulti\n\"\"\"\nn = 1_000\nwhen = 1979-05-27T07:32:00Z\n";
    check(
        "toml",
        src,
        &[
            ("package", Type),
            ("name", Property),
            ("\"ashlar\"", String),
            ("# comment", Comment),
            ("version", Property),
            ("workspace", Property),
            ("true", Constant),
            ("bin", Type),
            ("multi", String),
            ("1_000", Number),
            ("1979-05-27T07:32:00Z", Number),
        ],
    );
}

#[test]
fn yaml() {
    let src = "---\nname: ashlar # c\nlist:\n  - one\n  - 2\n  - true\nanchor: &a x\nref: *a\nscript: |\n  echo hi\n  echo there\nafter: \"q\"\n";
    check(
        "yaml",
        src,
        &[
            ("---", Meta),
            ("name", Property),
            ("ashlar", String),
            ("# c", Comment),
            ("one", String),
            ("2", Number),
            ("true", Constant),
            ("&a", Variable),
            ("*a", Variable),
            ("echo there", String),
            ("after", Property),
            ("\"q\"", String),
        ],
    );
}

#[test]
fn python() {
    let src = "@dataclass\nclass Foo(Base):\n    def run(self, n: int) -> None:\n        \"\"\"Doc\n        string\"\"\"\n        s = f'{n}' + rb\"x\"  # c\n        return MAX * 2\n";
    check(
        "python",
        src,
        &[
            ("@dataclass", Attribute),
            ("class@2", Keyword),
            ("Foo", Type),
            ("run", Function),
            ("self", Keyword),
            ("int", Type),
            ("None", Constant),
            ("string\"\"\"", String),
            ("f'{n}'", String),
            ("rb\"x\"", String),
            ("# c", Comment),
            ("MAX", Constant),
        ],
    );
}

#[test]
fn shell() {
    let src = "#!/bin/sh\nset -e\nNAME=\"world $USER\"\nif [ -n \"$1\" ]; then\n  echo 'hi' | grep -q h && ls ${HOME}/x\nfi\ncat <<-EOF\n\tbody $x\n\tEOF\nmain() { :; }\necho done";
    check(
        "sh",
        src,
        &[
            ("#!/bin/sh", Comment),
            ("set", Keyword),
            ("-e", Attribute),
            ("NAME", Variable),
            ("\"world ", String),
            ("$USER", Variable),
            ("if", Keyword),
            ("then", Keyword),
            ("echo", Keyword),
            ("'hi'", String),
            ("|", Operator),
            ("grep", Function),
            ("${HOME}", Variable),
            ("body $x", String),
            ("\tEOF", Keyword),
            ("main", Function),
            ("echo@2", Keyword),
        ],
    );
}

#[test]
fn diff() {
    let src =
        "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@ fn main\n context\n-old\n+new\n\\ No newline at end of file\n";
    check(
        "diff",
        src,
        &[
            ("diff --git", Meta),
            ("--- a/x", Meta),
            ("@@ -1,2 +1,2 @@", Meta),
            ("fn main", Plain),
            ("-old", Deleted),
            ("+new", Inserted),
            ("\\ No newline", Comment),
        ],
    );
}

#[test]
fn sql() {
    let src = "SELECT id, COUNT(*) FROM users -- c\nWHERE name = 'a''b' AND age > 21 AND x = $1 /* multi\nline */ LIMIT 10;";
    check(
        "sql",
        src,
        &[
            ("SELECT", Keyword),
            ("COUNT", Function),
            ("users", Plain),
            ("-- c", Comment),
            ("'a''b'", String),
            ("21", Number),
            ("$1", Variable),
            ("line */", Comment),
            ("LIMIT", Keyword),
        ],
    );
}

#[test]
fn go() {
    let src = "package main\nfunc Add(a int) error {\n\ts := `raw\nstring`\n\treturn nil\n}";
    check("go", src, &[("package", Keyword), ("Add", Function), ("int", Type), ("error", Type), ("string`", String), ("nil", Constant)]);
}

#[test]
fn c_and_cpp() {
    let c = "#include <stdio.h>\nint main(void) { return NULL; }";
    check("c", c, &[("#include", Attribute), ("<stdio.h>", String), ("int", Type), ("main", Function), ("NULL", Constant)]);
    let cpp = "template <typename T> class Box { std::vector<T> v; };\nauto p = nullptr;";
    check("cpp", cpp, &[("template", Keyword), ("Box", Type), ("nullptr", Constant)]);
}

#[test]
fn java_and_csharp() {
    check("java", "@Override public String toString() { return null; }", &[("@Override", Attribute), ("String", Type), ("toString", Function)]);
    check("cs", "public record Point(int X) { }", &[("record", Keyword), ("Point", Type), ("int", Type)]);
}

#[test]
fn html_with_script_and_style() {
    let src = "<!DOCTYPE html>\n<div class=\"a\" id=x>Hi &amp; bye<!-- c --></div>\n<script>\nconst n = 1; // js\n</script>\n<style>.a { color: #fff; }</style>";
    check(
        "html",
        src,
        &[
            ("<!DOCTYPE html>", Meta),
            ("div", Tag),
            ("class", Attribute),
            ("\"a\"", String),
            ("x", String),
            ("&amp;", Constant),
            ("<!-- c -->", Comment),
            ("const", Keyword),
            ("// js", Comment),
            ("script@2", Tag),
            (".a", Type),
            ("color", Property),
            ("#fff", Number),
        ],
    );
}

#[test]
fn css() {
    let src =
        "@media (max-width: 600px) {\n  a:hover > .btn, #main {\n    margin: 0 auto !important;\n    background: url(\"x.png\");\n  }\n}\n/* end */";
    check(
        "css",
        src,
        &[
            ("@media", Keyword),
            ("600px", Number),
            ("a@3", Tag),
            (":hover", Attribute),
            (".btn", Type),
            ("#main", Constant),
            ("margin", Property),
            ("auto", Constant),
            ("!important", Keyword),
            ("url", Function),
            ("/* end */", Comment),
        ],
    );
}

#[test]
fn markdown() {
    let src = "# Title\n\nSome *em* and **strong** and `code` and [a link](http://x).\n\n- [ ] task\n> quote\n\n```rust\nfn main() {}\n```\n\n```\nplain\n```\n";
    check(
        "md",
        src,
        &[
            ("# Title", Heading),
            ("*em*", Emphasis),
            ("**strong**", Emphasis),
            ("`code`", String),
            ("[a link]", Link),
            ("(http://x)", String),
            ("-", Punctuation),
            ("[ ]", Punctuation),
            (">", Punctuation),
            ("```", Punctuation),
            ("rust", Type),
            ("fn", Keyword),
            ("main", Function),
            ("plain", String),
        ],
    );
}

#[test]
fn unknown_language_is_one_plain_span() {
    assert_eq!(highlight(Some("klingon"), "abc"), vec![(0..3, Plain)]);
    assert_eq!(highlight(None, "abc\ndef"), vec![(0..7, Plain)]);
    assert!(highlight(Some("rust"), "").is_empty());
}

#[test]
fn paths_and_names() {
    assert_eq!(lang_for_path("src/main.rs"), Some("rust"));
    assert_eq!(lang_for_path("C:\\a\\B.TSX"), Some("typescript"));
    assert_eq!(lang_for_path("/etc/x.yml"), Some("yaml"));
    assert_eq!(lang_for_path("Cargo.lock"), Some("toml"));
    assert_eq!(lang_for_path("~/.bashrc"), Some("shell"));
    assert_eq!(lang_for_path("README"), None);
    assert_eq!(lang_for_path(".rs"), None);
    assert_eq!(lang_for_path("x.unknown"), None);
    for lang in Lang::ALL {
        assert_eq!(Lang::from_name(lang.name()), Some(lang));
    }
    assert_eq!(Lang::from_name(" Python "), Some(Lang::Python));
}

#[test]
fn line_states_resume_across_lines() {
    let mut out = Vec::new();
    let s1 = highlight_line(Lang::Python, "x = '''start", State::START, &mut out);
    assert_ne!(s1, State::START);
    out.clear();
    let s2 = highlight_line(Lang::Python, "middle", s1, &mut out);
    assert_eq!(out, vec![(0..6, String)]);
    out.clear();
    let s3 = highlight_line(Lang::Python, "end''' + 1\n", s2, &mut out);
    assert_eq!(s3, State::START);
    assert_eq!(out.last(), Some(&(10..11, Plain)));
    assert_eq!(State::from_bits(s1.to_bits()), s1);
}
