#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Property tests with a tiny deterministic PRNG: hostile input never panics, spans always obey
//! the `Span` rules, line-by-line highlighting matches whole-text highlighting, and time grows
//! linearly with input size.

use std::time::Duration;

use ashlar_syntax::{Builtin, Highlighter, Kind, Lang, Span, State, highlight, highlight_into, highlight_line};

/// xorshift64*: deterministic, no dependencies.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Pieces that open and close every construct the lexers track, plus unicode and junk.
const PIECES: &[&str] = &[
    "/*",
    "*/",
    "//",
    "\"",
    "'",
    "`",
    "${",
    "}",
    "{",
    "\\",
    "\n",
    "\r\n",
    "#",
    "#[",
    "<<EOF",
    "<<-X",
    "EOF",
    "\tX",
    "<!--",
    "-->",
    "<script>",
    "</script>",
    "<style>",
    "</style>",
    "```rust",
    "```",
    "~~~",
    "r#\"",
    "\"#",
    "b'",
    "'''",
    "\"\"\"",
    "|",
    "|\n  x",
    ": ",
    "- ",
    "*",
    "**",
    "_",
    "[",
    "](",
    ")",
    "@@ ",
    "+",
    "-",
    "fn ",
    "let ",
    "def ",
    "é",
    "😀",
    "\u{0301}",
    "日本",
    " ",
    "\t",
    "0x1F",
    "1.5e-3",
    "$x",
    "$(",
    "@a",
    "/re/",
    "<a b=\"c\">",
    "&amp;",
    "--",
    "a:",
    "&a",
    "*a",
    "!t",
    ">",
    "=",
    ";",
    ",",
    ".",
    "x",
    "Y_Z",
    "\0",
    "\u{7f}",
];

fn random_text(rng: &mut Rng, pieces: usize) -> String {
    let mut s = String::new();
    for _ in 0..pieces {
        if rng.below(10) == 0 {
            // a random char anywhere in the unicode range
            let c = char::from_u32((rng.next() % 0x11_0000) as u32).unwrap_or('?');
            s.push(c);
        } else {
            s.push_str(PIECES[rng.below(PIECES.len())]);
        }
    }
    s
}

fn check_spans(text: &str, spans: &[Span], ctx: &str) {
    let mut at = 0;
    for (i, (range, kind)) in spans.iter().enumerate() {
        assert_eq!(range.start, at, "{ctx}: gap or overlap at span {i}");
        assert!(range.end > range.start, "{ctx}: empty span {i}");
        assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end), "{ctx}: span {i} off a char boundary");
        if let Some((_, prev)) = i.checked_sub(1).and_then(|p| spans.get(p)) {
            assert_ne!(prev, kind, "{ctx}: span {i} not merged with its neighbour");
        }
        at = range.end;
    }
    assert_eq!(at, text.len(), "{ctx}: spans don't cover the input");
}

/// Whole-text highlighting rebuilt from `highlight_line` with carried states.
fn by_lines(lang: Lang, text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut state = State::START;
    let mut base = 0;
    let mut line_spans = Vec::new();
    for line in text.split_inclusive('\n') {
        line_spans.clear();
        state = highlight_line(lang, line, state, &mut line_spans);
        check_spans(line, &line_spans, "line");
        for (r, k) in line_spans.drain(..) {
            let r = r.start + base..r.end + base;
            match out.last_mut() {
                Some((last, lk)) if *lk == k && last.end == r.start => last.end = r.end,
                _ => out.push((r, k)),
            }
        }
        base += line.len();
    }
    out
}

#[test]
fn random_input_never_panics_and_spans_are_well_formed() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..400 {
        let pieces = 1 + rng.below(80);
        let text = random_text(&mut rng, pieces);
        for lang in Lang::ALL {
            let ctx = format!("{} round {round}: {text:?}", lang.name());
            let spans = highlight(Some(lang.name()), &text);
            check_spans(&text, &spans, &ctx);
            assert_eq!(by_lines(lang, &text), spans, "{ctx}: line-by-line differs");
        }
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(42);
    for _ in 0..300 {
        let len = rng.below(200);
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        let text = String::from_utf8_lossy(&bytes);
        for lang in Lang::ALL {
            check_spans(&text, &highlight(Some(lang.name()), &text), lang.name());
        }
    }
}

