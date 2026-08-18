//! Fuzzy matching for a command palette: the query's characters must appear in order in the
//! candidate (case-insensitive, spaces in the query ignored). The best-scoring alignment wins:
//! matches at word starts (`File Export` ← `fe`), camelCase humps and consecutive runs score
//! higher, gaps cost.

/// Longer queries never match (bounded work on hostile input).
const MAX_QUERY: usize = 64;
/// Only this many leading characters of a candidate are considered.
const MAX_CANDIDATE: usize = 512;

const SCORE_MATCH: i32 = 16;
const GAP_START: i32 = 3;
const GAP_EXTEND: i32 = 1;
const BONUS_FIRST: i32 = 8;
const BONUS_BOUNDARY: i32 = 8;
const BONUS_CAMEL: i32 = 7;
const BONUS_CONSECUTIVE: i32 = 6;
const BONUS_CASE: i32 = 1;
const NONE: i32 = i32::MIN / 4;

/// A match: higher scores are better; `positions` are the matched **char** indices of the
/// candidate (for highlighting), ascending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i32,
    pub positions: Vec<usize>,
}

fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn bonus(prev: Option<char>, c: char) -> i32 {
    match prev {
        None => BONUS_FIRST + BONUS_BOUNDARY,
        Some(p) if !p.is_alphanumeric() && c.is_alphanumeric() => BONUS_BOUNDARY,
        Some(p) if p.is_lowercase() && c.is_uppercase() => BONUS_CAMEL,
        Some(p) if p.is_alphabetic() && c.is_numeric() => BONUS_CAMEL,
        _ => 0,
    }
}

/// Score `candidate` against `query`; `None` when the query's characters do not all appear in
/// order. An empty query matches everything with score 0.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<FuzzyMatch> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(FuzzyMatch { score: 0, positions: vec![] });
    }
    if q.len() > MAX_QUERY {
        return None;
    }
    let c: Vec<char> = candidate.chars().take(MAX_CANDIDATE).collect();
    let (n, m) = (q.len(), c.len());
    if n > m {
        return None;
    }
    let ql: Vec<char> = q.iter().map(|&x| lower(x)).collect();
    let cl: Vec<char> = c.iter().map(|&x| lower(x)).collect();
    // cheap subsequence test before the n×m table
    let mut it = cl.iter();
    if !ql.iter().all(|x| it.any(|y| y == x)) {
        return None;
    }
    let gain: Vec<i32> = (0..m)
        .map(|j| {
            let ch = c.get(j).copied().unwrap_or(' ');
            let prev = j.checked_sub(1).and_then(|p| c.get(p)).copied();
            let case = if q.contains(&ch) { BONUS_CASE } else { 0 };
            SCORE_MATCH + bonus(prev, ch) + case
        })
        .collect();
    // score[i][j]: best score with query char i matched at candidate char j; from[i][j]: where
    // query char i-1 was matched on that best path
    let mut score = vec![vec![NONE; m]; n];
    let mut from = vec![vec![usize::MAX; m]; n];
    for i in 0..n {
        let qi = ql.get(i).copied().unwrap_or_default();
        let (prev_rows, rest) = score.split_at_mut(i);
        let prev = prev_rows.last();
        let Some(row) = rest.first_mut() else { break };
        let Some(from_row) = from.get_mut(i) else { break };
        // best over k <= j-2 of prev[k] minus the gap between k and j
        let (mut run, mut run_k) = (NONE, usize::MAX);
        for j in 0..m {
            if let Some(prev) = prev
                && j >= 2
            {
                let decayed = if run > NONE { run - GAP_EXTEND } else { NONE };
                let fresh = prev.get(j - 2).copied().filter(|v| *v > NONE).map_or(NONE, |v| v - GAP_START);
                if fresh >= decayed {
                    (run, run_k) = (fresh, j - 2);
                } else {
                    run = decayed;
                }
            }
            if cl.get(j) != Some(&qi) {
                continue;
            }
            let g = gain.get(j).copied().unwrap_or(SCORE_MATCH);
            match prev {
                None => {
                    if let Some(cell) = row.get_mut(j) {
                        *cell = g;
                    }
                }
                Some(prev) => {
                    let diag = j.checked_sub(1).and_then(|k| prev.get(k)).copied().filter(|v| *v > NONE).map(|v| (v + BONUS_CONSECUTIVE, j - 1));
                    let gap = (run > NONE).then_some((run, run_k));
                    let best = match (diag, gap) {
                        (Some(d), Some(g)) => Some(if d.0 >= g.0 { d } else { g }),
                        (d, g) => d.or(g),
                    };
                    if let Some((s, k)) = best {
                        if let Some(cell) = row.get_mut(j) {
                            *cell = s + g;
                        }
                        if let Some(cell) = from_row.get_mut(j) {
                            *cell = k;
                        }
                    }
                }
            }
        }
    }
    let last = score.last()?;
    let (mut j, best) = last.iter().copied().enumerate().filter(|(_, s)| *s > NONE).max_by_key(|&(j, s)| (s, std::cmp::Reverse(j)))?;
    let mut positions = vec![0; n];
    for i in (0..n).rev() {
        if let Some(p) = positions.get_mut(i) {
            *p = j;
        }
        if i > 0 {
            j = from.get(i).and_then(|r| r.get(j)).copied().filter(|k| *k != usize::MAX)?;
        }
    }
    Some(FuzzyMatch { score: best, positions })
}

