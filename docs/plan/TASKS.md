# Tasks

Each task: why (with the AUDIT.md section), where, what "done" means, and how it is measured.
Effort: S ≤ 1 day, M 2–4 days, L 1–2 weeks. Check every file:line before relying on it: the code
moves.

## Phase 0: plan

### IMP-0.1 Audit and plan files (S)
Write docs/plan/{README,AUDIT,ROADMAP,TASKS,TODO,PROGRESS}.md from the audit.
**Done:** committed on `imp`, with no other framework named.

## Phase 1: measure the whole frame

### IMP-1.1 Full-frame measurement in `Driver` (S–M)
Why: AUDIT 2.B. Today's frame numbers leave out tessellation and run at scale 1.
Where: `crates/app/src/driver.rs`, `crates/app/src/stats.rs`, `examples/notes/tests/perf.rs`.
Add `Driver::pixels_per_point(f32)` and a tessellate step (`ctx.tessellate(shapes, ppp)`). Split
`FrameStats` into `ui`, `pass` and `tessellate`. Native runs also record eframe's
`frame.info().cpu_usage`.
**Done:** the scroll perf test prints `tessellate_p95_scroll_100k` and
`frame_cpu_p95_scroll_100k_x2`, and the gate is the 8.3 ms budget at 120 Hz.

### IMP-1.2 Allocation gate in perf tests (S)
Why: AUDIT 2.C. Nothing counts allocations today.
Where: test binaries only. Use a counting global allocator from a crate (no `unsafe` in ashlar code),
check it against deny.toml, and add a helper in `ashlar-app` test support.
**Done:** `allocs_per_frame_scroll_100k` and `allocs_per_idle_wake_frame` are printed;
the idle wake frame is gated at ≤ a small constant.

### IMP-1.3 `ashlar-ui` perf suite (M)
Why: AUDIT 2.I. `ashlar-ui` has no perf tests.
Where: new `crates/ui/tests/perf.rs` on a headless `egui::Context`.
Tests: `perf_markdown_static_50kb`, `perf_markdown_stream_7kb`, `perf_galley_cache_hits` (with an
overflow case), `perf_virtual_list_variable_1m`, `perf_tree_1m_flatten_and_scroll`,
`perf_palette_filter_5k_commands`.
**Done:** all are picked up by `cargo xtask bench`, and their baselines are in PROGRESS.md.

### IMP-1.4 Plain-list scroll benchmark (S)
Why: AUDIT 1. This is the one apples-to-apples workload: 1,000 rows of 32 pt in a 760×800 viewport
at scale 2, 120 warm-up frames, then 480 wheel steps of 12 pt at 120 Hz, reversing halfway. ashlar
takes 0.63 ms p50 against 0.306 ms published for the reference framework.
Where: a `perf_` test (`examples/notes/tests` or `crates/app/tests`) with thread-CPU p50 and p95,
tessellation included, at 1k and 10k rows, plus the same workload through `VirtualList`.
**Done:** it prints `plain_scroll_1k_p50` and `plain_scroll_10k_p95`.

### IMP-1.5 Notes interaction benches (S)
Why: AUDIT 2.A, 2.D, 2.F.
Tests:
- `perf_select_last_of_100k`
- `perf_typing_with_query_100k`
- `perf_type_50kb_note`
- `perf_live_resize`
- `perf_frames_per_command`
- an idle-with-focused-field test that prints `idle_frames_focused_field_30s`
**Done:** the baselines are recorded *before* the fixes in IMP-2.1, 2.2 and 2.5 land.

### IMP-1.6 120 Hz frame budget in telemetry (S)
Why: AUDIT 2.B. The slow-frame bound is 16.7 ms, which is the 60 Hz budget.
Where: `crates/trace-proto/src/lib.rs` (additive 8.3 ms bound and an over-budget count).
**Done:** wire-format tests pass, and the change is additive only.

### IMP-1.7 Binary size and idle memory metrics (S)
Why: AUDIT 1. The stripped hello app is 12.76 MB, idle RSS is 107 MB and the footprint is 36.7 MB.
Where: a minimal `examples/hello`, plus a `cargo xtask bench` step that prints `hello_stripped_bytes`,
and a manual probe for footprint after a 10 s settle.
**Done:** both numbers are tracked in PROGRESS.md.

