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
| IMP-0.5 | `cargo deny check licenses` fails: `deny.toml` has no `[licenses]` allow list (pre-existing) | doing (wt-rowuse) |
| IMP-0.6 | `ashlar-trace`: a released spool re-adopted its own folder as an orphan (found through a telemetry test) | done (3e2c995) |
| IMP-0.7 | Pipeline-level shutdown test (a transport that blocks mid-send, then `shutdown_within` times out) | todo (optional) |
| IMP-0.4 | `ashlar-trace` edge test: 500 ms wall-clock init limit fails on a loaded machine | done (7729be3) |

## Phase 1: measure the whole frame
| Id | Task | Status |
|---|---|---|
| IMP-1.1 | Full-frame measurement in `Driver` (tessellate, scale 2, phase split) | done (0a78f56) |
| IMP-1.2 | Allocation gate in perf tests | done (1f1096d) |
| IMP-1.3 | `ashlar-ui` perf suite | done (c259234) |
| IMP-1.4 | Plain-list scroll benchmark (the apples-to-apples workload) | done (dfc6740) |
| IMP-1.5 | Notes interaction benches (typing, resize, select-last, frames per command, idle focused field) | done (726b637, d5bd509) |
| IMP-1.6 | 120 Hz frame budget in telemetry | done (e7735da) |
| IMP-1.7 | Binary size and idle memory metrics, `examples/hello` | done for size (4e91720); idle memory probe moved to IMP-2.10 |
| IMP-1.8 | Launch to first present | done (wt/launch, 4356a62) |
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
| IMP-2.7 | Debug-only perf warnings | doing (wt-perfwarn) |
| IMP-2.8 | Non-virtualized content cost | done (bfcafc7) |
| IMP-2.9 | Binary size | done (c8e3e97) |
| IMP-2.10 | Idle memory | done (wt/launch, 4356a62) |
| IMP-2.11 | Launch time | dropped (eframe 0.36 already creates the window hidden until the first paint) |
| IMP-2.12 | Per-frame JSON log | doing (wt-perfwarn) |
| IMP-2.13 | Telemetry incident rules | todo |
| IMP-2.16 | Cut per-frame allocations (idle wake frame: 954 vs 29 for bare egui) | done (1b0c572, 48fe599) |
| IMP-2.18 | Allocation regression when IMP-5.3 and IMP-2.16 combine: idle 550 → 652 (gate 578), keystroke 1,935 → 2,246 | done (3ad0ac1, ef6c1a4) |
| IMP-2.17 | Use `list::Row` in ashlar's own lists and the notes example; call `list::checked` from ashlar scroll containers | doing (wt-rowuse) |
| IMP-2.14 | One frame per ashlar wake (egui adds a second frame to zero-delay repaints) | done (0f32c3d) |
| IMP-2.15 | `serve_control` wake after answering: needed? (doubles frames per engine command) | done (861b535) |

## Phase 3: motion and theming
| Id | Task | Status |
|---|---|---|
| IMP-3.1 | `ashlar-motion` crate | done (ee02511) |
| IMP-3.2 | `ashlar_ui::motion` | done (ce86186) |
| IMP-3.3 | OS preferences: reduced motion, accent, contrast | done (2089b8c) |
| IMP-3.4 | Token upgrades (OKLCH, motion, elevation, contrast gate) | done (8ec7a04) |
| IMP-3.5 | Animated theme switch | doing (wt-animate (if time)) |
| IMP-3.9 | `motion::f32` tween with `Easing::LINEAR` reached about 12 % at 26 % of its duration (reported by wt-clock): check against a unit test | done (c657940) |
| IMP-3.6 | Presence and component adoption | done (378458b..ea70000) |
| IMP-3.7 | Gesture handoff | todo |
| IMP-3.8 | `Cx` API polish | todo |

