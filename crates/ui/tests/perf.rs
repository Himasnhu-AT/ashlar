#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Performance gates for `ashlar-ui` components, on a headless `egui::Context` (no app, no GPU):
//! the CPU cost of one egui pass, which is what the components control.
//!
//! `cargo test -p ashlar-ui --release --test perf -- --ignored --nocapture --test-threads=1`
//!
//! `md_*_layout_jobs` counts the text layouts egui's font cache served in the last pass (it keeps
//! only galleys used in that pass), a cheap stand-in for allocation counts: each one is a
//! `LayoutJob` built, hashed and looked up.

use std::time::{Duration, Instant};

use ashlar_command::CommandInfo;
use ashlar_command::shortcut::Platform;
use ashlar_ui::cache;
use ashlar_ui::commands::palette::CommandPalette;
use ashlar_ui::markdown::{self, MarkdownStyle};
use ashlar_ui::tree::{TreeModel, TreeState, TreeView};
use ashlar_ui::virtual_list::{RowHeights, VirtualList};
use egui::text::LayoutJob;
use egui::{Color32, Event, FontId, Rect, pos2, vec2};

#[path = "support/allocs.rs"]
mod allocs;

/// Counts allocations for the `*_allocs` metrics (this test binary only).
#[global_allocator]
static GLOBAL: &stats_alloc::StatsAlloc<std::alloc::System> = &stats_alloc::INSTRUMENTED_SYSTEM;

const SCREEN: Rect = Rect { min: pos2(0.0, 0.0), max: pos2(800.0, 600.0) };

/// One headless pass at time `t`, with `events`; returns the pass's wall time.
fn frame(ctx: &egui::Context, t: f64, events: Vec<Event>, ui: impl FnMut(&mut egui::Ui)) -> Duration {
    let input = egui::RawInput { screen_rect: Some(SCREEN), time: Some(t), events, ..Default::default() };
    let t0 = Instant::now();
    let mut out = ctx.run_ui(input, ui);
    let dt = t0.elapsed();
    out.textures_delta.clear();
    dt
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// The `p`-th percentile (0..=100) of `v` in milliseconds.
fn pct(v: &mut [Duration], p: usize) -> f64 {
    v.sort_unstable();
    let i = (v.len().saturating_sub(1) * p) / 100;
    ms(v[i])
}

fn layout_jobs(ctx: &egui::Context) -> usize {
    ctx.fonts(|f| f.num_galleys_in_cache())
}

fn wheel(dy: f32) -> Vec<Event> {
    vec![
        Event::PointerMoved(pos2(300.0, 300.0)),
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, dy),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}

// ---------------------------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------------------------

const WORDS: &[&str] = &[
    "the", "frame", "layout", "cache", "revision", "galley", "virtual", "list", "scroll", "theme", "token", "command", "palette", "document", "undo",
    "history", "panel", "docking", "window", "native", "runtime", "agent", "control", "port", "fast",
];

/// A deterministic Markdown document of about `bytes` bytes: headings, paragraphs with inline
/// styles and links, lists, quotes, code blocks and a table now and then.
fn markdown_doc(bytes: usize) -> String {
    let mut s = String::new();
    let mut n = 0usize;
    let word = |n: &mut usize| {
        *n += 7;
        WORDS[*n % WORDS.len()]
    };
    let mut section = 0;
    while s.len() < bytes {
        section += 1;
        s.push_str(&format!("## Section {section}: {} {}\n\n", word(&mut n), word(&mut n)));
        for p in 0..3 {
            let mut para = String::new();
            for w in 0..60 {
                let x = word(&mut n);
                match (w + p) % 17 {
                    3 => para.push_str(&format!("**{x}** ")),
                    7 => para.push_str(&format!("*{x}* ")),
                    11 => para.push_str(&format!("`{x}()` ")),
                    13 => para.push_str(&format!("[{x}](https://example.com/{x}) ")),
                    _ => {
                        para.push_str(x);
                        para.push(' ');
                    }
                }
            }
            s.push_str(para.trim_end());
            s.push_str(".\n\n");
        }
        for i in 1..=4 {
            s.push_str(&format!("{i}. {} {} {} `{}`\n", word(&mut n), word(&mut n), word(&mut n), word(&mut n)));
        }
        s.push_str("- [x] done item\n- [ ] open item with **bold** text\n\n");
        s.push_str(&format!("> A quote about {} and {}.\n\n", word(&mut n), word(&mut n)));
        if section % 2 == 0 {
            s.push_str("```rust\nfn main() {\n    let x = compute(1, 2);\n    println!(\"{x}\");\n}\n```\n\n");
        }
        if section % 3 == 0 {
            s.push_str("| name | value | note |\n|---|---:|---|\n");
            for r in 0..5 {
                s.push_str(&format!("| {} | {r} | {} |\n", word(&mut n), word(&mut n)));
            }
            s.push('\n');
        }
    }
    s
}

fn md_style() -> MarkdownStyle {
    MarkdownStyle::from_visuals(&egui::Visuals::dark(), 13.0)
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_markdown_static_50kb() {
    let _serial = allocs::serial();
    let doc = markdown_doc(50 * 1024);
    let style = md_style();
    let ctx = egui::Context::default();
    let draw = |ctx: &egui::Context, t: f64| {
        frame(ctx, t, Vec::new(), |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                markdown::show(ui, "doc", &doc, &style);
            });
        })
    };
    let first = draw(&ctx, 0.0);
    for i in 1..20 {
        draw(&ctx, f64::from(i) / 120.0);
    }
    let mut times = Vec::new();
    for i in 20..220 {
        times.push(draw(&ctx, f64::from(i) / 120.0));
    }
    println!("bench: md_static_first_frame {:.2} ms", ms(first));
    println!("bench: md_static_frame_p50 {:.3} ms", pct(&mut times, 50));
    println!("bench: md_static_frame_p95 {:.3} ms", pct(&mut times, 95));
    println!("bench: md_static_layout_jobs {} n", layout_jobs(&ctx));
    println!("md_static: {} bytes, {} blocks", doc.len(), markdown::parse(&doc).len());
}

