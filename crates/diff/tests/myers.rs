#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Myers against a naive LCS on random inputs: under the cap the script is minimal, and every
//! script (also past the cap) turns the old sequence into the new one.

use ashlar_diff::{Op, diff_ops};

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

fn lcs(a: &[u8], b: &[u8]) -> usize {
    let mut dp = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            dp[i][j] = if a[i - 1] == b[j - 1] { dp[i - 1][j - 1] + 1 } else { dp[i - 1][j].max(dp[i][j - 1]) };
        }
    }
    dp[a.len()][b.len()]
}

/// Checks the script covers both inputs in order and rebuilds `b` from `a`.
fn check(a: &[u8], b: &[u8], ops: &[Op]) {
    let (mut x, mut y) = (0, 0);
    let mut out = Vec::new();
    for op in ops {
        match *op {
            Op::Equal { old, new, len } => {
                assert_eq!((old, new), (x, y));
                assert_eq!(&a[old..old + len], &b[new..new + len]);
                out.extend_from_slice(&a[old..old + len]);
                x += len;
                y += len;
            }
            Op::Delete { old, len } => {
                assert_eq!(old, x);
                x += len;
            }
            Op::Insert { new, len } => {
                assert_eq!(new, y);
                out.extend_from_slice(&b[new..new + len]);
                y += len;
            }
        }
    }
    assert_eq!((x, y), (a.len(), b.len()));
    assert_eq!(out, b);
}

fn random(rng: &mut Rng, len: usize, alphabet: usize) -> Vec<u8> {
    (0..len).map(|_| rng.below(alphabet) as u8).collect()
}

#[test]
fn myers_is_minimal_against_naive_lcs() {
    let mut rng = Rng(0xDEAD_BEEF);
    for _ in 0..3000 {
        let alphabet = 1 + rng.below(8);
        let a = {
            let n = rng.below(60);
            random(&mut rng, n, alphabet)
        };
        let b = if rng.below(3) == 0 {
            // a nearby edit of `a`
            let mut b = a.clone();
            for _ in 0..rng.below(6) {
                let i = rng.below(b.len() + 1);
                if rng.below(2) == 0 && i < b.len() {
                    b.remove(i);
                } else {
                    b.insert(i, rng.below(alphabet) as u8);
                }
            }
            b
        } else {
            {
                let n = rng.below(60);
                random(&mut rng, n, alphabet)
            }
        };
        let edit = diff_ops(&a, &b, 1024);
        assert!(!edit.coarse);
        check(&a, &b, &edit.ops);
        assert_eq!(edit.cost(), a.len() + b.len() - 2 * lcs(&a, &b), "a={a:?} b={b:?}");
    }
}

#[test]
fn past_the_cap_the_script_is_still_valid() {
    let mut rng = Rng(7);
    for _ in 0..500 {
        let alphabet = 2 + rng.below(40);
        let a = {
            let n = rng.below(200);
            random(&mut rng, n, alphabet)
        };
        let b = {
            let n = rng.below(200);
            random(&mut rng, n, alphabet)
        };
        let cap = 1 + rng.below(8);
        let edit = diff_ops(&a, &b, cap);
        check(&a, &b, &edit.ops);
        assert!(edit.cost() >= a.len() + b.len() - 2 * lcs(&a, &b));
    }
}

#[test]
fn hostile_caps_and_sizes() {
    let a: Vec<u32> = (0..3000).collect();
    let b: Vec<u32> = (0..3000).rev().collect();
    let edit = diff_ops(&a, &b, usize::MAX);
    assert!(edit.cost() >= 5998 && edit.cost() <= 6000);
    let edit = diff_ops(&a, &b, 0);
    assert_eq!(edit.cost() % 2, 0);
    let empty: [u32; 0] = [];
    assert!(diff_ops(&empty, &empty, 10).ops.is_empty());
}
