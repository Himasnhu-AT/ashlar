# Progress log

Newest first. Each entry has the date, the task ids it covers, what changed, numbers before and after,
and any decision or surprise.

## 2026-11-10: wt/headless merged (CI green)

- **IMP-4.4:** `Driver::with_renderer(runtime, Renderer::wgpu())`
  - A wgpu device with no surface: a software adapter first, predictable renderer options.
  - Screenshots go through the existing offscreen path, and textures are uploaded after every frame
    (`offscreen::apply_textures`).
  - With no adapter, on wasm, or when a render fails, the image is grey and the reply says
    `"renderer": "none"`. Device creation runs under `catch_unwind`.
  - Grey screenshots are now sized in pixels, not points: this fixes the wrong size at scales other
    than 1.
  - Tests are in `crates/app/tests/headless_render.rs`: real pixels, the button's colour and label,
    byte-identical repeats, size at 2×.
  - `perf_headless_screenshot` (2560×1600, including PNG and base64): 26 ms release.
- wt-uiscript started IMP-4.5 and 4.6.

## 2026-11-10: wt/wake merged (CI green)

| Metric (release) | Before | After IMP-2.14 | After IMP-2.15 |
|---|---|---|---|
| `frames_per_command` | 3.20 | 2.23 | 1.26 |
| `frames_per_key_command` | 3.00 | 3.00 | 3.00 |

- **IMP-2.14:** `wake::repaint_now` asks egui for a repaint 1 ns away, so egui no longer adds its
  second frame. Key commands stay at 3, because egui itself asks for a zero-delay repaint after any
  frame with input (`input_state/mod.rs:655`).
- **IMP-2.15:** `serve_control` no longer wakes after answering. The answering frame already draws
  the change, and debugger requests ask for their own frames. The new test
  `an_engine_command_is_drawn_by_the_frame_that_answers_it` covers this.
- **IMP-0.4:** the edge test now checks init against under 250 ms of thread CPU and under 5 s of wall
  time. Measured: at most 7.6 ms CPU and 125 ms wall in debug.
- wt-rows started IMP-2.8.

## 2026-11-10: wt/uiperf, wt/controls, wt/menu merged (CI green)

