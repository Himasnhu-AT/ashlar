#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Diff perf: a 10k-line file with scattered edits.
//!
//! `cargo test -p ashlar-diff --release --test perf -- --ignored --nocapture`

use std::time::Instant;

use ashlar_diff::FileDiff;

/// 10k lines of code-like text and a copy with an edit every ~50 lines (changed, inserted and
/// deleted lines), about 200 changes in all.
fn texts(lines: usize) -> (String, String) {
    let mut old = String::new();
    let mut new = String::new();
    for i in 0..lines {
        let line = format!("    let value_{i} = compute({i}, \"item {}\");\n", i % 97);
        old.push_str(&line);
        match i % 50 {
            7 => new.push_str(&format!("    let value_{i} = compute({}, \"item\");\n", i + 1)),
            23 => {
                new.push_str(&line);
                new.push_str("    // inserted\n");
            }
            41 => {}
            _ => new.push_str(&line),
        }
    }
    (old, new)
}

#[test]
#[ignore]
fn perf_myers_10k_lines() {
    let (old, new) = texts(10_000);
    let mut times = Vec::new();
    let mut hunks = 0;
    for _ in 0..7 {
        let t0 = Instant::now();
        let d = FileDiff::from_texts(&old, &new);
        times.push(t0.elapsed());
        hunks = d.hunks.len();
        assert!(!d.coarse);
    }
    assert!(hunks >= 150, "{hunks}");
    times.sort_unstable();
    println!("bench: myers_10k_lines {:.3} ms", times[times.len() / 2].as_secs_f64() * 1000.0);
}
