# ashlar-syntax

Headless syntax highlighting for ashlar apps: which byte range of a text is a keyword, a string, a
comment…

- `highlight(lang, text)`: ordered, non-overlapping, non-empty spans on `char` boundaries that
  cover the whole input. An unknown language gives one plain span.
- `highlight_line(lang, line, state, out)`: one line at a time. Each line returns the `State` the
  next one starts in (inside a block comment, a raw string, a heredoc…), so a view re-highlights
  from an edit only until the states line up again.
- `lang_for_path("src/main.rs") == Some("rust")`, `Lang::from_name("tsx")`.
- `Highlighter`: the trait views use; `Builtin` is this crate's lexers, and a bigger engine can be
  plugged in later behind the same trait.
- `Kind`: keyword, type, function, string, comment, number, constant, punctuation, operator,
  attribute, property, variable, tag, heading, emphasis, link, inserted, deleted, meta, plain.

Languages: Rust, TypeScript/JavaScript, Go, C, C++, Java, C#, Python, shell, SQL, JSON (with
comments), TOML, YAML, HTML (with `<script>` and `<style>` highlighted inside), CSS, Markdown
(fenced code highlighted in its language) and diffs.

```rust
use ashlar_syntax::{highlight, Kind};

for (range, kind) in highlight(Some("rust"), "let n = 42; // answer") {
    if kind == Kind::Comment {
        assert_eq!(range, 12..21);
    }
}
```

Every lexer is hand-written, reads each line forward once and never backtracks past a token it
gave up on, so time is linear in the input (property-tested, including adversarial 512 KiB lines)
and hostile input never panics. About 0.5 ms per 1,000 lines of Rust in a release build.

No dependencies, no UI; builds for `wasm32-unknown-unknown`. Colours and the code view live in
`ashlar-ui` (`code_view`).
