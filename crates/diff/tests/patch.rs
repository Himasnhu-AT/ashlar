#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Patch parsing: round trips on real `git diff` output, malformed input never panics, and
//! `from_texts` → patch → parse agrees with itself.

use ashlar_diff::{FileDiff, FileStatus, LineKind, parse_patch, write_patch};

const FIXTURES: &[(&str, &str)] = &[
    ("git_mixed", include_str!("fixtures/git_mixed.patch")),
    ("git_binary_data", include_str!("fixtures/git_binary_data.patch")),
    ("git_zero_context", include_str!("fixtures/git_zero_context.patch")),
    ("git_function_context", include_str!("fixtures/git_function_context.patch")),
    ("plain_unified", include_str!("fixtures/plain_unified.patch")),
];

/// A tiny xorshift PRNG: deterministic, no dependencies.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[test]
fn fixtures_round_trip_byte_for_byte() {
    for (name, text) in FIXTURES {
        let files = parse_patch(text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!files.is_empty(), "{name}");
        assert_eq!(&write_patch(&files), text, "{name}");
    }
}

#[test]
fn git_headers_are_understood() {
    let files = parse_patch(FIXTURES[0].1).unwrap();
    let by_path = |p: &str| files.iter().find(|f| f.path() == p).unwrap_or_else(|| panic!("no {p}"));
    assert_eq!(by_path("added.txt").status, FileStatus::Added);
    assert_eq!(by_path("added.txt").old_path, None);
    assert_eq!(by_path("old.txt").status, FileStatus::Deleted);
    assert_eq!(by_path("old.txt").new_path, None);
    let moved = by_path("moved.txt");
    assert_eq!(moved.status, FileStatus::Renamed);
    assert_eq!(moved.old_path.as_deref(), Some("move_me.txt"));
    assert_eq!(moved.similarity, Some(94));
    assert_eq!(moved.hunks[0].section, "line 11");
    assert!(by_path("blob.bin").binary);
    assert!(by_path("blob.bin").hunks.is_empty());
    let script = by_path("script.sh");
    assert_eq!((script.old_mode.as_deref(), script.new_mode.as_deref()), (Some("100644"), Some("100755")));
    assert!(by_path("täb.txt").hunks.len() == 1);
    assert_eq!(by_path("with space.txt").old_path.as_deref(), Some("with space.txt"));
    let nonl = by_path("nonl.txt");
    let removed = nonl.hunks[0].lines.iter().find(|l| l.kind == LineKind::Removed).unwrap();
    assert!(removed.no_newline);
    // word-level emphasis on the paired lines
    let main = by_path("main.rs");
    let first_removed = main.hunks[0].lines.iter().find(|l| l.kind == LineKind::Removed).unwrap();
    assert_eq!(first_removed.text, "    let x = 1;");
    assert_eq!(first_removed.emphasis, vec![8..9, 12..13]);
    assert_eq!(main.stats(), (2, 2));

    let bin = parse_patch(FIXTURES[1].1).unwrap();
    assert!(bin[0].binary && bin[0].header.iter().any(|l| l.starts_with("literal")));
    let plain = parse_patch(FIXTURES[4].1).unwrap();
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0].old_path.as_deref(), Some("numbers.txt.orig"));
    assert_eq!(plain[0].hunks.len(), 2);
    // a parsed patch does not know the file's text: gaps are listed but not expandable
    let gaps = plain[0].gaps();
    assert_eq!((gaps[0].len, gaps[0].expandable), (1, false));
    assert_eq!(gaps[1].len, 18);
}

#[test]
fn mutated_patches_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for (_, text) in FIXTURES {
        // every truncation
        for cut in (0..=text.len()).filter(|i| text.is_char_boundary(*i)) {
            let _ = parse_patch(&text[..cut]);
        }
        // random byte edits, line drops and duplications
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        for _ in 0..2000 {
            let mut bytes = text.as_bytes().to_vec();
            for _ in 0..1 + rng.below(4) {
                let at = rng.below(bytes.len());
                let pick = [b' ', b'-', b'+', b'\\', b'@', b',', b'9', b'\n', b'"', b'\t', 0xff];
                bytes[at] = pick[rng.below(pick.len())];
            }
            let s = String::from_utf8_lossy(&bytes);
            if let Ok(files) = parse_patch(&s) {
                let _ = write_patch(&files);
                for f in &files {
                    let _ = f.gaps();
                }
            }
            let mut ls = lines.clone();
            let i = rng.below(ls.len());
            if rng.below(2) == 0 {
                ls.remove(i);
            } else {
                ls.insert(i, ls[i]);
            }
            let _ = parse_patch(&ls.concat());
        }
    }
    // hostile numbers and shapes
    for p in [
        "--- a\n+++ b\n@@ -4294967295,4294967295 +1 @@\n",
        "--- a\n+++ b\n@@ -0,0 +4294967295,1 @@\n+x\n",
        "--- a\n+++ b\n@@ -1,0 +1,0 @@\n",
        "diff --git \"a/unterminated\n",
        "diff --git \"a/\\777\\\" \"b/x\"\n--- \"a/\\\n+++ b\n",
        "@@ -1 +1 @@",
        "\\ No newline at end of file\n",
        "--- a\n+++ b\n@@ -1 +1 @@\n\\\n\\\n-a\n+b\n",
    ] {
        let _ = parse_patch(p);
    }
}

fn random_text(rng: &mut Rng, lines: usize, alphabet: usize) -> String {
    let mut s = String::new();
    for _ in 0..lines {
        s.push_str(&format!("w{} x{}\n", rng.below(alphabet), rng.below(2)));
    }
    if rng.below(4) == 0 {
        s.pop();
    }
    s
}

#[test]
fn from_texts_patches_parse_back_and_apply() {
    let mut rng = Rng(42);
    for round in 0..300 {
        let old = {
            let n = rng.below(40);
            random_text(&mut rng, n, 6)
        };
        let new = {
            let n = rng.below(40);
            random_text(&mut rng, n, 6)
        };
        let d = FileDiff::from_texts(&old, &new).with_paths("f.txt", "f.txt");
        let patch = d.to_patch();
        let back = parse_patch(&patch).unwrap_or_else(|e| panic!("round {round}: {e}\n{patch}"));
        if d.hunks.is_empty() {
            continue;
        }
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].hunks, d.hunks, "round {round}");
        assert_eq!(apply(&old, &d), new, "round {round}");
    }
}

/// Applies a diff with full context knowledge (the hunks plus the gaps between them).
fn apply(old: &str, d: &FileDiff) -> String {
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let mut out = String::new();
    let mut next_old = 1usize;
    for h in &d.hunks {
        let start = h.old_range().start as usize;
        for l in old_lines.iter().take(start - 1).skip(next_old - 1) {
            out.push_str(l);
        }
        for l in &h.lines {
            if l.kind != LineKind::Removed {
                out.push_str(&l.text);
                if !l.no_newline {
                    out.push('\n');
                }
            }
        }
        next_old = h.old_range().end as usize;
    }
    for l in old_lines.iter().skip(next_old - 1) {
        out.push_str(l);
    }
    out
}