/// Heap allocations of one frame of the static 50 KB document, once warm.
#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_markdown_static_50kb_allocs() {
    let _serial = allocs::serial();
    let doc = markdown_doc(50 * 1024);
    let style = md_style();
    let ctx = egui::Context::default();
    let draw = |ctx: &egui::Context, t: f64| {
        frame(ctx, t, Vec::new(), |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                markdown::show(ui, "doc", &doc, &style);
            });
        })
    };
    for i in 0..20 {
        draw(&ctx, f64::from(i) / 120.0);
    }
    let frames: u32 = 100;
    let ((), a) = allocs::count(|| {
        for i in 20..20 + frames {
            draw(&ctx, f64::from(i) / 120.0);
        }
    });
    let n = a.count / frames as usize;
    println!("bench: md_static_allocs {n} n");
    println!("bench: md_static_alloc_bytes {} B", a.bytes / frames as usize);
    assert!(n > 0, "the counting allocator saw the frames");
}

/// One 50 KB paragraph with no blank line in it (a pasted log, a minified reply): inline parsing
/// must stay linear in its length.
#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_markdown_long_paragraph() {
    let mut para = String::new();
    let mut n = 0usize;
    while para.len() < 50 * 1024 {
        n += 7;
        let x = WORDS[n % WORDS.len()];
        match n % 13 {
            1 => para.push_str(&format!("**{x}** ")),
            4 => para.push_str(&format!("*{x}* ")),
            6 => para.push_str(&format!("`{x}` ")),
            9 => para.push_str(&format!("[{x}](https://example.com/{x}) ")),
            11 => para.push_str("a*b [c] _d "),
            _ => {
                para.push_str(x);
                para.push(' ');
            }
        }
    }
    let mut times = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        let blocks = std::hint::black_box(markdown::parse(std::hint::black_box(&para)));
        times.push(t.elapsed());
        assert_eq!(blocks.len(), 1);
    }
    let p50 = pct(&mut times, 50);
    println!("bench: md_long_paragraph_parse {p50:.3} ms");
    assert!(p50 < 50.0, "a 50 KB paragraph parses in linear time: {p50} ms");
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_markdown_stream_7kb() {
    let _serial = allocs::serial();
    let doc = markdown_doc(7 * 1024);
    let style = md_style();
    let ctx = egui::Context::default();
    let mut times = Vec::new();
    let mut end = 0usize;
    let mut i = 0u32;
    while end < doc.len() {
        end = (end + 20).min(doc.len());
        while !doc.is_char_boundary(end) {
            end += 1;
        }
        let text = &doc[..end];
        i += 1;
        times.push(frame(&ctx, f64::from(i) / 120.0, Vec::new(), |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                markdown::show(ui, "reply", text, &style);
            });
        }));
    }
    let frames = times.len();
    println!("bench: md_stream_frame_p50 {:.3} ms", pct(&mut times, 50));
    println!("bench: md_stream_frame_p95 {:.3} ms", pct(&mut times, 95));
    println!("bench: md_stream_frame_max {:.3} ms", pct(&mut times, 100));
    println!("bench: md_stream_layout_jobs {} n", layout_jobs(&ctx));
    assert!(frames > 300, "{frames} frames");
}

