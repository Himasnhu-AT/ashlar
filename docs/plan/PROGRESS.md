# Progress log

Newest first. Each entry has the date, the task ids it covers, what changed, numbers before and after,
and any decision or surprise.

## 2026-11-10: wt/a11ygold and wt/motion2 merged (CI green)

- **IMP-5.15:** `examples/gallery/tests/a11y_goldens.rs` covers all 9 pages.
  - Each page renders headless at a fixed size and scale 1, light theme, reduced motion. Its AccessKit
    outline (role, name, value, description, states, position in set) is stored in
    `tests/a11y/<page>.txt`.
  - Each page is rendered twice for determinism. A mismatch fails with a readable diff;
    `ASHLAR_BLESS=1` rewrites the files.
  - A check rejects any node whose name equals an automation id or looks like one (allow list: the tab
    titles `main.rs` and `lib.rs`).
  - Findings went to IMP-5.21.
- **IMP-5.12:** `RollingText`: only changed characters roll (up when rising, down when falling) and
  the width animates. Reduced motion crossfades instead. The accessible value updates at once.
- **IMP-3.7:** fling on release for value fields, sliders and knobs (to a projected, snapped and
  clamped value; one undo step), and opt-in `VirtualList::fling(true)` with rubber banding.
  - A pause of 40 ms or more, or reduced motion, means no fling.
  - Fling speed is capped so a fling never overshoots an edge.
  - **Changed:** fling is on by default for fields, sliders and knobs (`.fling(false)` turns it off).
- **After merging both:** the Controls golden gained only the new sections (0 removed lines). It was
  re-recorded in its own commit.
- Started: wt-a11yfix (IMP-5.21, 5.22).

## 2026-11-10: wt/i18n merged (CI green)

- **IMP-6.13:** `ashlar-i18n` (rank 0, wasm32; depends on serde_json, log, thiserror).
  - Flat JSON catalogs, stackable, parsed on first use. A bad catalog is reported and treated as
    empty.
  - Plural rules for about 50 languages, number separators per locale, and the fallback order locale
    → language → source; `zh-TW` resolves to `zh-Hant`.
  - Key checks run in tests: `I18n::check` and `missing_source_keys`.
- **`ashlar_ui::labels::UiLabels`** covers toasts, form, chat, diff view, keymap editor, tab strip and
  print panel. Explicit labels still win. English and German catalogs live in `crates/ui/i18n/`.
  - System locale comes from `LANGUAGE`, `LC_ALL`, `LC_MESSAGES` and `LANG`. There is no native list
    yet (IMP-6.19).
  - Changed: `ToastLabels`, `FormLabels` and the chat labels now default to the installed labels.
  - Changed: `KeymapEditor.labels` uses the installed labels when it equals the English default.
- Started: wt-a11ygold (IMP-5.15).

## 2026-11-10: wt/window merged (CI green)

- **IMP-6.5:** a file dialog opens on the frame after the one that asks for it, parented to the
  window (`FileDialog::parent`), and acts on the pick then.
  - `after_save` carries "Save" in the confirm dialog on to the held-back open or quit.
  - Fixed: a failed save left `quit_after_save` set.
  - Four Driver tests.
- **IMP-6.4:** `window_state.rs` provides a pure `clamp` (10 unit tests) and a `Tracker` that saves
  under `"window"` only when the viewport changes. New: `App::viewport`, `App::icon`,
  `App::remember_window`.
  - Gap: no monitor list exists before the window does, so the launch-time clamp only bounds sizes;
    on Windows an off-screen window can stay off-screen (IMP-6.18).
- Process note: removing a merged worktree while its agent's gate is still running kills that gate,
  and one gate fell back to the main checkout and ran against a merge in progress. Merges now wait
  until the agent has reported its last `ci:` line.
- Started: wt-motion2 (IMP-5.12, 3.7), wt-native2 (IMP-6.7, 6.12), wt-flakes (IMP-0.8, 1.10).

## 2026-11-10: wt/notify and wt/perfwarn merged (CI green)

