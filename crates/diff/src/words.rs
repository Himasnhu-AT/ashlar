//! Word-level changes between a removed line and the added line it pairs with.

use std::ops::Range;

use crate::myers::{Op, diff_ops};

/// Lines longer than this (bytes) get no word-level emphasis.
pub const MAX_WORD_DIFF_LINE: usize = 4096;

/// Cost cap for one word diff.
const WORD_COST: usize = 256;

/// Splits a line into words (letters, digits, `_`), runs of whitespace, and single other chars.
fn tokens(s: &str) -> Vec<Range<usize>> {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Word,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last: Option<Class> = None;
    for (i, c) in s.char_indices() {
        let cl = class(c);
        let end = i + c.len_utf8();
        match out.last_mut() {
            Some(r) if last == Some(cl) && cl != Class::Other => r.end = end,
            _ => out.push(i..end),
        }
        last = Some(cl);
    }
    out
}

/// The byte ranges that changed between `old` and `new`, word by word: `(in old, in new)`.
/// Adjacent changed tokens merge into one range. `None` when either line is too long, the
/// lines are equal, or they share no word (emphasis would then cover everything and say
/// nothing).
///
/// ```
/// let (old, new) = ashlar_diff::word_diff("let x = 1;", "let y = 1;").unwrap();
/// assert_eq!(old, vec![4..5]);
/// assert_eq!(new, vec![4..5]);
/// ```
pub fn word_diff(old: &str, new: &str) -> Option<(Vec<Range<usize>>, Vec<Range<usize>>)> {
    if old.len() > MAX_WORD_DIFF_LINE || new.len() > MAX_WORD_DIFF_LINE || old == new {
        return None;
    }
    let (ta, tb) = (tokens(old), tokens(new));
    let wa: Vec<&str> = ta.iter().map(|r| old.get(r.clone()).unwrap_or("")).collect();
    let wb: Vec<&str> = tb.iter().map(|r| new.get(r.clone()).unwrap_or("")).collect();
    let edit = diff_ops(&wa, &wb, WORD_COST);
    let shares_word = edit.ops.iter().any(|op| match op {
        Op::Equal { old: o, len, .. } => wa.iter().skip(*o).take(*len).any(|w| !w.trim().is_empty()),
        _ => false,
    });
    if edit.coarse || !shares_word {
        return None;
    }
    let mut ra = Vec::new();
    let mut rb = Vec::new();
    let span = |toks: &[Range<usize>], at: usize, len: usize| -> Option<Range<usize>> {
        let first = toks.get(at)?;
        let last = toks.get(at + len.checked_sub(1)?)?;
        Some(first.start..last.end)
    };
    for op in &edit.ops {
        match *op {
            Op::Equal { .. } => {}
            Op::Delete { old: o, len } => ra.extend(span(&ta, o, len)),
            Op::Insert { new: n, len } => rb.extend(span(&tb, n, len)),
        }
    }
    Some((merge(ra), merge(rb)))
}

fn merge(mut v: Vec<Range<usize>>) -> Vec<Range<usize>> {
    v.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(l) if r.start <= l.end => l.end = l.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_split_words_spaces_and_punctuation() {
        let s = "foo_bar(x,  y)";
        let words: Vec<&str> = tokens(s).into_iter().map(|r| &s[r]).collect();
        assert_eq!(words, ["foo_bar", "(", "x", ",", "  ", "y", ")"]);
    }

    #[test]
    fn emphasis_marks_changed_words() {
        let (a, b) = word_diff("fn add(a: i32) -> i32", "fn add(a: i64) -> i64").unwrap();
        assert_eq!(a, vec![10..13, 18..21]);
        assert_eq!(b, vec![10..13, 18..21]);
        assert!(word_diff("abc", "xyz").is_none());
        assert!(word_diff("same", "same").is_none());
        let (a, b) = word_diff("é x", "é y").unwrap();
        assert_eq!((a.first().cloned(), b.first().cloned()), (Some(3..4), Some(3..4)));
    }
}