### IMP-1.8 Launch to first present (M)
Why: AUDIT 1, 2.B. `first_frame_ms` stops before the window and wgpu exist.
Where: `crates/app/src/runtime.rs` `run_native`. Report `launch_to_present_ms` in `app.stats`, and add
a launch-N-times probe.
**Done:** p50 and p95 over 9 warm launches are recorded.

### IMP-1.9 Complete the head-to-head table (S, needs the owner)
Why: AUDIT 1. The reference column is published numbers only.
Building the downloaded code needs the owner's explicit go-ahead.
**Done:** the AUDIT 1 table has measured numbers for build time, size, launch, idle and scroll on
this Mac.

## Phase 2: performance

### IMP-2.1 No caret-blink frames when idle (S)
Why: AUDIT 2.A, a bug against "idle apps do zero work".
Where: `crates/ui/src/tokens.rs` or `crates/app/src/runtime.rs`. After N seconds with no input, set
`text_cursor.blink = false` (the caret stays solid) and turn it back on with the next input. Credit
blink frames to their own reason, not to `input`.
**Done:** `idle_frames_focused_field_30s` ≤ 2·N; existing wake tests stay green.

### IMP-2.2 Runtime per-frame waste (S)
Why: AUDIT 2.D.
Where: `crates/app/src/runtime.rs`:
- key `persist_view` by `view_generation`
- key the title by `(dirty, file_generation)`
- wake `DOCUMENT` only when the revision changed after the view started drawing
**Done:** `frames_per_command` drops, there are fewer allocations per frame (once IMP-1.2 lands), and
the wake tests stay green.

### IMP-2.3 Markdown cache and off-screen culling (M)
Why: AUDIT 2.E.
Where: `crates/ui/src/markdown.rs`. Cache parsed blocks by content hash and each block's galley by
`(id, block, width, style)`. Skip laying out blocks outside the clip rect, using cached heights. When
text streams in, re-parse only from the last closed block.
**Done:** `md_static_frame_p50` and `md_static_allocs` fall by an order of magnitude against the
IMP-1.3 baseline.

### IMP-2.4 Galley cache hot path (S)
Why: AUDIT 2.G.
Where: `crates/ui/src/cache.rs`:
- fetch the shared cache once per pass, with no write lock per label
- use a fast hasher
- evict the oldest half instead of clearing everything
**Done:** `galley_hit` improves, and `galley_overflow_relayouts` no longer spikes.

### IMP-2.5 Notes example O(n) paths (S)
Why: AUDIT 2.F. The perf showcase should model the right pattern.
Where: `examples/notes/src/lib.rs`:
- an id → index map per revision
- a filter key of `(revision, query hash)`
- matching with no allocation
- re-checking only the edited note
**Done:** `frame_p95_scroll_sel_last` is flat in N, and `keystroke_allocs` is O(1).

### IMP-2.6 Incremental offsets for variable-height lists (M)
Why: AUDIT 2.H.
Where: `crates/ui/src/virtual_list.rs`. Use a Fenwick tree or chunked prefix sums, and add a
per-row invalidation API.
**Done:** `one_row_height_change_1m` costs microseconds.

### IMP-2.7 Debug-only perf warnings (M)
Why: AUDIT 2, techniques. Turns AGENTS.md §3 into detectors.
Where: a new `perf_warn` module in `crates/app` (debug builds):
- a stall warning when wall time ≥ 8 ms and ≥ 4× thread CPU
- a warning when the same frame is redrawn repeatedly on non-input wakes
- `ASHLAR_FRAME_BUDGET_MS`
Each warns once per site; `ASHLAR_PERF_WARN=0` silences them.
**Done:** unit tests feed synthetic frames, and the scroll perf test emits no warnings.

### IMP-2.8 Non-virtualized content cost (M)
Why: AUDIT 1. A plain list costs ~0.7 µs per row per frame (2× the reference framework at 1k rows,
over budget at 10k).
Where: `crates/ui`. First profile where the 0.63 ms goes, from IMP-1.4 numbers. Then cut it: cheaper
row widgets and cached galleys for static labels, and a debug warning when a `ScrollArea` holds more
than N widgets, pointing to `VirtualList`.
**Done:** `plain_scroll_1k_p50` ≤ 0.35 ms, or a documented reason it cannot get there.