- **IMP-6.6:** `Notification { id, silent, info }`, `notify::show`, `remove`, `on_click`,
  `set_app_identity`. Showing a notification with an existing id replaces it.
  - macOS: a delegate shows banners while the app is in front and reports clicks, under
    `catch_unwind`.
  - Windows: toasts, with the app id registered for unpackaged apps, escaped XML and hashed tags.
    Compile-checked for x86_64-pc-windows-msvc only.
  - Linux: the `desktop-entry` hint and `ActionInvoked` clicks.
  - New FFI modules are on the ADR 0001 list and in the deny-panics allow list.
  - **Changed:** macOS notifications play the default sound unless `silent` is set.
- **IMP-2.7:** `perf_warn` reports stalls (wall time far above thread CPU) and the same frame drawn
  60 times on non-input wakes, in debug builds, once per site. `ASHLAR_FRAME_BUDGET_MS` works in any
  build, at most one line per second. `ASHLAR_PERF_WARN=0` silences it.
- **IMP-2.12:** `ASHLAR_FRAME_LOG` (or `Runtime::log_frames`) writes one JSON line per pass from a
  writer thread, capped at 64 MiB.
- **IMP-4.2:** `app.stats.process` gives `{footprint_bytes, peak_bytes, cpu_s, threads}`.
  - `peak_bytes` is the highest footprint sampled, not the OS lifetime peak.
  - `threads` is reported on Linux only (IMP-6.18).
- The perf_warn Driver scroll test passes on one clean run out of three because of machine load
  (IMP-0.9).
- The syntax timing test also failed once under load (IMP-0.8). wt-flakes is fixing IMP-0.8 and
  IMP-1.10.

## 2026-11-10: wt/chat merged

- **IMP-5.7:** `chat::{Transcript, Message, ToolCard, Composer, PopupList}`. It is UI over plain data
  (the `TranscriptItem` trait or `ChatMessage`); its strings come from `ChatLabels`.
  - The transcript has its own height index (only visible rows are measured), stays at the bottom
    unless the user scrolled up (then `<id>.jump` appears), and is a polite AccessKit log.
  - Typing: Enter is ignored while composing and for one frame after an input-method event.
  - Performance: `transcript_10k_messages_frame` 0.129 ms (1k: 0.092 ms).
- **CI after the merge failed** on three direct `request_repaint` calls (the wake scan). The agent
  hadn't run the full gate. Fixed on `imp`: a new `chat::CHAT_LAYOUT` wake reason and `INPUT` for
  clicks. Rule from here on: no merge until the agent reports `ci: ok` from a full
  `cargo xtask ci`.
- Started: wt-i18n (IMP-6.13).

## 2026-11-10: wt/rowuse and wt/diff merged (CI green)

