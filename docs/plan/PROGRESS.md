# Progress log

Newest first. Each entry has the date, the task ids it covers, what changed, numbers before and after,
and any decision or surprise.

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