### IMP-2.9 Binary size (M)
Why: AUDIT 1. A 12.76 MB stripped hello app.
Where: the umbrella crate features, `crates/app`. Attribute the size with `cargo bloat --crates`, put
`image`, `rfd`, `muda` and similar behind opt-in features where an app can live without them, and
offer a measured size profile.
**Done:** `hello_stripped_bytes` drops, and the default features are documented.

### IMP-2.10 Idle memory (M)
Why: AUDIT 1. Footprint 36.7 MB, peak 90 MB.
Where: break the number down with `vmmap`/`footprint` (fonts, wgpu staging, accesskit, egui memory),
then fix the largest item.
**Done:** a lower footprint and peak, recorded in PROGRESS.md.

### IMP-2.11 Launch time (M)
Why: AUDIT 1. ~70 ms of window and wgpu setup.
Where: `crates/app/src/runtime.rs` `run_native`. Keep the window hidden until the first frame is
drawn (no white flash), and measure any setup that can overlap.
**Done:** `launch_to_present` p95 improves against the IMP-1.8 baseline.

### IMP-2.12 Per-frame JSON log (M)
Why: AUDIT 2, techniques.
Where: `crates/app`, behind `ASHLAR_FRAME_LOG=FILE`. Each line holds wall time, CPU, phases,
allocations, wake reasons and commands. The file size is bounded.
**Done:** an integration test checks the line count and schema.

### IMP-2.13 Telemetry incident rules (M)
Why: AUDIT 2, techniques. Add rules for 3 long frames within 1 s, a stall ≥ 250 ms, and memory
growth over minutes, deduplicated.
Where: `crates/trace/src/signals`.
**Done:** unit tests run on synthetic streams.

## Phase 3: motion and theming

### IMP-3.1 `ashlar-motion` crate (M)
Why: AUDIT 3.
Where: new `crates/motion`, rank 0, wasm32, no dependencies. Add it to `LAYERS`,
docs/architecture.md and `[workspace.dependencies]`.
Contents:
- closed-form springs set by `(duration, bounce)`, with clamped constructors that never panic
- `Curve` (Spring, Tween with cubic-bezier, Decay, Instant)
- `Animated<T>`, whose retargets keep position and velocity
- `Class`, carrying settle thresholds and reduced-motion rules (effects never overshoot)
- motion tokens, where exits are shorter than enters
- an OKLab/OKLCH colour module with WCAG contrast
- gesture helpers (velocity tracker, projection, rubber band)
**Done:** these tests pass:
- the closed form matches RK4 in all three regimes
- a property test shows retargets keep position and velocity continuous
- settle times are never early
- NaN and infinite inputs are safe
- OKLab round-trips within 1/512 on 4,096 random colours
- `cargo xtask wasm` passes
The perf test prints `spring_sample`, `animated_retarget_rect` and `oklab_mix`.

### IMP-3.2 `ashlar_ui::motion` (M)
Why: AUDIT 3.
Where: `crates/ui/src/motion.rs`. Add `value`, `f32`, `opacity`, `color` (OKLab), `rect` (animated by
edges), `set`, `release`, `prefs`, `set_prefs` and `active`. Values live in context data and are
pruned when unused. Nothing animates on first show, and frames are requested only while values are
unsettled.
**Done:** a test shows ANIMATION wakes stop on the frame after a value settles; the reduced-motion,
time-scale and velocity-continuity tests pass; and `motion_settled_1k_ids` is close to a map lookup.

### IMP-3.3 OS preferences: reduced motion, accent, contrast (S)
Why: AUDIT 3.
Where: `crates/app/src/runtime.rs` startup and window focus. Add the `App` builder flags `.follow_system_accent` and
`.motion`. The status-bar sweep holds still under reduced motion.
**Done:** tests run through an injected preference seam.

### IMP-3.4 Token upgrades (M)
Why: AUDIT 3.
Where: `crates/ui/src/tokens.rs`:
- derive state colours in OKLCH
- add motion, elevation and stroke tokens
- add `high_contrast()`
- add a table that documents the OKLCH source of each colour
- optional: themes loaded from JSON
**Done:** a WCAG contrast gate passes for both themes and for 64 random accents, and a hue-stability
test passes.

### IMP-3.5 Animated theme switch (M)
Why: AUDIT 3. Switch with an OKLab crossfade; text colour leads its background at 0.5× the duration.
**Done:** contrast holds at every sampled frame, and wakes stop once the switch settles.