/// Match `query` against every candidate and return the matches, best first (ties keep the input
/// order, so callers can pre-sort by recency or frequency).
pub fn fuzzy_rank<'a, T>(query: &str, candidates: impl IntoIterator<Item = (T, &'a str)>) -> Vec<(T, FuzzyMatch)> {
    let mut out: Vec<(T, FuzzyMatch)> = candidates.into_iter().filter_map(|(t, s)| fuzzy_match(query, s).map(|m| (t, m))).collect();
    out.sort_by_key(|m| std::cmp::Reverse(m.1.score));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_required() {
        assert!(fuzzy_match("xyz", "File Export").is_none());
        assert!(fuzzy_match("fe", "File Export").is_some());
        assert_eq!(fuzzy_match("", "anything").unwrap().score, 0);
    }

    #[test]
    fn word_starts_beat_middles() {
        let m = fuzzy_match("fe", "File Export").unwrap();
        assert_eq!(m.positions, [0, 5]);
        let a = fuzzy_match("exp", "File Export").unwrap();
        let b = fuzzy_match("exp", "Complex Paths").unwrap();
        assert!(a.score > b.score, "{a:?} {b:?}");
    }

    #[test]
    fn camel_case_and_ids() {
        let m = fuzzy_match("rd", "edit.rippleDelete").unwrap();
        assert_eq!(m.positions, [5, 11]);
    }

    #[test]
    fn consecutive_preferred() {
        let m = fuzzy_match("undo", "Edit: Undo").unwrap();
        assert_eq!(m.positions, [6, 7, 8, 9]);
    }

    #[test]
    fn ranking() {
        let items = ["Save As…", "Save", "Select All", "Snap"];
        let r = fuzzy_rank("sa", items.iter().enumerate().map(|(i, s)| (i, *s)));
        let order: Vec<&str> = r.iter().map(|(i, _)| items[*i]).collect();
        assert_eq!(order, ["Save As…", "Save", "Select All", "Snap"]);
    }

    #[test]
    fn hostile_sizes_are_bounded() {
        let long = "a".repeat(100_000);
        assert!(fuzzy_match(&long, &long).is_none());
        let q = "a".repeat(64);
        let m = fuzzy_match(&q, &long).unwrap();
        assert_eq!(m.positions.len(), 64);
    }
}
