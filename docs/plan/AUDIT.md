# Audit (2026-11-10)

ashlar at `393afe0` (branch `imp`) compared with a recently released native Rust UI framework (its
own GPU renderer, text, flexbox layout and springs; "the reference framework" below). Six read-only
auditors covered rendering and frame cost, motion and theming, components, agent tooling, platform
features, and measured numbers. Every finding below gives the ashlar file:line it was checked
against. Facts marked **verified** were re-read by the integrator before planning.

## 1. Measured numbers

Apple M5 Pro, macOS, release builds through memgate. Raw logs were kept in the session scratchpad.

### ashlar baseline (`cargo xtask bench`, 393afe0)

| Metric | Value |
|---|---|
| `notes_first_frame_wall` | 22.880 ms |
| `command_on_100k_notes` | 1.460 ms |
| `frame_p50_scroll_100k` / `p95` / `max` | 0.035 / 0.045 / 0.146 ms |
| `frame_wall_incl_egui_end_pass_scroll_100k` | 0.048 ms |
| `edit_100k_chunked` / `edit_100k_flat` | 515 / 12,632 ns |
| `snapshot_publish` / `read` / `load` | 74 / 2.3 / 3.4 ns |
| `edit_with_subscriber` | 550 ns |
| `undo_redo` / `undo_redo_10k_steps` | 14 / 19 ns |

### Head to head

The reference framework could not be built in the audit session (building downloaded code needs the
owner's go-ahead), so its column is its own published numbers from other machines (an M2 Max and an
M4 Pro under load). Only dependency count was measured here for both. Completing this table is
task IMP-1.9.

| Metric | ashlar (measured) | reference | Notes |
|---|---|---|---|
| Clean release build, hello app | 45.4 s wall, 380 CPU-s | not measured | naga 32 s, egui 22 s, wgpu-core 20 s, font stack ~57 s, image stack ~37 s |
| Incremental rebuild (touch main.rs) | 1.45 s | not measured | ashlar's release profile is not incremental |
| Stripped binary, hello app | 12.76 MB | target ≤ 5 MiB (published target) | image, png, rfd, muda, arboard, accesskit, full wgpu/naga all linked |
| Launch → window created | 89.8 ms median | not measured | |
| Launch → first UI pass | 91.9 ms median | 59–98 ms "process to pixels" (published, headless) | different boundaries; ~70 ms of ashlar's is window + wgpu setup |
| Idle CPU, 10 s | 0.0 % (0 ms CPU) | not measured | repaint on demand works (window was off screen) |
| Idle memory | RSS 107 MB, footprint 36.7 MB, peak 90 MB | target ≤ 50 MiB RSS (published target) | |
| Scroll frame CPU, 1,000 plain rows | 0.630 ms p50 / 0.656 p95 | 0.306 ms median (published) | **closest apples-to-apples pair; reference ~2× faster** |
| Scroll frame CPU, 10,000 plain rows | 6.98 ms p50 / 7.71 p95 | not measured | linear, ~0.7 µs per row; single runs reached 9–10 ms (over the 8.3 ms 120 Hz budget) |
| Scroll frame CPU, `VirtualList` 1k / 100k | 0.035 / 0.033 ms p50 | no equivalent | flat in row count; tessellation adds ~7 µs |
| Dependencies (hello) | 229 lines / 172 unique | 267 / 202 unique | ashlar −15 % |

**Reading:** ashlar wins on idle CPU and on virtualized lists (a design rule the reference framework
does not enforce: its worst real-app cost was a list that built every row). It loses on
non-virtualized content per frame (immediate mode lays out every widget every frame), binary size,
idle memory and launch time. Its own frame metric also undercounts (see 2.B).

## 2. Rendering and frame cost

Ashlar is ahead on idle design: wake reasons with per-reason counts (`crates/ui/src/wake.rs`), tests
that an idle app draws zero frames (`examples/notes/tests/wake.rs:40-96`), idle memory trim
(`crates/ui/src/idle.rs`), lazy virtual lists (`crates/ui/src/virtual_list.rs:352-411`), and caches
keyed by revision (`crates/ui/src/cache.rs:168-266`).

Findings:

- **A. A focused text field repaints at 2 Hz forever** (**verified**). egui's `paint_text_cursor`
  calls `request_repaint_after_secs` on every frame a `TextEdit` has focus while
  `visuals.text_cursor.blink` is on (egui 0.36 `text_selection/visuals.rs:297-313`), and ashlar
  never turns it off (`crates/ui/src/tokens.rs:321`). Each blink is a full UI pass, tessellation and
  present, credited to `input` in wake stats. The idle tests never focus a field.
- **B. The frame metric misses most of the frame.** `FrameStats::record` times only `Runtime::ui`
  (`crates/app/src/runtime.rs:490, 532-536`); the headless `Driver` never tessellates and runs at
  scale 1 (`crates/app/src/driver.rs:133-175`); `first_frame_ms` stops before the first present.
  Telemetry's slow-frame bound is 16.7 ms (`crates/trace-proto/src/lib.rs:42`), the 60 Hz budget,
  not 8.3 ms.
- **C. No CPU-vs-wall split, no allocation counting, no phase split.** Blocking I/O on the UI thread
  shows up only as a slow frame.
- **D. Per-frame runtime waste** (**verified**): `update_title` formats a string every frame
  (`runtime.rs:816-823`); `persist_view` clones and deep-compares the whole view-state map every
  frame (`runtime.rs:768-773`); `wake(DOCUMENT)` after any revision change doubles frames per
  edit, even for keyboard commands dispatched before the view draws (`runtime.rs:528-531`).
- **E. `markdown::show` rebuilds everything every frame** (`crates/ui/src/markdown.rs:465-538`):
  re-parses, rebuilds every `LayoutJob`, copies every span, lays out off-screen blocks. A long chat
  transcript costs O(document) per frame. The reference framework measured −80 % time and −99.8 %
  allocations from caching the same thing.
- **F. The notes example (the perf showcase) does O(n) work** (**verified**): `Notes::get` is a
  linear `find` called every frame (`examples/notes/src/lib.rs:38-40`, `:273`); the perf test
  selects id 50, which hides it; each keystroke with a query re-filters all notes with two
  `to_lowercase()` allocations per note (`:159-161`).
- **G. Galley cache hot path** (`crates/ui/src/cache.rs`): takes the context write lock per label
  (`:270-290`), hashes with SipHash (`:29-33`), and clears the whole cache when full (`:112-118`), a
  re-layout storm.
- **H. Variable-height `VirtualList` offsets** recompute the full prefix sum on any revision change
  (`virtual_list.rs:61-78`): O(rows) per edit.
- **I. Few perf tests**: six in total, none in `ashlar-ui`; nothing for typing, resize, markdown,
  tree, docking, palette, allocations, or idle with a focused field.

Techniques worth adopting (they do not need an own renderer): debug-only perf warnings (stall when
wall ≫ thread CPU, the same frame drawn repeatedly, a frame budget line), an allocation gate in perf
tests, full-frame measurement at scale 2 against the 120 Hz budget, an automatic markdown cache,
a per-frame JSON log, and incident rules in telemetry (long frames, stalls, memory growth).

## 3. Motion, tokens and developer API

- **No motion system.** The only time-based drawing is the indeterminate progress sweep
  (`crates/ui/src/status_bar.rs:110-120`, fixed 30 Hz, ignores reduced motion). Toasts, tabs,
  docking previews, dialogs, hover fills and theme switches change in one frame.
  `wake::Reason::ANIMATION` exists (`crates/ui/src/wake.rs:64`) with one caller.
- **Reduced motion is read but unused**: `ashlar-native` reads it on every OS
  (`crates/native/src/system.rs:26,111,169,260,323`); nothing consumes it; egui's
  `animation_time` is never tied to it.
- **The OS accent and increased contrast are ignored**: `runtime.rs:380` always installs the built-in
  light and dark tokens.
- **Tokens use sRGB byte lerps** for derived states (`tokens.rs:257-264, 344-348`), which shifts hue;
  there are no motion, elevation or stroke tokens; shadows are hard-coded (`tokens.rs:310-312`);
  themes cannot be loaded from data.
- **egui's own animation helpers** restart without velocity (visible kink on reversal), share one
  global duration and have no reduced-motion policy.
- **What transfers from the reference framework** is the pure math and policy, not its retained tree:
  closed-form springs set by (duration, bounce), retargeting that keeps position and velocity,
  settle time known up front (so frames stop exactly), colour interpolation in OKLab, per-class
  reduced-motion rules, "nothing animates on first show", "the user's own hand is never animated",
  exits shorter than enters, and a motion gate in tests on a virtual clock.