### IMP-3.6 Presence and component adoption (M–L, one component per commit)
Why: AUDIT 3.
Animate:
- toasts: enter, exit, and the others gliding into the gap
- the tab indicator
- the docking drop preview
- dialog entry
- hover and press fills
Hit testing and automation rects stay at the resting layout. Visual regression tests run with reduced
motion.

### IMP-3.7 Gesture handoff (S)
Why: AUDIT 3. Value fields, sliders and lists (opt-in) get fling and rubber band.

### IMP-3.8 `Cx` API polish (S each)
Why: AUDIT 3.
Items:
- `cx.tokens()`
- `cx.motion(..)`
- an edit origin (user, drag, undo, agent, job) for motion policy
- layout helpers that take space tokens
- `Radii` as `f32`
- typed, cached view keys (measure first)

## Phase 4: agent tooling

### IMP-4.1 Fix `trace` with `--port`/`--app` (S)
Why: AUDIT 4.1, a bug.
Where: `crates/debug/src/verbs.rs:767-810` and `batch.rs`. Keep trace state per batch run. Outside
a batch, return a usage error.
**Done:** a CLI test runs `--port P batch -` with `trace start <tmp>`, and the trace files exist.

### IMP-4.2 MCP `app_stats` and `app_logs`; process numbers in `app.stats` (S)
Why: AUDIT 4.5.
Where: `crates/mcp/src/tools.rs`. Process memory and CPU go into `app.stats` (`crates/app`) after
wave 1's `ashlar-app` work merges.
**Done:** conformance tests list the tools only when the backend reports them.

### IMP-4.3 Control-protocol doc drift (S)
Why: AUDIT 4.6.
Where: `docs/control-protocol.md`. Make it match `crates/mcp/src/tools.rs` and
docs/debugging-uis.md.

### IMP-4.4 Headless wgpu renderer for `Driver` (M)
Why: AUDIT 4.2.
Where: `crates/app/src/driver.rs` and `offscreen.rs`. Create a render state with no surface. When no
adapter is available, fall back to the grey image and report `"renderer": "none"`.
**Done:** a notes test checks the PNG is not uniform, a known button has the accent colour, and two
renders are identical on the same machine.

### IMP-4.5 In-process `.ui` scripts on `Driver` with virtual time (M)
Why: AUDIT 4.3.
Where: a transport trait in `crates/debug`, implemented by `Driver`. Add `Driver::run_ui(script)`.
`sleep` advances virtual time.
**Done:** the notes debug test runs with no thread and no sleep, and two runs give identical step
JSON.

### IMP-4.6 `--headless --snap --tree --script --out` flags for every app (M)
Why: AUDIT 4.2. Gives one-command pixels and a tree, with no window.
**Done:** an end-to-end test checks the PNG size and that the tree contains `notes.add`.

### IMP-4.7 `app.clock` control method (M)
Why: AUDIT 4.4. Pause, step and resume time in a live app. Input still runs while paused.
Add a debug verb and an MCP tool.
**Done:** a paused animation gives identical rects; a step changes them.

### IMP-4.8 App-level golden snapshots (M)
Why: AUDIT 4.7.
Add `assert screenshot <name>`, baselines with adapter metadata, `ASHLAR_UPDATE_SNAPSHOTS=1`, and
diff images. A baseline from another adapter is skipped. Evaluate egui_kittest snapshots first.

### IMP-4.9 Named scenes (S–M)
Why: AUDIT 4.7.
Add `App::scene(name, setup)`, `--scene` and `--list-scenes`, and a `ui.scene` control method with an
MCP tool.

### IMP-4.10 Motion and jank gate (M)
Why: AUDIT 3, 4.7.
Record rects per frame on `Driver`, then flag pops, overshoot of effect values, reversals, and frames
requested after settle. Shares its recorder with IMP-3.2.

### IMP-4.11 Record and replay (M–L)
Why: AUDIT 4.7.
`ASHLAR_RECORD=path` writes a bounded ring of input and time; `Driver::replay(path)` plays it back.
**Done:** a replay gives equal revisions and trees.

## Phase 5: components