- **IMP-0.5:** `deny.toml` has a `[licenses]` allow list: MIT, Apache-2.0 (and the LLVM exception),
  BSD-2/3, BSL-1.0, ISC, MPL-2.0, Unicode-3.0, Zlib, CDLA-Permissive-2.0 (webpki-roots), and OFL-1.1
  plus Ubuntu-font-1.0 (egui's fonts). No GPL or LGPL. `cargo deny check licenses` passes.
- **IMP-2.17:** the devtools Elements list was ashlar's only list of selectable labels. It now uses
  `list::Row` (which gained `indent`, `text_style` and `weak`): 1k rows p50 0.67 → 0.10 ms.
  - The palette, keymap editor and notes list already draw through `VirtualList`, where `Row` would add
    a second click target.
  - `list::checked` wraps the tab strip's overflow menu.
- **IMP-5.6:** `ashlar-diff` (rank 0, no dependencies, wasm32): Myers with a cost cap, word emphasis,
  and `parse_patch`/`write_patch`, which round-trip real `git diff` fixtures and never panic on
  mutated input. `DiffView` adds unified and split modes, a sticky header and hunk navigation.
  - `myers_10k_lines` 2.08 ms, `diff_50k_lines_frame` 0.41 ms.
  - **CI after the merge:** `crates/app/tests/wake_scan.rs` (all repaints go through `wake`) caught two
    direct `request_repaint` calls in `diff_view.rs`. They were fixed on `imp`. Agents now run the
    full `cargo xtask ci` before reporting, because repo-wide checks live in other crates.
- New: IMP-1.10 (the allocation counter is per process; it fails when tests run in parallel).
- Started: wt-window (IMP-6.4, 6.5), wt-notify (IMP-6.6).

## 2026-11-10: wt/launch merged (CI green)

- **IMP-6.9 / 1.8:** `app.stats.launch.{to_window_ms, to_first_frame_ms, launch_to_present_ms}`,
  measured from the first line of `run_native`. `<PREFIX>_READY_FILE` gets `{pid, ready_ms}` after
  the first present. `cargo xtask launch [--runs N] [--memory]` is a manual probe that opens windows.
  - Baseline over 9 launches (heavily loaded machine): p50 144–186 ms, p95 188–516 ms.
  - Breakdown of one launch:

    | Phase | Time |
    |---|---|
    | before eframe | ~8 ms |
    | window and wgpu setup | 80–150 ms |
    | `Runtime::new` | ~10 ms |
    | event loop to first frame | ~37 ms |
    | first `ui` | ~10 ms |
    | first paint and present | ~35 ms |

- **IMP-6.10:** a command-line document opens on the frame after the first. A `--scene` waits for that
  open; no test covers this.
- **IMP-2.11:** dropped. eframe 0.36 already creates the window hidden and shows it after the first
  paint.
- **IMP-2.10:** hello's idle memory falls in one of two groups between runs (cause unknown):
  - about 40 MB footprint / 94 MB peak
  - about 75 MB footprint / 203 MB peak

  Contributors: about 54 MB of GPU memory during startup (mostly freed afterwards), swapchain 3 × 9.5
  MB, heap 21 MB, an 8 MB system image buffer, and the font atlas at about 1 MB.
  - **Default changed:** windows queue at most one frame ahead (`App::frame_latency`, default 1; it
    was 2). In the high group, idle went 75.5 → 66 MB and peak 203 → 193 MB; the low group didn't
    change. This also cuts input latency by a frame. Apps with heavy GPU frames can set 2. Flagged to
    the owner.
- Started: wt-perfwarn (IMP-2.7, 2.12, the rest of 4.2).

## 2026-11-10: wt/motiongate and wt/overlays merged (CI green)

- **IMP-4.10:** `ashlar_ui::motion_gate::MotionRecorder` and `Driver::record`.
  - Checks for: overshoot on opacity and colour, reversal with bounce 0, pops, flicker, and frames
    requested after everything settled. There is an allow list per id, prefix or wake reason.
  - Seeded faults give exactly one finding each.
  - The gallery passes at 120 Hz: dialog, toast, animated theme switch, tab and page switches. The
    only finding was the gate's own false alarm on values set with `motion::set`, now ignored.
  - The idle check allows 3 frames after settle, because egui adds a repaint after a request.
- **IMP-5.9:** `overlay::{Select, Combobox, Popover}`, `ashlar_ui::help` tooltips, and
  `commands::menu::context`.
  - The combobox filters with fuzzy matching; composition never commits a choice.
  - The popover flips at window edges and returns focus when it closes.
  - Option lists are virtualized.
  - New: `MotionTokens.tooltip_delay` and new automation roles.
- **Merge incident:** the overlays merge left conflict markers in `crates/ui/src/lib.rs` (two new
  `mod` lines), and the format gate caught them. The merge script stopped silently, so merges now go
  through one script that stops on any non-CHANGELOG conflict and checks for leftover markers.
- Started: wt-rowuse (IMP-2.17, IMP-0.5), wt-chat (IMP-5.7).

## 2026-11-10: wt/allocreg, wt/tabs, wt/form, wt/snap merged (CI and allocation gate green)

- **IMP-2.18:** the allocation regression was real, and caused by devtools.
  - Devtools turned AccessKit on to answer a request and never turned it off, so every later frame
    built the full screen-reader tree. IMP-5.3's richer row nodes made that tree about 100
    allocations bigger.
  - Fix: devtools turns AccessKit back off only where no screen reader can exist (`Driver`,
    `--headless`, or a build without `accesskit`), through `devtools::set_platform_accesskit`. In a
    real window it stays on once on: eframe enables it once per activation and egui offers no
    getter, so a screen reader can't be told apart.
  - Test: "a screen reader activates during a request → stays on".
  - On merged `imp`: `allocs_per_idle_wake_frame` 272 (gate 578; was 652), `keystroke_allocs` 990
    (was 2,246), scroll 345.
  - Frames with AccessKit on are no longer gated.
- **IMP-5.10:** `TabStrip`, a single implementation that docking also uses.
  - One Tab stop with arrow keys, close by button, middle-click or ⌘W, an overflow menu, and reorder
    by drag or ⌘⇧+arrow.
  - Accessibility: a TabList node and automation ids.
  - The `tabs::indicator` is animated relative to the strip.
  - Docking tabs don't have close or automation ids yet.
- **IMP-5.8:** `form::section`/`row` (`.bind(&mut v, default)` handles modified and reset), `form::filter`
  with highlights and an empty state, and `Field` with announced errors. Ids are `<section>.<row>`.
  Gallery Settings page. Merge note: Tabs and Settings both added gallery pages, so `Page::ALL` now
  has 8 entries.
- **IMP-4.8:** `crates/debug/src/snapshot.rs` (exact by default, tolerance optional, `-new` and
  `-diff` PNGs, `ASHLAR_UPDATE_SNAPSHOTS=1`, a skip on another adapter). The egui_kittest snapshot
  feature was not reused: it would pull the UI toolkit into rank 4, and its compare is perceptual.
  Notes baselines were recorded on Apple M5 Pro / Metal.
- **IMP-4.9:** `App::scene`, `--scene`, `--list-scenes`, `ui.scene`, the `ui_scene` MCP tool; the notes
  app has `empty` and `two-notes`.
- Started: wt-diff (IMP-5.6), wt-overlays (IMP-5.9), wt-motiongate (IMP-4.10), wt-md2 (IMP-5.4,
  5.18). wt-launch is running.

## 2026-11-10: wt/tween, wt/openfiles, wt/size merged

- **IMP-3.9:** a real bug. `tween()` (`crates/motion/src/curve.rs`) applied its velocity-continuity
  term from rest, which forced every tween to start at zero velocity. A 2 s linear 0→100 tween read
  11.97 at 0.525 s instead of 26.25. A tween from rest now follows its easing exactly. Three new tests
  were added, and the retarget property tests still pass.
- **IMP-6.2:** single instance, opening files from Finder, and dropped files.
  - `App::single_instance(true)`: a second launch forwards its files and exits 0. The running app
    opens the first file and focuses its window.
  - macOS `listen_os` runs before the event loop.
  - `App::accept_drops`: a file with a known extension goes to `file.open`; other files reach the view
    through `Cx::dropped_files()`.
  - A dirty document asks first.
  - Only the first file of a batch is opened, and URLs are only logged.
  - The real socket and Finder paths were not run in a live app.
- **IMP-1.7 / 2.9:** `examples/hello` and `bench: hello_stripped_bytes`.

  | Hello (stripped, macOS arm64) | Default features | No default features |
  |---|---|---|
  | release | 13.55 MB | 12.58 MB |
  | `release-small` (fat LTO, 1 codegen unit, opt-level s, strip; still unwinds) | 8.96 MB (−34 %) | 8.38 MB (−38 %) |

  - Where the size goes: epaint 1.8 MB, wgpu-core 1.6, naga 1.4, font shaping 1.0, egui 0.8,
    ashlar-ui 0.6, ashlar-debug 0.45 (through `--headless`), the image stack about 0.45 (egui-winit's
    clipboard), and accesskit about 0.25.
  - `rfd`, `muda`, MCP and the CLI are already dropped by the linker when unused.
  - New: the `accesskit` and `headless` features, both on by default.
  - The reference framework's 5 MiB target is out of reach while ashlar uses wgpu and egui's font
    stack.
- Started: wt-tabs (IMP-5.10), wt-snap (IMP-4.8, 4.9).

## 2026-11-10: wt/clock, wt/animate, wt/syntax merged

- **IMP-4.7:** `app.clock {pause|step_ms|resume}` (`crates/app/src/clock.rs`,
  `crates/control/src/clock.rs`).
  - Also available as `ashlar-debug clock` and the MCP tool `ui_clock`.
  - While paused, input still runs frames at the frozen time. A step advances exactly one frame, and
    resume continues without a jump.
  - `ashlar_ui::motion` freezes too. Settings and autosave run on host time.
  - While paused: caret blink, toast timeouts and tooltip delays stand still, and two clicks count as a
    double click.
  - It reported a possible non-linear linear tween (IMP-3.9).
- **IMP-3.6 / 3.5:** `motion::presence`, animated toasts (in, out, glide into the gap), dialog
  (fade, grow from 97 %), docking drop preview, and state fills in the controls.
  - `tabs::indicator` exists but nothing calls it yet (IMP-5.20).
  - The theme switch crossfades in OKLab. A plain blend dropped text contrast to 1.16 mid-switch, so
    the text switches to whichever theme's text reads better, keeping it at 3:1 or more.
  - Visual regression tests run with reduced motion on; their expectations are unchanged.
- **IMP-5.5:** `ashlar-syntax` (rank 0, no dependencies, wasm32) highlights 18 languages and has an
  incremental `highlight_line`, property tests and golden tests.
  - `highlight_1k_lines`: Rust 477 µs, TypeScript 757 µs, Python 280 µs.
  - `CodeView`: virtualized, gutter, wrap, selection and copy, marks, lexer state cached per
    revision. `code_view_100k_lines_frame` is 0.058 ms (first frame about 4.5 ms).
  - Markdown code fences are coloured.
- **The CI run on the merge failed to compile:** CodeView called the list's row renderer with the
  `Kind` and immutable list it had before IMP-5.3. Each branch built alone. The fix is c6b3bc4, which
  also stops CodeView naming its Document node with its automation id. From now on, agents merge
  `imp` into their branch and retest before reporting.
- Started: wt-allocreg (IMP-2.18), wt-tween (IMP-3.9), wt-openfiles (IMP-6.2).

## 2026-11-10: wt/a11y and wt/allocfix merged

- **IMP-5.3:**
  - Every visible row of `VirtualList`, `VirtualTable` and `TreeView` gets an AccessKit node
    (ListItem, Row or TreeItem). Each has a name from `row_label`, selected state, and position in
    set; tree items also report level and expanded.
  - The nodes are built only while AccessKit is on. A 100k-row list makes 10–60 nodes.
  - New: `Selection` (anchor, cursor, sorted ranges; ⌘A is one range at 10M rows) and `type_ahead`.
  - Perf: within noise.
- **CI caught a regression from IMP-5.3:** the list node took its automation id (`notes.list`) as its
  accessible name, so a screen reader would read an internal id. The fix (on `imp`) leaves the list
  unnamed unless the app labels it, and the a11y test now asserts that.
- **IMP-2.16:**

  | Metric | Before | After |
  |---|---|---|
  | `allocs_per_idle_wake_frame` | 958 | 550 (gate 1,000 → 578) |
  | `allocs_per_frame_scroll_100k` | 472 | 345 |
  | `keystroke_allocs` | 3,720 | 1,935 |

  - List rows now draw cached, non-selectable galleys (`cache::row_label`, `cache::label`,
    `cache::button_text`), tested against egui's own layout. That took them from 623 to 138
    allocations per idle frame.
  - `refresh_infos` rebuilds command infos and menus only when a command's enabled state changes.
    Before, about 540 allocations per keystroke went there.
  - Bug fixed: clicking a note's title text now selects the note. The selectable label used to take
    the click.
  - **After merging with IMP-5.3** the perf gate fails: idle frame 652 (gate 578), keystroke 2,246.
    Each branch passed alone; IMP-2.18 finds the cause, and the gate stays where it is.
  - Left as is: inside egui, tessellation (about 107), per-row bookkeeping (about 7 per row) and each
    button (about 17).

## 2026-11-10: wt/race merged (CI green), a real bug

- **IMP-0.6:** the flaky telemetry test found a real bug in `ashlar-trace`, not in the courier
  hand-off.
  - Cause: `end(1ms)` timed out, so `shutdown_with` called `Spool::release()` and unlocked the run's
    own unsent folder. The worker's last pass then re-locked it through `adopt_orphans` and held it
    until the process exited. Couriers saw it as live and skipped it, so that session was never
    delivered while the process stayed alive.
  - Fix: a released spool adopts nothing (`crates/trace/src/pipeline/spool.rs`).
  - Test: `a_released_spool_adopts_nothing_and_leaves_its_folders_to_others`, which fails without the
    fix. The telemetry test is unchanged.
  - Failure rate under load: 4/60 before, 0/100 after. With a forced delay: 10/10 before, 0/10 after.
- Started: wt-a11y (IMP-5.3), wt-clock (IMP-4.7).

## 2026-11-10: wt/theme and wt/uiscript merged

- **IMP-3.4:** `with_accent` works in OKLCH (same hue, lightness steps, capped chroma).
  - `on_accent` turns dark below 4.5:1, and the focus ring is pushed to 3:1 against every surface.
  - New: `motion`, `elevation` and `stroke` tokens, `high_contrast()`, and `TOKEN_SPEC` (the OKLCH
    source of every built-in hex, tested within 1/255).
  - New tests: a WCAG gate over both themes and 64 random accents, and hue stability within 2°.
  - **Accessibility fix:** the old dark accent `#3b82f6` gave 3.68:1 with white text, failing the
    gate. It is now `#276ee0`, and a few derived state colours moved with it. The visual
    expectations were updated.
- **IMP-3.3 / 6.3:** OS preferences (reduced motion, accent, increased contrast).
  - Read at startup, before the app's setup, so an app's own tokens win. Read again when the window
    gains focus. On Linux the D-Bus read runs on a background thread.
  - New: `App::follow_system_accent`, `App::motion`, and the test seam `Runtime::set_preferences`.
  - The status-bar sweep holds still under reduced motion.
  - `ashlar-app` now turns on ashlar-native's `system` feature (zbus on Linux): a size item for
    IMP-2.9.
- **CI:** the first run on the theme merge failed in `ashlar-telemetry`
  `a_burst_of_runs_loses_nothing_to_a_courier_that_is_leaving` ("1 of 14 sessions not delivered").
  No merge touched that crate, it passed 3 of 3 runs alone, and the full rerun was green. It is a
  race test, so this may be a real rare race; IMP-0.6 investigates it rather than loosening the test.
- **IMP-4.5:** `ashlar_debug::Transport` and `run_script`; `Driver` implements `Transport`; and
  `Driver::run_ui(script)`.
  - `sleep` and `wait` advance virtual time.
  - The new test `ui_scripts_run_in_process_on_virtual_time` uses no thread and no sleep, and two runs
    give identical step JSON.
- **IMP-4.6:** every `App::run` binary takes `--headless --size --scale --theme --script --at --snap
  --tree`.
  - It runs in a fresh temporary folder, returns exit codes 0/1/2, and writes real wgpu pixels.
  - The end-to-end test is `examples/notes/tests/headless.rs`.
- Started: wt-animate (IMP-3.6), wt-syntax (IMP-5.5), wt-race (IMP-0.6).

## 2026-11-10: wt/alloc and wt/rows merged

- **IMP-1.2:** a `stats_alloc` 0.1.10 counting allocator (MIT, no dependencies, no `unsafe` in ashlar
  code), only in the two `tests/perf.rs` binaries. The helper is `crates/ui/tests/support/allocs.rs`.

  | Metric (release) | Value |
  |---|---|
  | `allocs_per_frame_scroll_100k` | 472 (215 KB) |
  | `allocs_per_idle_wake_frame` | 954; **gated at ≤ 1,000** |
  | `keystroke_allocs` | about 3,716 (2 MB) |
  | `md_static_allocs` | 108 (31 KB) |

  - A bare egui frame with a focused field makes 29 allocations. About 470 of the idle frame's
    allocations come from the visible list rows and about 480 from the chrome. Reducing them is
    IMP-2.16.
  - `cargo deny check licenses` fails before and after this change, because there is no allow list
    (IMP-0.5).
- **IMP-2.8:** `ashlar_ui::list::Row` uses one allocation, one interact, and a galley cached by
  (row id, hash of text, font and width). It is a list item for screen readers and supports Tab and
  Space/Enter; its automation id is opt-in. `list::checked` warns in debug builds when a list has
  more than 2,000 interactive widgets.

  | Metric (release, 5 runs, busy machine) | Before (`Button::selectable`) | After (`list::Row`) |
  |---|---|---|
  | `plain_scroll_1k` p50 | 0.73–0.77 ms | **0.094–0.103 ms** |
  | `plain_scroll_1k` p95 | 0.70–0.80 ms | 0.11–0.15 ms |
  | `plain_scroll_10k` p50 | 7.1–7.8 ms | 0.92–1.03 ms |

  - The cost breakdown for 1k rows (`perf_plain_scroll_breakdown`):
    - child `Ui` from `add_sized`: about 0.20 ms
    - the button's own layout, plus text layout of off-screen rows: about 0.30 ms
    - `interact`: about 0.035 ms
    - allocating space plus the scroll area: about 0.035 ms
  - The reference framework publishes 0.306 ms for this workload, so with `list::Row` ashlar is about
    3× faster. Adopting `Row` in ashlar's own lists is IMP-2.17.

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