// ---------------------------------------------------------------------------------------------
// Galley cache
// ---------------------------------------------------------------------------------------------

fn label_job(i: usize) -> LayoutJob {
    LayoutJob::simple_singleline(format!("label {i}"), FontId::proportional(13.0), Color32::WHITE)
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_galley_cache_hits() {
    let _serial = allocs::serial();
    const LABELS: usize = 2000;
    let ctx = egui::Context::default();
    let mut per_hit = Vec::new();
    for f in 0..60 {
        frame(&ctx, f64::from(f) / 120.0, Vec::new(), |ui| {
            let t0 = Instant::now();
            for i in 0..LABELS {
                let g = cache::galley(ui, ("perf-label", i), 1, || label_job(i));
                std::hint::black_box(&g);
            }
            if f >= 10 {
                per_hit.push(t0.elapsed() / LABELS as u32);
            }
        });
    }
    per_hit.sort_unstable();
    println!("bench: galley_hit {} ns", per_hit[per_hit.len() / 2].as_nanos());

    // overflow: a steady set of labels drawn every frame, with new labels mixed in (a scrolling
    // feed); the shared cache holds 8192. Every steady label laid out again is a wasted layout.
    const STEADY: usize = 6000;
    const FRESH: usize = 300;
    let ctx = egui::Context::default();
    let mut worst = 0u64;
    let mut total = 0u64;
    let mut next_fresh = 1_000_000usize;
    for f in 0..120u32 {
        let before = cache::galley_stats(&ctx).misses;
        frame(&ctx, f64::from(f) / 120.0, Vec::new(), |ui| {
            for i in 0..STEADY {
                if i % (STEADY / FRESH) == 0 {
                    next_fresh += 1;
                    let k = next_fresh;
                    std::hint::black_box(cache::galley(ui, ("perf-feed", k), 1, || label_job(k)));
                }
                std::hint::black_box(cache::galley(ui, ("perf-feed", i), 1, || label_job(i)));
            }
        });
        let misses = cache::galley_stats(&ctx).misses - before;
        if f >= 2 {
            let relayouts = misses.saturating_sub(FRESH as u64);
            worst = worst.max(relayouts);
            total += relayouts;
        }
    }
    println!("bench: galley_overflow_relayouts {worst} n");
    println!("bench: galley_overflow_relayouts_total {total} n");
}

// ---------------------------------------------------------------------------------------------
// Virtual list
// ---------------------------------------------------------------------------------------------

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_virtual_list_variable_1m() {
    let _serial = allocs::serial();
    const ROWS: usize = 1_000_000;
    let ctx = egui::Context::default();
    let draw = |ctx: &egui::Context, t: f64, revision: u64, tall_row: usize, changed: &[usize]| {
        let height = move |i: usize| if i == tall_row { 60.0 } else { 18.0 + (i % 3) as f32 * 4.0 };
        frame(ctx, t, Vec::new(), |ui| {
            VirtualList::new("perf.vlist", ROWS, RowHeights::Variable { revision, height: &height }).changed_rows(changed).show(ui, |ui, row| {
                ui.label(format!("row {}", row.index));
            });
        })
    };
    let mut revision = 1u64;
    for i in 0..10 {
        draw(&ctx, f64::from(i) / 120.0, revision, 0, &[]);
    }
    let mut steady = Vec::new();
    for i in 0..20 {
        steady.push(draw(&ctx, 0.1 + f64::from(i) / 120.0, revision, 0, &[]));
    }
    let steady = pct(&mut steady, 50);
    let mut rebuild = Vec::new();
    for i in 0..20 {
        revision += 1;
        rebuild.push(draw(&ctx, 0.3 + f64::from(i) / 120.0, revision, 0, &[]));
    }
    // one row's height changes, said with `changed_rows` (the revision stays)
    let mut one = Vec::new();
    let mut prev = 0;
    for i in 0..20 {
        let tall_row = 500_000 + i;
        let t = draw(&ctx, 0.5 + f64::from(i as u32) / 120.0, revision, tall_row, &[prev, tall_row]);
        prev = tall_row;
        one.push(t);
    }
    let one = (pct(&mut one, 50) - steady).max(0.0);
    println!("bench: vlist_variable_steady_frame_1m {steady:.3} ms");
    println!("bench: offsets_rebuild_1m {:.3} ms", pct(&mut rebuild, 50));
    println!("bench: one_row_height_change_1m {:.1} us", one * 1000.0);
}

// ---------------------------------------------------------------------------------------------
// Tree
// ---------------------------------------------------------------------------------------------

struct BigTree {
    roots: u64,
    per_root: u64,
    labels: Vec<String>,
}

impl TreeModel for BigTree {
    fn children(&self, parent: Option<u64>, out: &mut Vec<u64>) {
        match parent {
            None => out.extend(0..self.roots),
            Some(r) if r < self.roots => out.extend((0..self.per_root).map(|k| self.roots + r * self.per_root + k)),
            Some(_) => {}
        }
    }
    fn has_children(&self, node: u64) -> bool {
        node < self.roots
    }
    fn label(&self, node: u64) -> &str {
        &self.labels[(node % self.labels.len() as u64) as usize]
    }
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_tree_1m_flatten_and_scroll() {
    let _serial = allocs::serial();
    let model = BigTree { roots: 1000, per_root: 1000, labels: (0..1000).map(|i| format!("Node {i}")).collect() };
    let mut state = TreeState::default();
    for r in 0..model.roots {
        state.set_expanded(r, true);
    }
    let ctx = egui::Context::default();
    let flatten = frame(&ctx, 0.0, Vec::new(), |ui| {
        TreeView::new("perf.tree", &mut state, &model, 1).show(ui);
    });
    assert!(state.row_count() >= 1_000_000, "{} rows", state.row_count());
    for i in 1..20 {
        frame(&ctx, f64::from(i) / 120.0, Vec::new(), |ui| {
            TreeView::new("perf.tree", &mut state, &model, 1).show(ui);
        });
    }
    let mut times = Vec::new();
    for i in 0..480 {
        let dy = if (i / 120) % 2 == 0 { -240.0 } else { 240.0 };
        times.push(frame(&ctx, 0.2 + f64::from(i) / 120.0, wheel(dy), |ui| {
            TreeView::new("perf.tree", &mut state, &model, 1).show(ui);
        }));
    }
    println!("bench: tree_flatten_1m {:.2} ms", ms(flatten));
    println!("bench: tree_scroll_1m_p50 {:.3} ms", pct(&mut times, 50));
    println!("bench: tree_scroll_1m_p95 {:.3} ms", pct(&mut times, 95));
}

// ---------------------------------------------------------------------------------------------
// Command palette
// ---------------------------------------------------------------------------------------------

fn command(i: usize) -> CommandInfo {
    let area = ["file", "edit", "view", "tools", "window", "help", "export", "layer"][i % 8];
    let verb = ["open", "close", "toggle", "show", "reset", "duplicate", "rename", "move"][(i / 8) % 8];
    CommandInfo {
        id: format!("{area}.{verb}_{i}"),
        label: format!("{} {} {i}", verb.to_uppercase(), area),
        menu: Vec::new(),
        shortcut: i.is_multiple_of(50).then(|| "Mod+Shift+K".to_string()),
        params_schema: serde_json::Value::Null,
        params_text: None,
        enabled: !i.is_multiple_of(13),
        disabled_reason: None,
        mutates: false,
    }
}

#[test]
#[ignore = "perf: run with --release --ignored --nocapture"]
fn perf_palette_filter_5k_commands() {
    let _serial = allocs::serial();
    let commands: Vec<CommandInfo> = (0..5000).map(command).collect();
    let ctx = egui::Context::default();
    let mut palette = CommandPalette::new();
    palette.open();
    let mut t = 0.0;
    let mut step = |palette: &mut CommandPalette, events: Vec<Event>| {
        t += 1.0 / 120.0;
        frame(&ctx, t, events, |ui| {
            std::hint::black_box(palette.show(ui.ctx(), &commands, 1, Platform::Other));
        })
    };
    for _ in 0..4 {
        step(&mut palette, Vec::new());
    }
    let mut keys = Vec::new();
    for round in 0..40 {
        for c in ["t", "o", "g"] {
            keys.push(step(&mut palette, vec![Event::Text(c.into())]));
        }
        assert_eq!(palette.query(), "tog", "round {round}");
        for _ in 0..3 {
            keys.push(step(&mut palette, vec![key(egui::Key::Backspace)]));
        }
        assert_eq!(palette.query(), "", "round {round}");
    }
    println!("bench: palette_keystroke_5k_p50 {:.3} ms", pct(&mut keys, 50));
    println!("bench: palette_keystroke_5k_p95 {:.3} ms", pct(&mut keys, 95));
}

fn key(key: egui::Key) -> Event {
    Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
}
