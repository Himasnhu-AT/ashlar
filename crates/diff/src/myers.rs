//! Sequence diff: Myers' O((N+M)·D) algorithm with a cost cap, and a coarse fallback past the cap.
//!
//! Elements are interned to `u32` first, so comparisons are integer compares. The common prefix
//! and suffix are stripped before Myers runs. When the edit distance of what is left exceeds the
//! cap, the input is split at lines that occur exactly once on both sides (the longest increasing
//! run of them) and each piece is diffed again; past a small recursion depth, or with no such
//! anchors, the piece becomes one delete and one insert. The result is then a valid but not
//! necessarily minimal edit script, and [`Edit::coarse`] says so.

use std::collections::HashMap;
use std::hash::Hash;

/// The default cost cap: the largest edit distance one Myers run may reach. Its trace takes about
/// `cap² / 2` words, so the default keeps it near 4 MB.
pub const DEFAULT_MAX_COST: usize = 1024;

/// Hard ceiling on the cost cap, whatever the caller asks for (about 16 MB of trace).
const MAX_COST_CEILING: usize = 2048;

/// How many times the anchor fallback may split a piece that still exceeds the cap.
const MAX_DEPTH: u32 = 6;

/// One run of an edit script. Indices are 0-based positions in the old (`a`) and new (`b`) inputs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `len` elements equal on both sides.
    Equal { old: usize, new: usize, len: usize },
    /// `len` elements of the old side removed.
    Delete { old: usize, len: usize },
    /// `len` elements of the new side added.
    Insert { new: usize, len: usize },
}

/// An edit script from the old to the new sequence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edit {
    /// Runs in order; together they cover both inputs exactly once.
    pub ops: Vec<Op>,
    /// The cost cap was hit somewhere: the script is valid but may not be minimal.
    pub coarse: bool,
}

