# Benchmarks

How fast ashlar is, how that was measured, and how to reproduce it. Machine: Apple M5 Pro
(15 cores), 24 GB, macOS. All builds are release builds, run through `memgate`.

Two kinds of numbers appear below:

- **Quiet measurements** were taken with nothing else heavy running. Comparisons use only these.
- **The full-suite snapshot** (`cargo xtask bench`, at the end) ran while other builds shared the
  machine. Its absolute timings are inflated, often by 2–4×. Allocation counts, sizes and frame
  counts are not affected by load.

## Running them

```sh
cargo xtask bench                                   # every `perf_*` test; writes target/bench/bench.{json,md}
cargo xtask bench --filter plain_scroll             # one group
cargo xtask bench --repeat 3 --compare base.json    # fails on a >5 % regression against a saved run
cargo xtask launch --runs 9                         # launch → window → first present, opens real windows
cargo xtask launch --memory                         # idle footprint and peak after a settle
```

A perf test is a `#[test] #[ignore] fn perf_*` that prints `bench: <metric> <value> <unit>` lines
(see docs/architecture.md, "Performance gate").

## Against a comparable native Rust UI framework

Measured on 2026-11-10, both frameworks in the same session with the same tools:
- a launcher that times spawn → window, plus `ps` and `vmmap` for idle cost
- each framework's own benchmark for the same scrolling workload: a plain list of 1,000 rows of 32 pt
  in a 760×800 pt viewport at 2×, with 120 warm-up frames and then 480 wheel steps of 12 pt at 120 Hz,
  reversing halfway

The other framework draws with its own GPU renderer and a retained element tree. ashlar draws with
egui.

| Metric | ashlar | other framework | Better |
|---|---|---|---|
| Scroll 1k plain rows, frame CPU p50 / p95, with `list::Row` | **0.10 / 0.12 ms** | 0.298 / 0.32 ms | ashlar, ~2.8× |
| Same, rows drawn as egui selectable labels | 0.65–0.76 / 0.70–0.81 ms | 0.298 / 0.32 ms | other, ~2.3× |
| Scroll 10k plain rows, p50, `list::Row` / labels | 0.96–1.15 / 8.6–8.9 ms | not run | — |
| Stripped hello binary | 14.0 MB; **9.2 MB** with `--profile release-small` | 21.3 MB | ashlar |
| Incremental rebuild of hello | 5.3 s | 14.7–17.7 s | ashlar, ~3× |
| Clean release build of hello (loaded machine) | 133 s | 133 s | tie |
| Launch → window, warm median of 8 | 106.5 ms | 98 ms | other, ~8 % |
| Launch → window, cold first run | 111 ms | 1,360 ms | ashlar |
| Idle CPU over 10 s | 0 % | 0 % | tie |
| Idle footprint / peak footprint | 40 MB / 94 MB (window never covered); 66 MB / 192 MB after the window was covered | 29 MB / 176 MB | other at idle |
| Dependencies of hello (unique packages) | 179 | 202 | ashlar |

What each scroll number covers:
- ashlar's is the thread CPU of the egui pass plus tessellation.
- The other framework's is the thread CPU from wheel dispatch to its draw list.
- Neither includes GPU submission.

Lists should be virtualized (`VirtualList`). Then the cost does not depend on the row count at all:
0.035 ms at 1k or 100k rows, measured quiet.

**Idle memory, explained:** ashlar's idle footprint is about 40 MB (94 MB peak) for a window that has
never been covered, and about 66 MB (about 193 MB peak) once it has been covered, minimized or moved
to another Space, even briefly right after launch. At that point macOS maps two swapchain images it
shares with the window server into the process (about 19 MB), and Metal allocates a second set of
blocks. This happens with any present mode or frame latency, so it is not something ashlar's own
settings cause. `cargo xtask launch --memory` launches 5 times and reports both groups. The other
framework's 29 MB was measured the same way. Whether its windows avoid the effect, or were simply
never covered during the measurement, is not known.