## Phase 4: agent tooling
| Id | Task | Status |
|---|---|---|
| IMP-4.1 | Fix `trace` with `--port`/`--app` | done (5ef4d81) |
| IMP-4.2 | MCP `app_stats`/`app_logs`, process numbers | doing (MCP tools done 550ce99; process numbers: wt-perfwarn) |
| IMP-4.3 | Control-protocol doc drift | done (1084a69) |
| IMP-4.4 | Headless wgpu renderer for `Driver` | done (c2d7270) |
| IMP-4.5 | In-process `.ui` scripts on virtual time | done (3669d42) |
| IMP-4.6 | `--headless --snap --tree` flags | done (11a4853) |
| IMP-4.7 | `app.clock` | done (221164b) |
| IMP-4.8 | App-level golden snapshots | done (434ecf6) |
| IMP-4.9 | Named scenes | done (24f06e4) |
| IMP-4.10 | Motion and jank gate | done (20796ea) |
| IMP-4.11 | Record and replay | todo |

## Phase 5: components
| Id | Task | Status |
|---|---|---|
| IMP-5.1 | Basic controls kit | done (10ee980) |
| IMP-5.2 | Toast v2 | done (aaf674c) |
| IMP-5.3 | List and tree accessibility and selection | done (b2775e5, fix 9bc416c) |
| IMP-5.4 | Markdown v2 | doing (wt-md2) |
| IMP-5.5 | `ashlar-syntax` and `CodeView` | done (4473fe0, c7311b4) |
| IMP-5.6 | Diff view | doing (wt-diff) |
| IMP-5.7 | Chat kit | doing (wt-chat) |
| IMP-5.8 | Settings form pattern | done (de225b3) |
| IMP-5.9 | Overlays (select, combobox, popover, tooltip, context menu) | done (5885b46) |
| IMP-5.10 | Standalone tab strip | done (2a523df) |
| IMP-5.11 | Dialog focus | done (9fcbdd7) |
| IMP-5.12 | Rolling numbers | todo (after IMP-3.2) |
| IMP-5.13 | Small content components | done (05f44af) |
| IMP-5.14 | Sidebar / source list | todo |
| IMP-5.15 | Accessibility goldens | todo |
| IMP-5.20 | Follow-ups to IMP-3.6 and 5.5: use `tabs::indicator` in docking and the tab strip; check the cost of the dialog's per-frame shape copy (up to 4,096 shapes) with the allocation bench; CodeView gallery page, horizontal scrolling, a Document automation role | todo |
| IMP-5.16 | Image viewer, tuning panel, diagrams | todo (later) |
| IMP-5.18 | Markdown follow-ups: visual snapshot, `show` with a revision (no full-text hash per frame), O(n²) `inline()`, relayout on width change | doing (wt-md2) |
| IMP-5.19 | Controls follow-ups: pixel pairs for progress circle and spinner, skeleton rows, exclusive accordion, disabled options, the ⇧ keycap glyph | todo |
| IMP-5.17 | Toast leftovers: motion, F6 to the toast region, runtime runs action commands, gallery page | todo (motion after IMP-3.2) |

## Phase 6: platform
| Id | Task | Status |
|---|---|---|
| IMP-6.1 | Native menu fixes (Windows attach, typing and modal guards, standard macOS menus) | done (95924f5) |
| IMP-6.17 | Native menu follow-ups: Windows accelerators through a message hook (needs an ADR 0001 module), run the Windows attach path on a real Windows machine | todo |
| IMP-6.2 | Open files, single instance, dropped files | done (b090f59) |
| IMP-6.3 | OS preference watch | done (2089b8c, with IMP-3.3) |
| IMP-6.4 | Window placement and viewport hook | todo |
| IMP-6.5 | Parented, non-blocking file dialogs | todo |
| IMP-6.6 | Notifications | todo |
| IMP-6.7 | Dock Quit, logout and shutdown | todo |
| IMP-6.8 | Web target gate | doing (gate and warnings done 766bdf8; web example and DOM error todo) |
| IMP-6.9 | Honest launch timing and ready signal | done (wt/launch, 4356a62) |
| IMP-6.10 | Open command-line document after first frame | done (wt/launch, 4356a62) |
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