Rules for every component:
- It takes state by reference and reports intent.
- It registers dotted automation ids and AccessKit roles and states.
- It requests zero frames once settled, and snaps under reduced motion.
- No `unwrap`/`unsafe`; hostile sizes are clamped; long content is virtualized.
- It gets a gallery page.
- It has `egui_kittest` behaviour tests (keyboard, pointer, automation, AccessKit), plus
  reference-painter pairs in `crates/ui/tests/visual_regression.rs` for painted controls.
- Its strings come from a labels struct (IMP-6.13).

### IMP-5.1 Basic controls kit (M)
Why: AUDIT 5.
Where: `crates/ui/src/controls/`.
Contents:
- `Button` with Primary, Secondary, Ghost and Destructive variants, sizes and a loading state;
  dialogs move onto it
- `Switch`, `Segmented` and `RadioGroup`, with roving focus and one Tab stop
- `Badge`, `Keycap`
- `Progress::bar`/`circle` and `Spinner`

### IMP-5.2 Toast v2 (M)
Why: AUDIT 5.
Changes:
- live regions (polite, or assertive for errors)
- title, description and action; a key that updates a toast in place
- progress toasts
- Escape dismisses
- keyboard focus pauses the timer
- "+N more"
- motion, once IMP-3.2 lands

### IMP-5.3 List and tree accessibility and selection (M)
Why: AUDIT 5.
Changes:
- an AccessKit node per visible row (role, name, selected, expanded, level, position in set)
- an app-owned `Selection` with Shift and ⌘ ranges and ⌘A
- type-ahead
- optional reordering
**Done:** fewer than 60 nodes at 100k rows.

### IMP-5.4 Markdown v2 (M; after IMP-2.3)
Why: AUDIT 5.
Changes:
- focusable links with a link role and hit rects on every row
- content-sized tables with an "N more rows" footer
- highlighted code fences (IMP-5.5)
- find, and task-checkbox callbacks
- streaming repair of unclosed markers
- labels from IMP-6.13

### IMP-5.5 `ashlar-syntax` and `CodeView` (L)
Why: AUDIT 5.
`ashlar-syntax` is a rank-0, wasm32 crate with forward-only lexers for about 15 languages behind a
`Highlighter` trait. A bigger engine is optional and off by default, because binary size matters.
`CodeView` has virtualized lines, a gutter, wrap, selection and copy.
**Done:** a lexer property test (no panic, ordered ranges, linear time) and
`bench: highlight_1k_lines`.

### IMP-5.6 Diff view (L)
Why: AUDIT 5.
Work: a Myers diff with a cost cap, word-level diffs, unified-patch parsing, and unified and split
modes with collapsed ranges.
**Done:** frame cost on a 50k-line diff stays flat, and malformed patches never panic.

### IMP-5.7 Chat kit: transcript, message, tool card, composer, popup list (L)
Why: AUDIT 5. It is UI over plain data, with no dependency on `ashlar-assistant` types.
Behaviour:
- the view stays pinned to the bottom while text streams in
- 10k messages cost O(visible)
- composition (IME) never submits

### IMP-5.8 Settings form pattern (M)
Sections of rows (label, description, control), modified and reset, a filter, and field errors.
Ids are `<section>.<row>`.

### IMP-5.9 Overlays: select, combobox, popover, tooltip with a delay token, context menu from `MenuSpec` (M)

### IMP-5.10 Standalone tab strip (M)
Extracted from docking. One Tab stop with arrow keys, close, overflow and reorder, plus automation
ids. Also fixes the missing automation on `Tab`.

### IMP-5.11 Dialog focus (S)
Initial focus goes to the default button or the first field; focus returns to the opener on close;
a description node.

### IMP-5.12 Rolling numbers (S; after IMP-3.2)

### IMP-5.13 Small content components (S each)
Empty state, banner or callout, card, collapsible or accordion, skeleton rows, and tooltips for
icon buttons.

### IMP-5.14 Sidebar / source list (M)

### IMP-5.15 Accessibility goldens (S)
Snapshot the AccessKit subtree of each gallery page to JSON and compare it with a stored copy;
`ASHLAR_BLESS=1` updates the stored copies.

### IMP-5.16 Later
Image viewer, a live token tuning panel, native diagrams (low value for the cost).

## Phase 6: platform

FFI marks a task that needs `unsafe` in `ashlar-native` under ADR 0001, one named module at a time.