- **API gaps**: no `cx.tokens()`, no motion access on `Cx`, commands carry no origin (user, undo,
  agent) for a motion policy, `cx.view::<T>(key)` decodes with serde on every read
  (`crates/app/src/runtime.rs:66-138`).

## 4. Agent tooling and testing

Ashlar is ahead here: a hardened control port, an AccessKit element tree with stable refs, a `.ui`
script language with assertions, MCP `ui_*` tools, annotated screenshots, and an installer for coding
agents. The reference framework has no accessibility tree and no inspector socket.

Gaps and bugs:

1. **`trace` does nothing with `--port`/`--app`** (bug): `record()` returns early
   (`crates/debug/src/verbs.rs:792-795`), so the CI flow the docs recommend records nothing.
2. **`Driver` screenshots are flat grey** (`crates/app/src/driver.rs:160-164, 198-203`): no pixels
   without a window. The reference framework renders every app headless to PNG from its own flags.
3. **`.ui` `sleep` is wall-clock** (`crates/debug/src/batch.rs:175-179`); scripts cannot run
   in-process on `Driver` (`verbs.rs:170-177` always uses TCP).
4. **No clock control over the port**: an agent cannot pause or step time to capture an animation.
5. **MCP lacks `app.stats` and `app.logs`** (`crates/mcp/src/tools.rs:136-280`); `app.stats` has no
   memory or CPU numbers (`runtime.rs:1076-1088`).
6. **Doc drift**: the MCP tools table in `docs/control-protocol.md:215-216` is stale against
   `tools.rs:143-150` and `docs/debugging-uis.md:113`.
7. **No app-level golden snapshots, no named scenes, no motion/jank gate, no record and replay, no
   hot reload.** Scripting (an embedded VM) is not recommended: commands plus MCP already cover it.

## 5. Components

**What ashlar has.** `ashlar-ui` has strong infrastructure components:
- a virtual list, table and tree at 100k rows
- docking
- a command palette and a keymap editor
- value fields with edit phases
- automation ids on everything

**What it lacks** is the everyday controls and content components that the reference framework's
kit ships:
- text buttons with variants, a switch, a segmented control, select and combobox
- tooltips, badges, a spinner and a progress circle
- empty states, cards, accordions, a sidebar and settings rows
- syntax highlighting, a code view and a diff view
- a chat transcript (`ashlar-assistant` ships a harness with no UI)

The reference kit has no slider, stepper, date picker, colour picker or command palette.

Quality gaps in existing components:

- **Toasts** (`crates/ui/src/toasts.rs`):
  - Not a live region; the body has no `widget_info` (`:257`).
  - No title, action, icon or dedupe key (`:28-33`).
  - Escape doesn't dismiss, and keyboard focus doesn't pause the timer (`:211`).
  - Overflow is invisible (`:200`).
  - Entry and exit are a one-frame snap.
- **Markdown** (`crates/ui/src/markdown.rs`):
  - Rebuilds every frame (2.E).
  - Links can't be focused, are registered as buttons, and only the first row of a wrapped link is
    hit-tested (`:568-600`).
  - No syntax colours; English strings are hard-coded (`:618-625`).
  - Tables silently drop rows past 500 (`:665`) and wrap at a fixed 320 pt (`:680`).
  - A selection can't span blocks.
- **Virtual list, table and tree**:
  - Rows have no AccessKit nodes (`virtual_list.rs:370-411`, `tree.rs:193-200`), so a screen
    reader sees one region.
  - No multi-selection model, no type-ahead, no reorder.
- **Tabs** (`crates/ui/src/tabs/mod.rs:152`):
  - No automation id.
  - Each tab is its own Tab stop: no arrow-key movement inside a tab list, and no container node.
  - The standalone strip exists only inside docking.
- **Dialog** (`crates/ui/src/dialog.rs:106-125`): no initial focus, focus not returned to the
  opener on close, and a private button painter (`:157`).
- **Icon buttons** (`crates/ui/src/buttons/mod.rs:39`): no tooltip, so they have no visible name on
  hover.
- **Cross-cutting**:
  - English strings are hard-coded across components, with no single localisation path (only
    `KeymapLabels`).
  - The gallery has no pages for markdown, tabs, icons, docking or status-bar states.

