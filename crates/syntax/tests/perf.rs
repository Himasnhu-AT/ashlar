#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Perf tests (`cargo xtask bench`): each prints `bench: <metric> <value> us`.

use std::hint::black_box;
use std::time::Instant;

use ashlar_syntax::{Lang, Span, highlight_into};

const RUST: &str = r##"/// A documented item.
#[derive(Debug, Clone)]
pub struct Item<'a> { name: &'a str, count: u32 }
impl<'a> Item<'a> {
    pub fn new(name: &'a str) -> Self { Self { name, count: 0 } } // trailing comment
    fn bump(&mut self) -> u32 { self.count = self.count.saturating_add(1); println!("{} {}", self.name, 0x1F); self.count }
}
/* block
   comment */ const MAX: usize = 1_000;
fn raw() -> &'static str { r#"raw "string""# }
"##;

const TS: &str = r#"import { thing } from "./thing";
export class Store<T> extends Base implements Api {
  private items: T[] = []; // comment
  add(item: T): number { this.items.push(item); return this.items.length; }
  label(n: number) { return `item ${n} of ${this.items.length}`; }
}
const re = /ab+c/gi, half = total / 2;
/* block */ let x: string | undefined = undefined;
"#;

const PY: &str = r#"@dataclass
class Item(Base):
    """Doc string."""
    def bump(self, n: int = 1) -> int:  # comment
        self.count += n
        return f"{self.name}: {self.count}"
MAX = 1_000
"#;

/// `n` lines made by repeating `unit`.
fn lines(unit: &str, n: usize) -> String {
    let per = unit.lines().count().max(1);
    unit.repeat(n.div_ceil(per))
}

/// Microseconds per call, best of 5 rounds of `iters`.
fn us_per(iters: u32, mut f: impl FnMut()) -> f64 {
    let mut best = f64::INFINITY;
    for _ in 0..5 {
        let start = Instant::now();
        for _ in 0..iters {
            f();
        }
        best = best.min(start.elapsed().as_secs_f64() * 1e6 / f64::from(iters));
    }
    best
}

fn bench(name: &str, lang: Lang, unit: &str) {
    let text = lines(unit, 1000);
    let mut out: Vec<Span> = Vec::with_capacity(16 * 1024);
    let us = us_per(20, || {
        out.clear();
        highlight_into(Some(lang), black_box(&text), &mut out);
        black_box(out.len());
    });
    println!("bench: {name} {us:.1} us");
}

#[test]
#[ignore]
fn perf_highlight_1k_lines() {
    bench("highlight_1k_lines_rust", Lang::Rust, RUST);
    bench("highlight_1k_lines_typescript", Lang::TypeScript, TS);
    bench("highlight_1k_lines_python", Lang::Python, PY);
}