#[test]
fn hostile_states_never_panic() {
    // a view may hand back a state from another language or a corrupted cache
    let mut rng = Rng(7);
    for _ in 0..2000 {
        let state = State::from_bits(rng.next());
        let line = random_text(&mut rng, 6);
        for lang in Lang::ALL {
            let mut out = Vec::new();
            let _ = highlight_line(lang, &line, state, &mut out);
            check_spans(&line, &out, lang.name());
        }
    }
}

#[test]
fn builtin_highlighter_matches_functions() {
    let text = "fn a() {}\n/* x\n*/ let y = \"z\";\n";
    let mut via_trait = Vec::new();
    Builtin.highlight("rust", text, &mut via_trait);
    assert_eq!(via_trait, highlight(Some("rust"), text));
    assert!(Builtin.supports("rs") && !Builtin.supports("cobol"));
    // the trait's default `highlight` (line by line) agrees too
    struct Lines;
    impl Highlighter for Lines {
        fn supports(&self, lang: &str) -> bool {
            Builtin.supports(lang)
        }
        fn highlight_line(&self, lang: &str, line: &str, state: State, out: &mut Vec<Span>) -> State {
            Builtin.highlight_line(lang, line, state, out)
        }
    }
    let mut via_default = Vec::new();
    Lines.highlight("rust", text, &mut via_default);
    assert_eq!(via_default, via_trait);
    let mut plain = Vec::new();
    let _ = Builtin.highlight_line("cobol", "abc", State::START, &mut plain);
    assert_eq!(plain, vec![(0..3, Kind::Plain)]);
}

/// Inputs built to make a careless lexer quadratic: long lines of openers that never close.
fn adversarial(unit: &str, bytes: usize, newline_every: usize) -> String {
    let mut s = String::with_capacity(bytes + unit.len());
    let mut since = 0;
    while s.len() < bytes {
        s.push_str(unit);
        since += 1;
        if newline_every > 0 && since % newline_every == 0 {
            s.push('\n');
        }
    }
    s
}

/// The best of five runs, in this thread's CPU time: wall-clock time also counts the time other
/// processes hold the core, so a loaded machine made a linear lexer look quadratic (IMP-0.8).
fn time(lang: Lang, text: &str) -> Duration {
    let mut out = Vec::new();
    let mut best = Duration::MAX;
    for _ in 0..5 {
        out.clear();
        let t = cpu_time::ThreadTime::now();
        highlight_into(Some(lang), text, &mut out);
        best = best.min(t.elapsed());
    }
    best
}

#[test]
fn time_is_linear_in_input_size() {
    const UNITS: &[&str] = &[
        "/* \" ' ` ${ [ * _ ` ``` <!-- <a b=\" & $( r#\" ''' |",
        "*a _b `c [d] (e ** __ ``` ~~ ",
        "\"\\",
        "<<",
        "[",
        "`",
        "*",
        "/",
        "{",
        ": ",
        "a ",
        "😀",
    ];
    // one line 8x longer than another: a quadratic lexer takes 64x as long (smaller in debug)
    let (small, big) = if cfg!(debug_assertions) { (16 * 1024, 128 * 1024) } else { (64 * 1024, 512 * 1024) };
    for unit in UNITS {
        for lang in Lang::ALL {
            let a = adversarial(unit, small, 0);
            let b = adversarial(unit, big, 0);
            // generous: linear is ~8x; allow 24x plus timer noise. Load still slows CPU time a
            // little (shared caches, clock speed), so a pair gets three tries: a quadratic lexer
            // fails every one, noise does not repeat.
            let fits = |(ta, tb): (Duration, Duration)| tb <= ta.mul_f64(24.0) + Duration::from_millis(2);
            let mut tries = Vec::new();
            while tries.len() < 3 && !tries.last().copied().is_some_and(fits) {
                tries.push((time(lang, &a), time(lang, &b)));
            }
            assert!(tries.last().copied().is_some_and(fits), "{} on {unit:?}: (time for {small} bytes, time for {big}) {tries:?}", lang.name());
        }
    }
}

#[test]
fn one_megabyte_of_mixed_lines_is_fast() {
    let mut rng = Rng(1234);
    let mut text = String::new();
    while text.len() < 1024 * 1024 {
        text.push_str(&random_text(&mut rng, 40));
        text.push('\n');
    }
    for lang in Lang::ALL {
        let t = time(lang, &text);
        // a release build takes a few ms; a debug build well under a second
        let limit = if cfg!(debug_assertions) { Duration::from_secs(5) } else { Duration::from_millis(500) };
        assert!(t < limit, "{}: {t:?} for 1 MiB", lang.name());
    }
}