Ashlar is ahead on sliders and knobs, value fields, the command palette, the keymap editor and agent
control. Two other differences: the search field in the reference kit's settings examples doesn't
filter, and its toasts have no timeout.

Two measured results from the reference framework's notes are worth testing in egui:
- one text field per block in a long note took typing from 12 ms to 0.8 ms per keystroke at 50 KB
- re-wrapping cached text on resize, instead of reshaping it, took frames from 16 ms to 4.6 ms

IMP-1.5 (`perf_type_50kb_note`, `perf_live_resize`) measures both.

Ideas worth copying:
- derive a palette from three seeds (background, foreground, accent) in OKLab, with contrast floors
- repair streaming markdown so unclosed markers never flash
- hold table column widths while a stream is inside a row
- golden-test each component's accessibility tree

## 6. Platform and native features

Ashlar is ahead on the keychain, printing, update checks, Dock progress, settings that survive
corruption, and a menu bar on Windows by design. The reference framework also runs on winit 0.30 and
patches it with small per-OS modules, so most of its ideas fit eframe/winit as ADR-0001 modules or
existing crates (`tray-icon`, `global-hotkey`).

Bugs:

1. **Windows apps with `native-menu` have no menu** (**verified**). `NativeMenu::install`
   (`crates/app/src/native_menu.rs:36-45`) never calls `NativeMenuBar::attach_window`, and nothing
   else does (`crates/native/src/menu/native.rs:156`). `native_menu_active()` still returns true
   (`runtime.rs:569-578`), so the in-window bar is hidden too.
2. **Native menu shortcuts skip the typing guard** on macOS. NSMenu key equivalents fire before
   winit's keyDown, so ⌘Z in a text field runs document undo (`crates/app/src/builtins.rs:32`)
   instead of text undo (keymap path: `crates/ui/src/commands/keys.rs:179-200`).
3. **Native menu commands skip the modal guard** (**verified**): `Runtime::logic` drains native menu
   ids without checking `modal_open()` (`runtime.rs:430-433`; the keymap path checks it at `:501`).
   ⌘Q, ⌘S or ⌘O can fire under the "save changes?" dialog.
4. **macOS app menu lacks Quit and there is no Window menu** (`native_menu.rs:16-27`).
5. **wasm**: ranks ≥ 2 are never checked for wasm32 (`xtask/src/main.rs:175-178`); `ashlar-app`
   compiles but warns about dead code at `crates/native/src/dialogs.rs:100,114`.

Gaps:

6. **Native features never reach apps**: the runtime never calls `system::preferences`,
   `open_files`, `single_instance`, `notify`, `power` or `progress`. Opening from Finder, a second
   launch, dropped files, the OS accent, reduced motion and the Wayland dark-mode setting do nothing.
   On Linux, `preferences()` opens a blocking D-Bus connection per call (`system.rs:239`).
7. **File dialogs have no parent window and block the frame** (`runtime.rs:1131-1159`,
   `crates/native/src/dialogs.rs:147-204`).
8. **The Linux clipboard drops its handle right after copying** (`clipboard.rs:61-78`), so a copy can
   be lost without a clipboard manager.
9. **Notifications**:
   - macOS has no delegate, so no banner shows while the app is frontmost and clicks are lost.
   - Each notification gets a time-based id, so it cannot be replaced.
   - Windows returns `Unsupported` (`notify.rs:9,108-115`). An unpackaged path exists: register an
     app user model id under the current user, plus protocol activation.
10. **Dock ▸ Quit, logout and shutdown skip the dirty prompt and the flushes** (winit 0.30 has no
    `applicationShouldTerminate:`). Not yet confirmed on a bundled build.
11. **Startup**:
    - `first_frame_ms` starts after eframe built the window and GPU device (`runtime.rs:542-544`,
      `:1339-1343`).
    - A command-line document loads synchronously before the first paint (`runtime.rs:267-272`).
    - Window size and position are never restored (`app.rs:118,196`).
12. Smaller items:
    - `increased_contrast` is always `None` on Windows (`system.rs:170`).
    - Single instance doesn't bring the first window forward on Windows.
    - Agent rebuild loops steal focus on every relaunch on macOS.

Not a priority for document apps: overlays and panels, system materials (vibrancy), spell check, a
custom Windows title bar.
