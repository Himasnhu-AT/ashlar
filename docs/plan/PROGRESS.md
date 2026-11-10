# Progress log

Newest first. Each entry has the date, the task ids it covers, what changed, numbers before and after,
and any decision or surprise.

## 2026-11-10: wave 1, first merges (wt/tools, wt/notes → imp, CI green)

- IMP-4.1: `trace` with `--port`/`--app` now records inside a batch, and is a usage error outside
  one. The new test failed before the fix.
- IMP-4.2 (MCP part): `app_stats {reset?}` and `app_logs {limit?, level?}`, listed only when the app
  reports the methods. Process memory and CPU in `app.stats` remain todo.
- IMP-4.3: the protocol doc's MCP table is complete; `crates/mcp/tests/protocol_doc.rs` fails if it
  drifts from the code.
- IMP-1.4: `examples/notes/tests/scroll_bench.rs` measures thread CPU of the egui pass plus
  tessellation. Each plain row is a selectable label.

  | Metric | p50 | p95 |
  |---|---|---|
  | `plain_scroll_1k` | 0.625 ms | 0.663 ms |
  | `plain_scroll_10k` | 6.389 ms | 6.882 ms |
  | `plain_scroll_1k_painted` (rows painted with no widget, a floor for IMP-2.8) | 0.205 ms | — |
  | `virtual_scroll_1k` / `virtual_scroll_100k` | 0.024 ms | 0.030 ms |

- IMP-1.5 and IMP-2.5, before → after (release, Apple M5 Pro):

  | Metric | Before | After |
  |---|---|---|
  | `frame_p95_scroll_sel_last` | 0.106 ms | 0.038 ms |
  | `keystroke_p95_100k_query` | 4.6–5.0 ms | 0.53 ms |
  | `resize_frame_p95` | 0.103 ms | 0.034 ms |
  | `command_on_100k_notes` | 0.91–1.56 ms | 0.61 ms |
  | `type_50kb_frame_p95` | 1.7 ms | 1.7 ms (egui text layout; a candidate for one text field per block) |

- Notes:
  - The new matching differs from `str::to_lowercase` only for a word-final Greek sigma.
  - Typing now scrolls the list only when the filter's contents change.
  - `cargo deny advisories` already failed before this merge, on a libsqlite3-sys CVE and a yanked
    async-executor. That is tracked here and not fixed yet.
- Wave 2 started: wt-controls (IMP-5.1, 5.11, 5.13) and wt-toasts (IMP-5.2, IMP-1.6).

## 2026-11-10: wave 1 started

Worktrees under `.claude/worktrees/` were branched from `imp` at 0f3d6dd. Each agent owns the files
listed in its brief, so merges stay mechanical.

| Agent | Branch | Tasks | Owns |
|---|---|---|---|
| wt-frame | `wt/frame` | IMP-1.1, IMP-1.5 (idle field, frames per command), IMP-2.1, IMP-2.2 | `crates/app`, the caret fix, `examples/notes/tests/wake.rs`, one fn in `examples/notes/tests/perf.rs` |
| wt-notes | `wt/notes` | IMP-1.4, IMP-1.5 (typing, resize, select-last), IMP-2.5 | `examples/notes/src`, the rest of `examples/notes/tests/perf.rs`, new scroll bench |
| wt-uiperf | `wt/uiperf` | IMP-1.3, IMP-2.4, IMP-2.3, then IMP-2.6 | `crates/ui/src/{cache,markdown,virtual_list}.rs`, `crates/ui/tests/perf.rs` |
| wt-motion | `wt/motion` | IMP-3.1 | `crates/motion`, `LAYERS`, the architecture table |
| wt-tools | `wt/tools` | IMP-4.1, IMP-4.2 (MCP part), IMP-4.3 | `crates/debug`, `crates/mcp`, protocol docs |

Expected merge conflicts: CHANGELOG.md (every branch adds lines), `Cargo.lock`.

## 2026-11-10: audit and plan (IMP-0.1)

- Six read-only auditors ran in parallel, one per area: rendering, motion, components, agent tooling,
  platform and measurement. The results are in AUDIT.md.
- The baseline came from `cargo xtask bench` at 393afe0, on an Apple M5 Pro through memgate (peak 2.57
  GB, 3m44s). The numbers are in AUDIT.md §1. A future `--compare` needs this baseline file, so keep a
  copy as `target/bench/baseline-393afe0.json` on the integrating machine.
- The reference framework was **not built**: building downloaded code needs the owner's go-ahead.
  Its head-to-head column uses its published numbers. Measuring it is IMP-1.9.
- Decision: no gate in the repository checks for the other framework's name; the owner asked for it
  not to be committed. Every staged diff and commit message is checked by hand instead.
- Decision: measure first. Phase 1 baselines (IMP-1.4, IMP-1.5) must land before the fixes they
  measure (IMP-2.1, 2.2, 2.5, 2.8).
- Verified by re-reading the code before planning:
  - the caret-blink repaint (egui `paint_text_cursor`)
  - per-frame `update_title`/`persist_view` work (`crates/app/src/runtime.rs:768-823`)
  - the linear `Notes::get` (`examples/notes/src/lib.rs:38-40`)