- **IMP-1.3, 2.3, 2.4, 2.6**, before → after (release; old and new binaries alternated on a loaded
  machine):

  | Metric | Before | After |
  |---|---|---|
  | `md_static_frame_p50` (52 KB, 319 blocks) | 0.944 ms | 0.015–0.031 ms |
  | `md_static_first_frame` | 16.2–17.2 ms | 11.4–13.3 ms |
  | `md_static_layout_jobs` | 115 | 6 |
  | `md_stream_frame_p95` (7 KB) | 0.183 ms | 0.11–0.36 ms |
  | `galley_hit` | 26 ns | 20 ns |
  | `galley_overflow_relayouts` (worst frame) | 4,160 | 0 |
  | `one_row_height_change_1m` | 1,388 µs | 0–1.3 µs |
  | `offsets_rebuild_1m` | 1.70–2.21 ms | 2.66–2.95 ms (the Fenwick tree's cost; a rare path) |

  Also measured: `tree_flatten_1m` 4.02 ms, `tree_scroll_1m_p95` 0.041 ms,
  `palette_keystroke_5k_p50` 1.55 ms.

  Behaviour changes:
  - Markdown automation ids exist only for blocks that are drawn.
  - `ahash` is a direct dependency, at the version egui already uses.
  - No markdown visual snapshot exists yet (IMP-5.18).
- **IMP-5.1, 5.11, 5.13:** the `controls` kit, `content` components and dialog focus.
  - Tests: 23 kittest tests, 282 visual pairs (12 broken references are caught), and gallery
    Controls and Content pages.
  - Changed: dialog buttons are now `Button`; a disabled one is drawn faded, and the default button
    no longer reports `selected`.
  - Reduced motion is a per-control flag until IMP-3.3.
- **IMP-6.1:**
  - Menu picks are queued and run only when no modal or palette is open.
  - `works_while_typing` leaves the undo, redo, clipboard and select-all chords to a focused text
    field.
  - Undo and Redo keep app commands but have no OS shortcut, because the standard roles would block
    document undo from the menu.
  - Quit is in the macOS app menu (`app.quit`, which asks about unsaved changes); there is a Window
    menu.
  - The Windows menu attaches after the first frame. This is compile-checked only.
  - Windows accelerators are not done: they need new FFI (IMP-6.17).
- **Wave 3 additions:** wt-alloc (IMP-1.2), wt-theme (IMP-3.4, 3.3), wt-headless (IMP-4.4).

## 2026-11-10: wt/uimotion and wt/deps merged; wave 3 started

- **IMP-3.2:** `ashlar_ui::motion` (value, f32, opacity, colour in OKLab, rect by edges, set, release,
  prefs, active).
  - Behaviour:
    - Slots are pruned after 120 unused frames.
    - The first call returns the target.
    - There is at most one ANIMATION wake per frame, and none once settled.
    - Under reduced motion, egui's `animation_time` is 0.
    - A new frame is detected from `i.time`; the pass counter cost an extra context lock per call.
  - Perf: `motion_value_1k_ids` 36 µs, `motion_settled_1k_ids` 17 µs (3.5× a plain ctx-data lookup),
    `idle_wakes_after_settle` 0.
- **IMP-0.3:**
  - rusqlite 0.40 → 0.41 (libsqlite3-sys 0.39, RUSTSEC-2026-0459)
  - async-executor 1.14.0 → 1.14.1 (the old one was yanked)
  - `cargo deny check advisories` now passes, with no ignores
- **IMP-6.8 (gate part):** `cargo xtask wasm` also checks `ashlar-ui` and `ashlar-app` (12 crates), and
  the wasm dead-code warnings are fixed.
- **CI:** the `names` gate caught a word in the new CHANGELOG line that contains an app name as a substring. The line was reworded in a34be3c.
- **Wave 3:**
  - wt-wake: IMP-2.14, IMP-2.15, IMP-0.4
  - wt-menu: IMP-6.1

## 2026-11-10: wt/toasts and wt/frame merged

- **IMP-5.2 toast v2 (additive API):**
  - each toast is an AccessKit live region (polite; assertive for errors)
  - title, up to three actions (read with `take_actions()`), a key that updates a toast in place,
    and progress toasts
  - Escape, focus pause, a focus ring, and a "+N more" pill
  - the labels live in a struct
  - a settled stack requests no repaint
- **IMP-1.6:** the 8.333 ms bucket bound already existed. New: `DISPLAY_BUDGET_US` and an optional
  `over_budget` field (schema 1, left out when unset), which the server recounts from the buckets.
  The dashboard shows "Over 120 Hz budget".
- **CI flake:** the first CI run on the toasts merge failed in `ashlar-trace` `edge.rs:102` (init took
  588 ms against a 500 ms limit, under load). Rerun alone it passed, and the full rerun was green.
  Logged as IMP-0.4.
- **IMP-1.1:** `Driver` tessellates, takes `pixels_per_point`, and splits stats into
  `ui`/`pass`/`tessellate`/`frame_cpu`. New metrics (noisy, 7 builds sharing the machine):

  | Metric | Value |
  |---|---|
  | `tessellate_p95_scroll_100k` | 0.014–0.055 ms |
  | `frame_cpu_p95_scroll_100k_x2` | 0.09–0.37 ms (gate 8.3 ms) |
  | `pass_p95_scroll_100k` | 0.08–0.28 ms |

  The scroll test now runs at 2× and includes tessellation, so it doesn't compare directly with the
  audit numbers.
- **IMP-2.1:** egui's blink was worse than the audit estimated: about 6.5 frames per second, since it
  asks one predicted frame early on every phase change. The runtime now blinks the caret itself
  (`crates/app/src/caret.rs`, wake reason `caret`), one frame per phase, solid after 10 s without
  input. `idle_frames_focused_field_30s` went from 194 (all credited to `input`) to 21, then 0.
- **IMP-2.2:** the view state and title are rebuilt only when they change, and key commands no
  longer ask for a `document` frame. Frames per command stay at 3.2 (engine) and 3.0 (key) because
  egui doubles zero-delay repaints. Follow-ups are IMP-2.14 and IMP-2.15.

## 2026-11-10: IMP-3.1 merged (wt/motion → imp, CI green)

- `crates/motion` (`ashlar-motion`) sits at rank 0, builds for wasm32 and has no dependencies. Tests:
  - 24 unit tests, 9 property tests and 1 doc test
  - the closed form matches RK4 within 1e-4 over 1,000 springs
  - retargets keep position and velocity continuous (|Δx| < 1e-4, |Δv| < 1e-3)
  - settle times are never early
  - NaN and infinite inputs are safe
  - OKLab round-trips within 1/512
- Perf (release, ±25 % because the machine was shared):

  | Metric | Measured | Target |
  |---|---|---|
  | `spring_sample` | 7.9 ns | 4 |
  | `animated_retarget_rect` | 185 ns (483 before tuning) | 60 |
  | `oklab_mix` | 36 ns | 25 |
  | `settle_time` | 145 ns | 150 |

  The next lever is f32 spring maths. Today it uses f64 so near-critical damping stays accurate.
- Choices:
  - `Class::Other` jumps under reduced motion.
  - `INSTANT` exits in 0.08 s.
  - `Animated` assumes pixels-per-point is 2 until told otherwise.
  - There is no serde feature yet.
- wt-uimotion started IMP-3.2.

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