impl Edit {
    /// Elements deleted plus elements inserted.
    pub fn cost(&self) -> usize {
        self.ops
            .iter()
            .map(|op| match op {
                Op::Equal { .. } => 0,
                Op::Delete { len, .. } | Op::Insert { len, .. } => *len,
            })
            .fold(0usize, usize::saturating_add)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Eq,
    Del,
    Ins,
}

/// Diffs two sequences. `max_cost` caps one Myers run (see the module docs); it is clamped to
/// `1..=2048`.
///
/// ```
/// use ashlar_diff::{diff_ops, Op};
///
/// let edit = diff_ops(&["a", "b", "c"], &["a", "c", "d"], 1024);
/// assert_eq!(edit.cost(), 2);
/// assert_eq!(edit.ops.first(), Some(&Op::Equal { old: 0, new: 0, len: 1 }));
/// ```
pub fn diff_ops<T: Eq + Hash>(a: &[T], b: &[T], max_cost: usize) -> Edit {
    let mut ids: HashMap<&T, u32> = HashMap::new();
    let mut ia = Vec::with_capacity(a.len());
    for x in a {
        let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        ia.push(*ids.entry(x).or_insert(next));
    }
    let mut ib = Vec::with_capacity(b.len());
    for x in b {
        let next = u32::try_from(ids.len()).unwrap_or(u32::MAX);
        ib.push(*ids.entry(x).or_insert(next));
    }
    diff_ids(&ia, &ib, max_cost)
}

/// [`diff_ops`] on interned ids.
pub(crate) fn diff_ids(a: &[u32], b: &[u32], max_cost: usize) -> Edit {
    let cap = max_cost.clamp(1, MAX_COST_CEILING);
    let mut steps = Vec::with_capacity(a.len().saturating_add(b.len()));
    let mut coarse = false;
    let mut scratch = Vec::new();
    rec(a, b, cap, 0, &mut steps, &mut coarse, &mut scratch);
    Edit { ops: compress(&steps), coarse }
}

fn compress(steps: &[Step]) -> Vec<Op> {
    let mut ops: Vec<Op> = Vec::new();
    let (mut x, mut y) = (0usize, 0usize);
    for s in steps {
        match (s, ops.last_mut()) {
            (Step::Eq, Some(Op::Equal { len, .. })) | (Step::Del, Some(Op::Delete { len, .. })) | (Step::Ins, Some(Op::Insert { len, .. })) => {
                *len = len.saturating_add(1);
            }
            (Step::Eq, _) => ops.push(Op::Equal { old: x, new: y, len: 1 }),
            (Step::Del, _) => ops.push(Op::Delete { old: x, len: 1 }),
            (Step::Ins, _) => ops.push(Op::Insert { new: y, len: 1 }),
        }
        match s {
            Step::Eq => {
                x = x.saturating_add(1);
                y = y.saturating_add(1);
            }
            Step::Del => x = x.saturating_add(1),
            Step::Ins => y = y.saturating_add(1),
        }
    }
    ops
}

fn push_n(out: &mut Vec<Step>, step: Step, n: usize) {
    out.extend(std::iter::repeat_n(step, n));
}

fn rec(a: &[u32], b: &[u32], cap: usize, depth: u32, out: &mut Vec<Step>, coarse: &mut bool, scratch: &mut Vec<i64>) {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (a, b) = (a.get(prefix..).unwrap_or(&[]), b.get(prefix..).unwrap_or(&[]));
    let suffix = a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count();
    let (a, b) = (a.get(..a.len() - suffix).unwrap_or(&[]), b.get(..b.len() - suffix).unwrap_or(&[]));
    push_n(out, Step::Eq, prefix);
    if a.is_empty() || b.is_empty() {
        push_n(out, Step::Del, a.len());
        push_n(out, Step::Ins, b.len());
    } else if !myers(a, b, cap, out, scratch) {
        let anchors = if depth < MAX_DEPTH { unique_anchors(a, b) } else { Vec::new() };
        if anchors.is_empty() {
            *coarse = true;
            push_n(out, Step::Del, a.len());
            push_n(out, Step::Ins, b.len());
        } else {
            let (mut i, mut j) = (0usize, 0usize);
            for (ai, bj) in anchors {
                let (Some(pa), Some(pb)) = (a.get(i..ai), b.get(j..bj)) else { continue };
                rec(pa, pb, cap, depth + 1, out, coarse, scratch);
                out.push(Step::Eq);
                i = ai + 1;
                j = bj + 1;
            }
            rec(a.get(i..).unwrap_or(&[]), b.get(j..).unwrap_or(&[]), cap, depth + 1, out, coarse, scratch);
        }
    }
    push_n(out, Step::Eq, suffix);
}

/// Marks a diagonal no path reaches in a round.
const NONE: i64 = i64::MIN;

/// Myers' greedy algorithm. Appends the steps and returns `true`, or returns `false` (appending
/// nothing) when the edit distance exceeds `cap`.
///
/// `trace` keeps, for each round `d`, the furthest x on diagonals `-d, -d+2, …, d` (`d + 1`
/// entries starting at `d·(d+1)/2`), so a path can be walked back without storing it.
fn myers(a: &[u32], b: &[u32], cap: usize, out: &mut Vec<Step>, trace: &mut Vec<i64>) -> bool {
    let (n, m) = (a.len() as i64, b.len() as i64);
    let limit = (cap as i64).min(n + m);
    trace.clear();
    let snake = |mut x: i64, mut y: i64| {
        while x < n && y < m && a.get(x as usize) == b.get(y as usize) {
            x += 1;
            y += 1;
        }
        x
    };
    for d in 0..=limit {
        for j in 0..=d {
            let k = 2 * j - d;
            let x = if d == 0 { Some(0) } else { choose(trace, d, k, n, m).map(|(x, _)| x) };
            let x = match x {
                Some(x) => snake(x, x - k),
                None => NONE,
            };
            trace.push(x);
            if x == n && x - k == m {
                backtrack(trace, d, n, m, out);
                return true;
            }
        }
    }
    false
}

/// The furthest x a round-`d` path reaches on diagonal `k` before its snake, and whether it came
/// down (an insertion) rather than right (a deletion). Reads round `d - 1` from `trace`.
fn choose(trace: &[i64], d: i64, k: i64, n: i64, m: i64) -> Option<(i64, bool)> {
    if k < -m || k > n {
        return None;
    }
    let prev = d - 1;
    let start = (prev * (prev + 1) / 2) as usize;
    let at = |kk: i64| -> Option<i64> {
        if kk < -prev || kk > prev {
            return None;
        }
        let x = *trace.get(start + ((kk + prev) / 2) as usize)?;
        (x != NONE).then_some(x)
    };
    let down = at(k + 1).filter(|x| x - k <= m);
    let right = at(k - 1).map(|x| x + 1).filter(|x| *x <= n);
    match (down, right) {
        (Some(xd), Some(xr)) if xd >= xr => Some((xd, true)),
        (_, Some(xr)) => Some((xr, false)),
        (Some(xd), None) => Some((xd, true)),
        (None, None) => None,
    }
}

fn backtrack(trace: &[i64], last: i64, n: i64, m: i64, out: &mut Vec<Step>) {
    let mut rev: Vec<Step> = Vec::new();
    let (mut x, mut y) = (n, m);
    let mut d = last;
    while d > 0 {
        let k = x - y;
        let Some((mx, down)) = choose(trace, d, k, n, m) else { break };
        // the snake from the move's end point to (x, y)
        for _ in mx..x {
            rev.push(Step::Eq);
        }
        if down {
            rev.push(Step::Ins);
            x = mx;
            y = mx - (k + 1);
        } else {
            rev.push(Step::Del);
            x = mx - 1;
            y = x - (k - 1);
        }
        d -= 1;
    }
    for _ in 0..x.min(y) {
        rev.push(Step::Eq);
    }
    out.extend(rev.into_iter().rev());
}

/// Pairs `(i, j)` with `a[i] == b[j]` where the element occurs exactly once in each side, in the
/// longest run increasing on both sides.
fn unique_anchors(a: &[u32], b: &[u32]) -> Vec<(usize, usize)> {
    // per id: (count in a, count in b, position in b)
    let mut seen: HashMap<u32, (u32, u32, usize)> = HashMap::new();
    for x in a {
        let e = seen.entry(*x).or_insert((0, 0, 0));
        e.0 = e.0.saturating_add(1);
    }
    for (j, x) in b.iter().enumerate() {
        if let Some(e) = seen.get_mut(x) {
            e.1 = e.1.saturating_add(1);
            e.2 = j;
        }
    }
    let pairs: Vec<(usize, usize)> =
        a.iter().enumerate().filter_map(|(i, x)| seen.get(x).filter(|e| e.0 == 1 && e.1 == 1).map(|e| (i, e.2))).collect();
    longest_increasing(&pairs)
}

/// The longest subsequence of `pairs` (sorted by `.0`) increasing in `.1` (patience sorting).
fn longest_increasing(pairs: &[(usize, usize)]) -> Vec<(usize, usize)> {
    // tails[l] = index into pairs of the smallest tail of an increasing run of length l + 1
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<Option<usize>> = vec![None; pairs.len()];
    for (idx, (_, v)) in pairs.iter().enumerate() {
        let pos = tails.partition_point(|&t| pairs.get(t).is_some_and(|p| p.1 < *v));
        if let Some(p) = prev.get_mut(idx) {
            *p = pos.checked_sub(1).and_then(|q| tails.get(q).copied());
        }
        if pos < tails.len() {
            if let Some(t) = tails.get_mut(pos) {
                *t = idx;
            }
        } else {
            tails.push(idx);
        }
    }
    let mut run = Vec::with_capacity(tails.len());
    let mut cur = tails.last().copied();
    while let Some(c) = cur {
        if let Some(p) = pairs.get(c) {
            run.push(*p);
        }
        cur = prev.get(c).copied().flatten();
        if run.len() > pairs.len() {
            break;
        }
    }
    run.reverse();
    run
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(a: &[u32], b: &[u32], edit: &Edit) -> Vec<u32> {
        let mut out = Vec::new();
        for op in &edit.ops {
            match *op {
                Op::Equal { old, len, .. } => out.extend_from_slice(&a[old..old + len]),
                Op::Delete { .. } => {}
                Op::Insert { new, len } => out.extend_from_slice(&b[new..new + len]),
            }
        }
        out
    }

    #[test]
    fn simple_scripts() {
        let a = [1, 2, 3, 4, 5];
        let b = [1, 3, 4, 6, 5];
        let e = diff_ids(&a, &b, 1024);
        assert_eq!(e.cost(), 2);
        assert_eq!(apply(&a, &b, &e), b);
        assert!(!e.coarse);
    }

    #[test]
    fn cap_falls_back_to_anchors_then_coarse() {
        let a: Vec<u32> = (0..200).collect();
        let b: Vec<u32> = (0..200).map(|i| if i % 2 == 0 { i } else { 1000 + i }).collect();
        let e = diff_ids(&a, &b, 4);
        assert!(e.coarse || e.cost() == 200);
        assert_eq!(apply(&a, &b, &e), b);
        // with anchors every other element, the pieces fit the cap and the result is minimal
        assert_eq!(e.cost(), 200);
        let c: Vec<u32> = (0..50).map(|i| 5000 + i).collect();
        let e = diff_ids(&a[..50], &c, 4);
        assert!(e.coarse);
        assert_eq!(e.cost(), 100);
    }

    #[test]
    fn lis_is_increasing() {
        let pairs = [(0, 5), (1, 1), (2, 2), (3, 9), (4, 3), (5, 4)];
        assert_eq!(longest_increasing(&pairs), vec![(1, 1), (2, 2), (4, 3), (5, 4)]);
    }
}