## Before and after the 2026-11 improvement work

All quiet measurements. "Before" is commit 393afe0.

| Metric | Before | After | Change |
|---|---|---|---|
| Plain 1k-row list, frame CPU p50 | 0.63 ms (labels) | 0.10 ms (`list::Row`) | 6× less |
| Typing with a search query, 100k notes, keystroke p95 | 4.6–5.0 ms | 0.53 ms | 9× less |
| Selecting the last of 100k notes, frame p95 | 0.106 ms | 0.038 ms | no longer depends on the selected row's position |
| Live resize, frame p95 | 0.103 ms | 0.034 ms | 3× less |
| Static 52 KB markdown, frame p50 | 0.944 ms | 0.015–0.031 ms | 30–60× less |
| Static 52 KB markdown, layout jobs per frame | 115 | 6 | |
| Long 50 KB markdown paragraph | quadratic | linear (`md_long_paragraph_parse` 0.4–0.7 ms) | |
| Galley cache, worst frame after it fills | 4,160 re-layouts | 0 | |
| One row height change in a 1M-row variable list | 1,388 µs | ~1.3 µs | |
| Frames drawn per engine command | 3.20 | 1.26 | 61 % fewer |
| Frames over 30 s of idle with a focused text field | 194 (all "input") | 21 (caret blinks), then 0 | |
| Allocations per idle wake frame | 954 | 272 | 71 % fewer, gated at ≤ 578 |
| Allocations per keystroke with a query | 3,716 | 990 | |
| Allocations per scroll frame (100k notes) | 472 | 345 | |
| Stripped hello binary | 12.76 MB (scratch app) | 9.2 MB with `release-small` | |

## Component costs (quiet)

| Metric | Value |
|---|---|
| `VirtualList` scroll, 1k / 100k rows | 0.035 / 0.033 ms |
| Tree, 1M nodes, scroll frame p95 | 0.041 ms |
| Code view, 100k lines, scrolled frame | 0.058 ms (first frame about 4.5 ms) |
| Diff view, 50k lines, scrolled frame | 0.41 ms (split 0.49 ms) |
| Chat transcript, 10k messages, frame | 0.129 ms (1k: 0.092 ms) |
| Myers diff, 10k lines | 2.08 ms |
| Syntax highlighting, 1k lines (Rust / TypeScript / Python) | 477 / 757 / 280 µs |
| Headless wgpu screenshot, 2560×1600 including PNG and base64 | 26 ms |
| Spring sample / rect retarget / OKLab mix | 7.9 ns / 185 ns / 36 ns |
| Galley cache hit | 20 ns |
| Document edit at 100k elements, chunked vs flat | 515 ns vs 12,632 ns |
| Undo or redo | 14 ns |

## Full-suite snapshot (loaded machine)

This is `cargo xtask bench` at `imp` 30e3264, run while five other builds were compiling. Use it to
see which metrics exist, not for absolute timings: run it on a quiet machine before comparing.

| Metric | Value |
|---|---|
| `allocs_per_idle_wake_frame` | 272 (gate ≤ 578) |
| `bytes_per_frame_scroll_100k` | 203,865 B |
| `notes_first_frame_wall` | 18.3 ms |
| `frame_p95_scroll_100k` / `tessellate_p95` / `frame_cpu_p95_x2` | 0.132 / 0.026 / 0.183 ms |
| `frames_per_key_command` | 3.0 |
| `plain_scroll_1k_rows_widget_p50` | 0.275 ms (0.10 ms quiet) |
| `virtual_scroll_100k_p50` | 0.057 ms |
| `md_static_frame_p50` / `md_static_layout_jobs` | 0.031 ms / 6 |
| `galley_overflow_relayouts` | 0 |
| `one_row_height_change_1m` | 1.5 µs |
| `transcript_10k_messages_frame` | 0.111 ms |
| `code_view_100k_lines_frame` / `diff_50k_lines_frame` | 0.094 / 0.428 ms |
| `hello_stripped_bytes` | 14,009,160 B |