### IMP-6.1 Native menu fixes (S–M; Windows part FFI, in the existing exemption)
Why: AUDIT 6.1–6.4.
Where: `crates/app/src/native_menu.rs`, `runtime.rs:430-433, 569-586`, `crates/native/src/menu`.
Changes:
- Attach the menu to the window on Windows. The native bar counts as active only once attached.
- Translate Windows accelerators in a message hook.
- Drain native ids only when no modal is open.
- Native chords obey `works_while_typing`.
- macOS: Edit items use the standard roles (text undo while typing), Quit goes in the app menu, and
  a Window menu is added.
**Done:**
- unit test: inactive when attach fails
- `Driver` test: ⌘Z while typing never reaches `app.undo`
- `Driver` test: a queued native id under the confirm modal does not run

### IMP-6.2 Open files, single instance, dropped files (M)
Why: AUDIT 6.6.
Add `App::single_instance(true)` and `App::accept_drops(true)`. A forwarded launch focuses the window.
Dropped files with a known extension open as documents; anything else becomes an effect the app can
handle.
**Done:** `Driver` tests for dropped files and forwarded args.

### IMP-6.3 OS preference watch (S; shared with IMP-3.3)
Why: AUDIT 6.6.
Add `system::watch(waker)`: the portal signal on Linux, off the UI thread; a re-read on focus on
macOS and Windows. Covers the Wayland system theme, the accent, reduced motion and contrast.
Optional FFI: Windows high contrast.

### IMP-6.4 Window placement and viewport hook (S)
Why: AUDIT 6.11.
Add `App::viewport(..)`, `App::icon(png)` and `App::remember_window(true)`. On launch, clamp the
saved placement to a visible monitor.
**Done:** unit tests of the clamp function, including off-screen and mixed-DPI cases.

### IMP-6.5 Parented, non-blocking file dialogs (M)
Why: AUDIT 6.7.
Parent each dialog to the window. Show it after the frame, and deliver the result on a later frame.

### IMP-6.6 Notifications: Windows toasts, macOS delegate, stable ids (M, FFI)
Why: AUDIT 6.9.
**Done:** tests for XML escaping, tag hashing and URL round trips.

### IMP-6.7 Dock Quit, logout and shutdown go through the dirty check (M, FFI)
Why: AUDIT 6.10.
**Done:** a pure quit state machine with unit tests.

### IMP-6.8 Web target gate (S, then M)
Why: AUDIT 6.5.
`cargo xtask wasm` also checks `ashlar-app`; fix the dead-code warnings; on a start failure, show a
visible error next to the canvas. Later: a web example and a size report.

### IMP-6.9 Honest launch timing and a ready signal (S; with IMP-1.8)
Why: AUDIT 6.11.
Take the process start time in `run_native`. Report time to window and time to first frame. A
ready-file environment variable gets `{pid, ready_ms}` written to it after the first present.

### IMP-6.10 Open the command-line document after the first frame (S)
Why: AUDIT 6.11.
**Done:** a `Driver` test shows the first frame comes before the open.

### IMP-6.11 Tray / status item (M)
`tray-icon` behind a `tray` feature, sharing `MenuSpec`.

### IMP-6.12 Rich clipboard, secret copy, long-lived Linux clipboard (M)
Why: AUDIT 6.8.
Add `set_rich`/`rich` (text, html, png, files) and `set_secret(text, clear_after)`. On Linux, keep
one clipboard on a worker thread.

### IMP-6.13 `ashlar-i18n` (M–L)
A rank-0, wasm-clean crate: build-time catalogs with key checks, plurals and number formatting.
ashlar's built-in strings move to an English layer that apps can override.

### IMP-6.14 macOS shell shims (S each, FFI)
- Survive a nil appearance during screen recording.
- Handle "reopen" from the Dock.
- Add a quiet launch for agent rebuild loops, so a relaunch doesn't steal focus.

### IMP-6.15 Drag-out and native context menus (M)

### IMP-6.16 Global shortcuts and bring-to-front (S–M)

## Phase 7: later

- IMP-7.1 Hot reload of views (L). Only after IMP-4.4–4.6 give a measured edit-to-pixel loop.
  Patched views run under `catch_unwind`.
- IMP-7.2 Present latency options (frame latency, present mode). Measure on a 120 Hz display first.
- Not planned: an embedded scripting VM. Commands plus MCP cover that need.
