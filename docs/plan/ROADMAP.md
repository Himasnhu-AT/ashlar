# Roadmap

Ordered by what unblocks what. Measurement comes first: a fix only counts once a number shows it.
Phases 1–4 run partly in parallel (different crates); see TODO.md for the live state and
TASKS.md for each task's acceptance criteria.

| Phase | Goal | Exit criteria |
|---|---|---|
| 0. Plan | Audit written, tasks defined, baseline recorded | AUDIT.md, TASKS.md, TODO.md, PROGRESS.md committed on `imp` |
| 1. Measure the whole frame | Perf tests see what users pay: tessellation, scale 2, allocations, typing, resize, markdown, plain lists, size, memory, launch | Every metric named in phase 2 has a `perf_*` test printing `bench:` lines, and `cargo xtask bench` runs them |
| 2. Performance | Fix what phase 1 shows: idle caret, runtime per-frame waste, markdown, galley cache, O(n) paths, binary size, memory, launch | Each fix lands with before and after numbers in PROGRESS.md and no regression over 5 % elsewhere (`cargo xtask bench --compare`) |
| 3. Motion and theming | Springs with velocity-keeping retargets, reduced motion, OKLCH tokens, OS accent and contrast, animated toasts, tabs, dialogs, theme switch | `ashlar-motion` at rank 0 (wasm32); settled animations request zero frames (tested); reduced motion honoured (tested) |
| 4. Agent tooling | Pixels without a window, deterministic in-process scripts, clock control, snapshots, scenes, motion gate | `my-app --headless --snap out.png --tree tree.json` works for the examples; the notes debug test runs with no thread and no wall-clock sleep |
| 5. Components | Everyday controls, toast v2, accessible lists, markdown v2, code and diff views, chat kit, settings forms | Each component has automation ids, AccessKit roles, kittest behaviour tests and a gallery page; settled components request zero frames |
| 6. Platform | Fix the native menu bugs, connect the native crate's tested pieces (open files, single instance, drops, preferences), window placement, notifications, i18n | Bugs in AUDIT 6.1–6.4 have regression tests; a dropped or Finder-opened file reaches `file.open` |
| 7. Later | Large or unproven items: hot reload, record and replay, present latency | Decided after phases 1–4 report numbers |

## Waves

Work runs in waves of parallel agents. Each agent gets its own worktree and touches only its own files.
The integrator merges into `imp` after `cargo xtask ci`, then updates TODO.md and PROGRESS.md.

- **Wave 1:**
  - frame measurement and runtime waste (`ashlar-app`)
  - notes example O(n) paths and their perf tests
  - `ashlar-ui` perf suite with markdown and galley cache fixes
  - the `ashlar-motion` crate
  - small agent-tooling fixes (trace bug, MCP stats and logs, doc drift)
- **Wave 2:**
  - allocation gate
  - headless wgpu `Driver`
  - in-process `.ui` scripts
  - `ui::motion` with OS preferences
  - token upgrades
  - binary size and memory work
  - first component tasks
- **Wave 3:**
  - `--snap/--tree` flags
  - clock control
  - snapshots and scenes
  - motion adoption in components
  - motion gate
  - platform tasks
