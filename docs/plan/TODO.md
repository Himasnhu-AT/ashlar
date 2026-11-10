# TODO

Status values: `todo` · `doing (<owner>)` · `review` · `done (<commit>)` · `blocked (<why>)` ·
`dropped (<why>)`. Details for each id are in TASKS.md. Update this file in the same commit that changes
a task's state.

## Phase 0: plan
| Id | Task | Status |
|---|---|---|
| IMP-0.1 | Audit and plan files | done (0f3d6dd) |
| IMP-0.2 | `cargo xtask names` skips `.claude` (agent worktrees are whole checkouts) | done (845a519) |
| IMP-0.3 | Dependency advisories (libsqlite3-sys CVE, yanked async-executor) | done (f0f8e63) |
| IMP-0.4 | `ashlar-trace` edge test: 500 ms wall-clock init limit fails on a loaded machine | done (7729be3) |

## Phase 1: measure the whole frame
| Id | Task | Status |
|---|---|---|
| IMP-1.1 | Full-frame measurement in `Driver` (tessellate, scale 2, phase split) | done (0a78f56) |
| IMP-1.2 | Allocation gate in perf tests | doing (wt-alloc) |
| IMP-1.3 | `ashlar-ui` perf suite | done (c259234) |
| IMP-1.4 | Plain-list scroll benchmark (the apples-to-apples workload) | done (dfc6740) |
| IMP-1.5 | Notes interaction benches (typing, resize, select-last, frames per command, idle focused field) | done (726b637, d5bd509) |
| IMP-1.6 | 120 Hz frame budget in telemetry | done (e7735da) |
| IMP-1.7 | Binary size and idle memory metrics, `examples/hello` | todo |
| IMP-1.8 | Launch to first present | todo |
| IMP-1.9 | Complete the head-to-head table | blocked (needs the owner's OK to build the reference framework) |

## Phase 2: performance
| Id | Task | Status |
|---|---|---|
| IMP-2.1 | No caret-blink frames when idle | done (29decb2) |
| IMP-2.2 | Runtime per-frame waste (title, view persist, double wake) | done (56afafc) |
| IMP-2.3 | Markdown cache and off-screen culling | done (cc70f84) |
| IMP-2.4 | Galley cache hot path | done (342ab80) |
| IMP-2.5 | Notes example O(n) paths | done (0a5775a) |
| IMP-2.6 | Incremental offsets for variable-height lists | done (a913bcd) |
| IMP-2.7 | Debug-only perf warnings | todo |
| IMP-2.8 | Non-virtualized content cost | doing (wt-rows) |
| IMP-2.9 | Binary size | todo |
| IMP-2.10 | Idle memory | todo |
| IMP-2.11 | Launch time | todo |
| IMP-2.12 | Per-frame JSON log | todo |
| IMP-2.13 | Telemetry incident rules | todo |
| IMP-2.14 | One frame per ashlar wake (egui adds a second frame to zero-delay repaints) | done (0f32c3d) |
| IMP-2.15 | `serve_control` wake after answering: needed? (doubles frames per engine command) | done (861b535) |

## Phase 3: motion and theming
| Id | Task | Status |
|---|---|---|
| IMP-3.1 | `ashlar-motion` crate | done (ee02511) |
| IMP-3.2 | `ashlar_ui::motion` | done (ce86186) |
| IMP-3.3 | OS preferences: reduced motion, accent, contrast | doing (wt-theme) |
| IMP-3.4 | Token upgrades (OKLCH, motion, elevation, contrast gate) | doing (wt-theme) |
| IMP-3.5 | Animated theme switch | todo |
| IMP-3.6 | Presence and component adoption | todo |
| IMP-3.7 | Gesture handoff | todo |
| IMP-3.8 | `Cx` API polish | todo |

## Phase 4: agent tooling
| Id | Task | Status |
|---|---|---|
| IMP-4.1 | Fix `trace` with `--port`/`--app` | done (5ef4d81) |
| IMP-4.2 | MCP `app_stats`/`app_logs`, process numbers | doing (MCP tools done 550ce99; process numbers in `app.stats` still todo) |
| IMP-4.3 | Control-protocol doc drift | done (1084a69) |
| IMP-4.4 | Headless wgpu renderer for `Driver` | done (c2d7270) |
| IMP-4.5 | In-process `.ui` scripts on virtual time | doing (wt-uiscript) |
| IMP-4.6 | `--headless --snap --tree` flags | doing (wt-uiscript) |
| IMP-4.7 | `app.clock` | todo |
| IMP-4.8 | App-level golden snapshots | todo |
| IMP-4.9 | Named scenes | todo |
| IMP-4.10 | Motion and jank gate | todo |
| IMP-4.11 | Record and replay | todo |

## Phase 5: components
| Id | Task | Status |
|---|---|---|
| IMP-5.1 | Basic controls kit | done (10ee980) |
| IMP-5.2 | Toast v2 | done (aaf674c) |
| IMP-5.3 | List and tree accessibility and selection | todo |
| IMP-5.4 | Markdown v2 | todo (after IMP-2.3) |
| IMP-5.5 | `ashlar-syntax` and `CodeView` | todo |
| IMP-5.6 | Diff view | todo |
| IMP-5.7 | Chat kit | todo |
| IMP-5.8 | Settings form pattern | todo |
| IMP-5.9 | Overlays (select, combobox, popover, tooltip, context menu) | todo |
| IMP-5.10 | Standalone tab strip | todo |
| IMP-5.11 | Dialog focus | done (9fcbdd7) |
| IMP-5.12 | Rolling numbers | todo (after IMP-3.2) |
| IMP-5.13 | Small content components | done (05f44af) |
| IMP-5.14 | Sidebar / source list | todo |
| IMP-5.15 | Accessibility goldens | todo |
| IMP-5.16 | Image viewer, tuning panel, diagrams | todo (later) |
| IMP-5.18 | Markdown follow-ups: visual snapshot, `show` with a revision (no full-text hash per frame), O(n²) `inline()`, relayout on width change | todo |
| IMP-5.19 | Controls follow-ups: pixel pairs for progress circle and spinner, skeleton rows, exclusive accordion, disabled options, the ⇧ keycap glyph | todo |
| IMP-5.17 | Toast leftovers: motion, F6 to the toast region, runtime runs action commands, gallery page | todo (motion after IMP-3.2) |

## Phase 6: platform
| Id | Task | Status |
|---|---|---|
| IMP-6.1 | Native menu fixes (Windows attach, typing and modal guards, standard macOS menus) | done (95924f5) |
| IMP-6.17 | Native menu follow-ups: Windows accelerators through a message hook (needs an ADR 0001 module), run the Windows attach path on a real Windows machine | todo |
| IMP-6.2 | Open files, single instance, dropped files | todo |
| IMP-6.3 | OS preference watch | todo |
| IMP-6.4 | Window placement and viewport hook | todo |
| IMP-6.5 | Parented, non-blocking file dialogs | todo |
| IMP-6.6 | Notifications | todo |
| IMP-6.7 | Dock Quit, logout and shutdown | todo |
| IMP-6.8 | Web target gate | doing (gate and warnings done 766bdf8; web example and DOM error todo) |
| IMP-6.9 | Honest launch timing and ready signal | todo |
| IMP-6.10 | Open command-line document after first frame | todo |
| IMP-6.11 | Tray / status item | todo |
| IMP-6.12 | Rich clipboard, secret copy | todo |
| IMP-6.13 | `ashlar-i18n` | todo |
| IMP-6.14 | macOS shell shims | todo |
| IMP-6.15 | Drag-out and native context menus | todo |
| IMP-6.16 | Global shortcuts and bring-to-front | todo |

## Phase 7: later
| Id | Task | Status |
|---|---|---|
| IMP-7.1 | Hot reload of views | todo (after phase 4) |
| IMP-7.2 | Present latency options | todo (measure first) |
