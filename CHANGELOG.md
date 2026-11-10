# Changelog

All notable changes to ashlar are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, a minor version
(0.x) may contain breaking changes.

## [Unreleased]

### Added
- `ashlar_native::mac_shell` (default feature `mac-shell`, IMP-6.14): `guard_appearance()` checks
  the window delegate's `effectiveAppearance` changes before winit casts them, so a nil appearance
  while the screen is captured no longer ends the app; `listen_reopen(waker)` reports Dock reopen
  clicks; `quiet_requested(prefix)` reads `<PREFIX>_QUIET`. An ashlar app installs both after its
  first frame, shows and focuses its window on reopen, and with `<PREFIX>_QUIET=1` opens without
  taking the focus.
- `ashlar_native::hotkey` (feature `hotkey`, IMP-6.16): system-wide shortcuts on global-hotkey
  (macOS, Windows, Linux on X11). `GlobalShortcuts::new(waker)`, `register(id, &chord)` with the
  keymap's chords (a chord without Ctrl, Alt or ⌘ is refused), `unregister`, `drain()`; dropping
  it releases every chord. `system::bring_to_front(window)` restores and focuses a window through
  the OS (Windows borrows the foreground thread's input when the foreground lock refuses), and a
  second launch on Windows now calls `AllowSetForegroundWindow` for the first (its pid travels in
  the endpoint file). `App::global_shortcut(command, chord)` (feature `hotkey` of `ashlar-app`)
  runs the command and brings the window forward; forwarded launches and tray clicks also bring
  it forward through the OS.
- `ashlar_native::tray` (feature `tray`, IMP-6.11): a tray / status-bar icon with a menu built from
  a `MenuSpec` (macOS status bar, Windows notification area, Linux StatusNotifierItem over D-Bus
  with no GTK). `Tray::new(TraySpec { id, tooltip, icon, template, menu }, waker)`, `events()`
  (`Activate`, `Menu(id)`), `set_tooltip` / `set_icon` / `set_menu`; dropping it removes the icon.
  The menu bar, trays and later popups now share one process-wide menu-event router, so a tray's
  items never reach the menu bar. `App::tray(spec)` (feature `tray` of `ashlar-app`) runs the
  chosen item's command and shows the window on a click.
- `ashlar_ui::markdown` v2 (IMP-5.4): links take keyboard focus (Tab, then Enter or Space opens
  them) with a focus ring, have the AccessKit `Link` role and automation role `Link` (new), and answer
  the pointer on every row they wrap onto; tables size columns to their content and wrap them down
  to their longest word, with a "N more rows" footer (`<id>.table.<n>.more`) past
  `MAX_TABLE_ROWS` (500) instead of dropping rows silently; the `Markdown` builder adds
  `.on_task(|index, checked| ..)` (clickable task checkboxes, `<id>.task.<n>`), `.find(query)`
  (highlighted matches, counted in `MarkdownResponse::matches`; `find_ranges` gives the byte
  ranges), `.streaming(true)` (an unclosed `**`, `*`, `~~`, `` ` `` or link at the end is drawn as
  already styled, so markers never flash; also `parse_streaming`) and `.labels(&MarkdownLabels)`
  for "Copy", "Copied", "code" and the table footer, by default from the installed `UiLabels`
  (`ui.markdown.copy`, `.copied`, `.code`, `.more_rows`; English and German). `MarkdownStyle`
  gains `find_bg`.
- `ashlar_ui::markdown::show_with_revision` and `Markdown::revision`: a caller that keeps a
  revision of the text skips hashing it each frame. Inline parsing is linear in the paragraph's
  length (a 50 KB paragraph: `bench: md_long_paragraph_parse`, about 0.4 ms), and a drawing
  snapshot (`tests/markdown_snapshot.rs`) checks what markdown paints (IMP-5.18).
- Rich clipboard: `clipboard::set_rich` / `rich` (text, HTML, PNG, file lists; read HTML is
  sanitized by `clipboard::sanitize_html`, every form size-capped), `clipboard::set_secret(text,
  clear_after)` (kept out of clipboard history where supported, cleared only if still ours) and a
  `Backend` trait with `MemoryClipboard` for tests.
- Dock ▸ Quit, logout, restart and shutdown go through the "Save changes?" check and flush
  settings, autosave and telemetry (`ashlar_native::session_end`, feature `session-end`, on by
  default): macOS answers `applicationShouldTerminate:` later until the user chooses; Windows
  handles `WM_QUERYENDSESSION` / `WM_ENDSESSION` and, since it can't wait for a dialog, keeps
  unsaved changes for recovery. The flow is the pure `session_end::step`.
- Gesture handoff: value fields, sliders and knobs keep going when a drag is let go while moving
  and ease to rest (clamped, snapped; the commit carries the resting value, so one drag is still
  one undo step); `.fling(false)` turns it off. `VirtualList::fling(true)` drags the list to
  scroll it, with fling and rubber band past either end. A pause of 40 ms before release, or
  reduced motion, means no fling.
- `ashlar_ui::rolling::RollingText`: counters and readouts that roll instead of jumping. Only the
  changed characters move (prefix/suffix, then LCS for short text), up when `.value(..)` rises and
  down when it falls; the width follows; `.tabular(true)` gives digits one width. Reduced motion
  crossfades. The accessible value is the whole new text at once; nothing animates once settled.
- Windows reopen where they were: `App::remember_window(bool)` (default on) saves position, size,
  maximized and full screen state in the settings under `"window"` when they change, and the next
  launch opens the window there, sizes bounded and a window left on no monitor moved back
  (`ashlar_app::window_state::clamp`). `App::viewport(|builder| …)` changes the window builder
  (minimum size, decorations…) and `App::icon(png_bytes)` sets the window icon.
- `ashlar-i18n` (new crate, rank 0, builds for wasm32): translations without a full Unicode
  library. Flat JSON catalogs per locale, parsed on first use and layered (later catalogs override
  key by key); a bad catalog is reported and acts as empty, never a panic. `I18n::{text, format,
  plural, integer, decimal, set_locale, negotiate}`: `{name}` templates, CLDR plural categories
  for about 50 languages (`key.one`, `key.few`, …), per-locale group and decimal separators
  (including Indian grouping), fallback locale → language → source (`zh-TW` → `zh-Hant`), POSIX
  and BCP 47 tags (`de_DE.UTF-8` → `de-DE`), `system_locales()` from `LANGUAGE`/`LC_ALL`/`LANG`,
  and `I18n::check()` for missing keys and mismatched placeholders.
- `ashlar_ui::labels`: one `UiLabels` installed on the context holds every built-in component
  string (toasts, form, chat, diff view, keymap editor, tab strip, print panel). Components use it
  unless the app passes its own labels, which still win. English and German catalogs ship for the
  `ui.*` keys; `UiLabels::from_i18n` builds the labels for any locale.
- Perf warnings in debug builds (`ashlar_app::perf_warn`): one stderr line per site when a frame
  waits (≥ 8 ms wall at ≥ 4× its CPU time; names the command or wake reason) or when 60 identical
  frames in a row are drawn for non-input wakes. `ASHLAR_FRAME_BUDGET_MS=N` (any build) prints at
  most one line a second for frames over `N` ms with the phase split; `ASHLAR_PERF_WARN=0` silences.
- `ASHLAR_FRAME_LOG=FILE` and `Runtime::log_frames(path)`: one JSON line per frame (wall and
  UI-thread CPU time, phases, wake reasons, commands run, pass number), written by a background
  thread and capped at 64 MiB with a final `"end"` line (`ashlar_app::frame_log`).
- `app.stats` (and the MCP `app_stats` tool) reports `process`: `footprint_bytes`, `peak_bytes`,
  `cpu_s` and `threads` (`ProcessStats`; `null` where the platform cannot say).
- `ashlar-native` notifications: stable ids (`Notification::id`; the same id replaces a shown
  notification), `notify::show` (returns the id), `notify::remove`, `notify::on_click` (a
  `Receiver<Click>` with the notification's `info` map), `silent`, and `notify::set_app_identity`.
  Windows now shows toasts (AppUserModelID registered under HKCU for unpackaged apps, escaped XML,
  tags over 64 characters hashed); macOS installs a notification-center delegate so banners show
  while the app is frontmost and clicks are reported; Linux sends the `desktop-entry` hint and
  reports clicks from `ActionInvoked`.
- `ashlar_ui::chat`: a chat kit over plain data. `Transcript` is a virtualized, variable-height
  message list (heights kept per message key, revision and width; a frame costs O(visible)) that
  follows the newest message while it streams, stays put once the user scrolls up and then offers
  "Jump to latest"; it is a polite AccessKit log. `Message` draws a role and a markdown body with a
  streaming state, `ToolCard` a tool call with folded details and keyboard-reachable Approve/Deny
  (`<id>.approve`, `<id>.deny`), and `Composer` a text field where Enter sends, Shift+Enter adds a
  line, input-method composition never sends, and `@` / `/` open a `PopupList` of suggestions.
  Strings come from `ChatLabels`. The gallery has a Chat page.
- `ashlar-diff` (new crate, rank 0, no dependencies, builds for wasm32): line diffs with Myers'
  algorithm under a cost cap (past the cap, a split at lines unique to both sides, then a coarse
  delete/insert, flagged `coarse`), word-level emphasis for paired changed lines, hunks with
  context and the unchanged ranges between them (`FileDiff::gaps`, expandable when the new text is
  known), and `parse_patch` / `write_patch` for unified and `git diff` patches (renames, copies,
  modes, binary markers, quoted paths, `\ No newline at end of file`); malformed patches are an
  error, never a panic. About 2 ms for a 10k-line file.
- `ashlar_ui::diff_view::DiffView`: unified and split diffs, virtualized (about 0.4 ms per
  scrolled frame at 50k lines), syntax-coloured per file language, word-level emphasis, "expand N
  lines" on collapsed ranges, foldable files, a sticky file header, line selection, `N`/`P` (or
  `F7`) between hunks. Automation `<id>.file.<f>.hunk.<h>` and friends for visible rows; AccessKit
  document, headings and labelled lines.
- `app.stats` reports `launch`: `to_window_ms`, `to_first_frame_ms` and `launch_to_present_ms`,
  counted from the first line of `run()` (`FrameStats::launch`, `LaunchTimes`). Native windows only.
- `<PREFIX>_READY_FILE=path`: the app writes `{"pid", "ready_ms"}` there atomically once its first
  frame is presented.
- `cargo xtask launch` measures `examples/hello` launch time over 9 launches (p50/p95), and
  `cargo xtask launch --memory` its idle RSS and physical footprint (macOS). Manual, opens windows.
- `App::frame_latency(frames)`: how many frames the GPU may queue ahead of the screen (1 to 3).
- Motion and jank gate for tests: `ashlar_ui::motion_gate::MotionRecorder` records values
  animated through `ashlar_ui::motion` and per-frame automation rects (`Driver::record` does both
  for an app), and `check(&MotionRules)` flags overshoot of opacity and colour, reversals without
  bounce, pops, flicker and frames requested after everything settled, with a per-id allow-list.
  The gallery's dialog, toast, theme crossfade and tab switches pass it.
- `ashlar_ui::overlay`: `Select` (a button with a list of options), `Combobox` (a text field whose
  options are filtered with the command palette's fuzzy matcher; input-method composition never
  chooses), and `Popover` (anchored, flips above at the window edge, Escape or an outside click
  closes, focus moves in and returns). Option lists are virtualized, fade with the `enter_exit`
  motion token and register `<id>.option.<n>` with ListBox/option roles.
- `ashlar_ui::help(response, text)`: a tooltip after the new `motion.tooltip_delay` token, also
  given to screen readers as the control's description.
- `ashlar_ui::commands::menu::context(response, &spec)`: a context menu built from a `MenuSpec`.
- Automation roles `ComboBox`, `ListBox`, `ListOption` and `Tooltip`.
- `ashlar_ui::form`: the settings form pattern. Sections of rows (label, description, control) with a
  modified marker and reset button, a filter that hides non-matching rows and empty sections,
  highlights matches and shows "No settings match", and `Field` with a required marker and an
  announced error. Row controls get automation ids `<section>.<row>`; strings come from
  `FormLabels`. The gallery has a Settings page.
- Golden snapshots: `Driver::assert_snapshot(name)` and the `.ui` line `assert screenshot <name>
  [target] [--tolerance channel=N,pixels=N]` compare with `tests/snapshots/<name>.png`, exact by
  default; a mismatch writes `-new.png` and a magenta `-diff.png`; `ASHLAR_UPDATE_SNAPSHOTS=1`
  records baselines with a `.meta.json` (adapter, backend, scale); a baseline from another adapter
  is skipped with a note. Screenshot replies from a `Driver` now carry `adapter`, `backend` and
  `scale` (IMP-4.8).
- Named scenes: `App::scene(name, |s| ..)`, `--scene NAME` (in a window or `--headless`),
  `--list-scenes`, `Driver::scene`, the `ui.scene` control method and the MCP tool `ui_scene`
  (listed only for apps with scenes) (IMP-4.9).
- `App::single_instance(true)`: a second launch hands its arguments to the running app and exits 0;
  the running app opens the first file with `file.open` (a dirty document asks to save first) and
  comes to the front. Desktop only.
- Files from Finder (macOS `odoc` / `GURL` Apple events: a double-click, a drop on the Dock icon)
  open with `file.open` while the app runs and at launch.
- `App::accept_drops(bool)` (on by default when the app sets a format or a file extension): the
  first dropped file with a known extension opens with `file.open`; the others reach the view as
  `Cx::dropped_files()`. `Driver::drop_files` and `Driver::forward_launch` test both paths.
- `app.clock` control method: `{pause: true}`, `{step_ms}` (one frame that much later) and
  `{resume: true}` hold the time an `ashlar-app` window's frames see, so an animation can be
  screenshotted or measured at an exact moment; input still runs while paused and resume does not
  jump. `ashlar-debug clock pause|step <ms>|resume` (also in `.ui` scripts) and the MCP tool
  `ui_clock` send it; `ashlar_control::{APP_CLOCK, ClockOp}` parse it.

- `examples/hello`, the smallest ashlar app, and a `cargo xtask bench` step that reports its
  stripped release size as `hello_stripped_bytes`.
- Features `accesskit` and `headless` on `ashlar-rs` and `ashlar-app`, both on by default (no
  change for existing apps). `default-features = false` drops AccessKit and `--headless` for a
  binary about 1 MB smaller.
- A `release-small` profile in the workspace (fat LTO, one codegen unit, `opt-level = "s"`,
  stripped): the hello app goes from 13.6 MB to 9.0 MB stripped on macOS arm64.
- `ashlar-ui`: screen readers see every visible row of `VirtualList`, `VirtualTable` and
  `TreeView`: list items, table rows and tree items named by `row_label` (the tree's labels),
  with selected state, position and set size, and level and expanded state for tree items. Only
  visible rows get nodes, and only while AccessKit is on.
- `ashlar-ui`: `selection::Selection`, an app-owned, serializable multi-selection (anchor, cursor,
  ranges). `VirtualList::selection(&mut sel)` and `VirtualTable::selection` handle Shift-click,
  Shift+arrows, ⌘/Ctrl-click and ⌘A; `selected` and `is_selected` keep working without it.
- `ashlar-ui`: `VirtualList::type_ahead(&text)` and `VirtualTable::type_ahead`: letters typed
  within a second of each other jump to the next row starting with them.
- `ashlar-app`: `--headless` for every `App::run` binary: `--size WxH --scale S --theme
  light|dark --script file.ui --at SECONDS --snap out.png --tree tree.json` writes a screenshot
  (real pixels when there is a GPU) and the UI tree with no window, in a temporary folder. Exit
  codes 0/1/2 as in `ashlar-debug`. `ashlar-debug` exports `save_png`.
- `ashlar-app`: `Driver::run_ui(script)` runs a `.ui` script (the `ashlar-debug batch` language)
  in process, with no port or thread. `sleep` and `wait N` let virtual time pass, so two runs give
  the same steps. `ashlar-debug` gains the `Transport` trait and `run_script` to run scripts
  against any target.
- `ashlar-ui`: `cache::label` (a `ui.label` with its galley cached under a key: no layout and
  no `String` on steady frames), `cache::row_label` (the same, not selectable, for text inside a
  clickable row) and `cache::button_text` (a button's text as a cached galley).
- `ashlar_ui::tokens::set_mode_animated`: switches theme with an OKLab crossfade, surfaces over
  0.4 s and text leading at half that; primary and secondary text keep 3:1 on the panel at every
  frame, and once settled the theme's own visuals are back and nothing more runs. `Tokens::mix`
  blends two token sets.
- `ashlar_ui::tabs::indicator`: where to paint a tab strip's selection underline or pill, sliding
  by its edges to the newly selected tab (paint only; tabs keep their rects for hit testing).
- `ashlar_ui::tabs::TabStrip`: a standalone tab strip with one Tab stop (arrows, Home and End
  move the selection), close by button, middle-click or ⌘W, an overflow menu for tabs that do not
  fit, drag or ⌘⇧ + arrow reordering (`tabs::reorder` applies a move), the animated selection
  indicator, an AccessKit tab list with positioned tabs, and automation ids `<id>.tab.<n>`.
  The gallery has a Tabs page.
- `ashlar_ui::tabs::Tab::automation`: registers a tab under a dotted automation id.
- `ashlar_ui::motion::presence`: keeps an item drawn while it leaves (`Some(progress)` until
  its exit settles, then `None`), entering and leaving along a token pair as an opacity.
- `ashlar_ui::code_view::CodeView`: a read-only code view with virtualized lines (O(visible) per
  frame), a line-number gutter, optional wrapping, mouse selection, select all and copy, search
  marks and syntax colours from `SyntaxTheme::from_tokens`. Line galleys are cached per line and
  revision; lexer states are computed lazily. Automation `<id>.line.<n>` for visible lines only;
  AccessKit document and paragraphs. About 0.06 ms per scrolled frame at 100k lines.
- `ashlar_ui::markdown`: fenced code blocks in a known language are syntax-coloured.
- `ashlar-syntax` (new crate, rank 0, no dependencies, builds for wasm32): syntax highlighting
  with hand-written, linear-time lexers for Rust, TypeScript/JavaScript, Go, C, C++, Java, C#,
  Python, shell, SQL, JSON, TOML, YAML, HTML (with script and style), CSS, Markdown (fenced code
  in its language) and diffs. `highlight` returns ordered byte ranges that cover the input;
  `highlight_line` carries a `State` from line to line for incremental views; `lang_for_path`;
  a `Highlighter` trait so a bigger engine can plug in. About 0.5 ms per 1,000 lines of Rust.
- `ashlar-app`: `Driver::with_renderer(runtime, Renderer::wgpu())` renders real screenshots
  with no window, on a wgpu device with no surface, at the driver's display scale. Without an
  adapter it falls back to the grey image. Screenshot replies from a `Driver` say which was
  used (`"renderer": "wgpu"` or `"none"`), and grey screenshots now have the screen's size in
  pixels instead of points.
- `ashlar_ui::list::Row`: a cheap selectable list row for plain (non-virtual) lists: one allocation,
  one interact, and a cached galley only while on screen. AccessKit list item with label and
  selection; focusable, Space and Enter click; opt-in automation id. A 1k-row plain list scrolls in
  about 0.10 ms per frame with it, against about 0.7 ms with `Button::selectable` rows.
- `ashlar_ui::list::checked`: wraps a list's content and, in debug builds, warns once per call site
  when it builds more than 2,000 interactive widgets in a frame, pointing to `VirtualList`
  (`ASHLAR_QUIET_LISTS` silences it).
- `ashlar-motion` (new crate, rank 0, no dependencies, builds for wasm32): closed-form springs set
  by duration and bounce (`Spring`, never early `settle_time`), `Curve` (spring, cubic-bezier
  tween, decay, instant), `Animated<T>` values whose retargets keep position and velocity and that
  land exactly on their target, `Class` settle thresholds and reduced-motion rules (opacity and
  colour never overshoot), `MotionPrefs`, motion `tokens` (exits shorter than enters), an
  OKLab/OKLCH `color` module with gamut mapping, premultiplied mixing and WCAG contrast, and
  `gesture` helpers (velocity tracker, fling projection, rubber band).
- `ashlar-app`: frame stats split the frame into phases: `app.stats` keeps the UI time as
  `p50_ms`/`p95_ms`/`max_ms` and adds `pass`, `tessellate` and `frame_cpu` (the whole frame on the
  CPU: eframe's own figure in a window). `Driver` tessellates every frame like a window and
  takes the display scale (`Driver::pixels_per_point`).
- `ashlar_ui::motion`: animated values for egui in one call per frame (`value`, `f32`, `opacity`,
  `color` in OKLab, `rect` by edges, `set`, `release`), kept in the context and forgotten when
  unused. Nothing animates on first show, frames are requested (`Reason::ANIMATION`) only while a
  value moves, and settled values are exactly their target. `prefs`/`set_prefs` set reduced
  motion (also turning off egui's widget animations) and a time scale; `active` counts moving
  values.
- `ashlar_ui::tokens`: `motion` (spring per kind of change), `elevation` (popup and window
  shadows, no longer hard-coded in `visuals()`) and `stroke` (hairline, focus) tokens;
  `Tokens::high_contrast()`; `contrast_ratio`; `TOKEN_SPEC`, the OKLCH source of every built-in
  colour, checked against the hex values by a test.
- `ashlar-app` follows the OS preferences: reduced motion (through `ashlar_ui::motion::set_prefs`),
  the accent colour (`Tokens::with_accent`) and increased contrast (`Tokens::high_contrast`), read
  at startup and again when the window gains focus (on Linux off the UI thread). Tokens an app
  installs itself win. `App::follow_system_accent(bool)` (default on) and `App::motion(MotionPrefs)`
  override; `Runtime::set_preferences` injects preferences in tests, and `App::runtime()` ignores
  the OS so test runs match on every machine. `tokens::of` and `tokens::apply_colors` are new.

### Changed
- File dialogs open over the app window (a sheet on macOS) and no longer block a frame: the
  frame that asks for one finishes, the next shows the dialog and acts on the pick. Saving an
  untitled document from the "Save changes?" dialog now goes on to the held-back open or quit
  after Save As. `ashlar_native::dialogs::FileDialog::parent(&window)` parents any picker.
- `ashlar-native` macOS notifications play the default sound unless `silent` is set, and ask for
  sound permission only then.
- A document named on the command line opens on the frame after the first, so the window shows
  before the load runs (it used to load before the first paint).
- Windows queue at most one frame ahead of the screen by default (`App::frame_latency(1)`): lower
  input latency and one swapchain image fewer (about 10 MB less idle footprint for a 960×640
  window on a Retina screen). Apps with heavy per-frame GPU work can ask for 2.

- `ashlar_ui::list::Row` gains `indent`, `text_style` and `weak`. The devtools Elements list draws
  its rows with it (about 6.7× cheaper per row than its old selectable labels: 0.10 ms against
  0.67 ms for 1k rows), and the tab strip's overflow menu runs inside `list::checked`.
- `ashlar_ui::docking`: group tab strips use the shared tab strip: one Tab stop per group with
  arrow keys, an AccessKit tab list, and the animated selection underline.
- `ashlar_ui::controls`: button, switch, segmented and radio hover and press fills ease over about
  0.1 s, like egui's own widgets (`tokens::state_color`, `Tokens::interact_fill_animated`); they
  change at once when egui's `animation_time` is 0, which reduced motion sets. Visual regression
  tests now run with reduced motion.
- `ashlar_ui::dialog`: on open the backdrop fades in and the panel fades in while growing from
  97 %; when the app stops showing a dialog, its last frame fades out a little faster without
  taking input. No growing under reduced motion; hit testing uses the resting layout.
- `ashlar_ui::docking`: the drop preview moves quickly (0.1 s) from one drop target to the next
  while a tab is dragged, and appears in place when a drag first shows it.
- `ashlar_ui::toasts`: a new toast fades and slides 8 pt in, a dismissed one slides back out and
  is painted until its exit settles, and the toasts behind it glide into the gap. Toasts queued
  before the stack is first shown appear at once; hit testing and automation rects use the resting
  layout; reduced motion keeps only the fade.
- `Tokens::with_accent` derives hover, focus, selection and selection text in OKLCH (same hue,
  lightness steps, capped chroma) instead of sRGB mixing, flips `text.on_accent` dark when white
  falls below 4.5:1 and pushes the focus ring to 3:1 on every surface. The dark theme's accent is
  darker (`#276ee0`, was `#3b82f6`) so white text on it reaches 4.5:1; its derived states follow.
- The status bar's indeterminate progress holds still (and asks for no frames) under reduced
  motion, and its sweep follows the motion time scale.
- Fewer allocations per frame (IMP-2.16): a frame woken only by a caret blink in the notes app
  makes 550 allocations instead of 958, a scroll frame 345 instead of 472, a keystroke 1,935
  instead of 3,720. The menu bar and the notes list draw cached galleys, and an edit that changes
  no command's enabled state no longer rebuilds every command's info and the menus.
- `ashlar-app`: a command run from the keyboard (or anything else before the view draws) no longer
  asks for a `document` frame: the frame that ran it already shows the change. The runtime also
  stops copying and comparing the view state and formatting the window title on frames that did
  not change them.
- `ashlar-ui`: variable-height `VirtualList` and `VirtualTable` keep row heights in a Fenwick tree,
  and `changed_rows(&[row])` says which rows changed height: those are measured again in
  O(log rows) each instead of re-summing every row (1M rows: about 2 ms → 1 µs). A full rebuild
  (new revision or row count) costs about a third more than before.
- `ashlar-ui`: `markdown::show` keeps its parse while the text is the same and parses streamed
  text again only from its last open block; each block's layout is kept per width and style, and
  blocks outside the clip rect only reserve their space (a 50 KB document: about 1.9 ms → 0.02 ms
  a frame). Automation ids (`<id>.link.<n>`, `<id>.code.<n>`) now exist only for blocks drawn,
  with the same numbering. `MdBlock`, `Span` and `Marker` implement `Hash`.
- `ashlar-ui`: the galley cache (`cache::galley`, `RevisionCache`, `MeshCache`) hashes with a fast
  fixed-seed hasher (`cache::hash_of` values differ from before), reads the shared cache under the
  context's read lock, and when full drops its least recently used half instead of everything, so
  labels in use are not laid out again.
- `ashlar-ui`: a controls kit (`controls`): `Button` (primary, secondary, ghost and destructive
  variants, three sizes, an icon, a loading state that shows a spinner and reports busy),
  `Switch`, `Segmented` and `RadioGroup` (one Tab stop; arrows, Home and End move the choice),
  `Badge` with tones, `Keycap` (a cap per key of a shortcut), `Progress::bar` and
  `Progress::circle` (determinate or indeterminate) and `Spinner`. Each registers a dotted
  automation id and fills its AccessKit node; indeterminate indicators ask for frames
  (`wake::Reason::ANIMATION`) only while on screen and hold a static pose with
  `reduced_motion(true)`. New automation roles: `switch`, `radio`, `radio_group`, `progress`,
  `label`, `group`, `disclosure`, `alert`. The gallery has a Controls page, and the visual
  regression suite compares the kit against reference painters (222 pairs).
- `ashlar-ui`: dialogs move the keyboard focus inside when they open (the body's first field,
  else the default button, else the cancel button; `Dialog::initial_focus` chooses) and give it
  back to the widget that had it when they close, however they close (`Dialog::return_focus_to`
  names it for a dialog opened by a click). Screen readers get a modal dialog node named by the
  title, with `Dialog::description` (also shown under the title) as its description.
- `ashlar-ui`: content components (`content`): `EmptyState` (icon, title, text, action),
  `Banner` (a tone, title, action and close button; a live region that screen readers announce,
  assertively for errors), `Card` (a titled group) and `Collapsible` (a header that shows or hides
  its content; reports expanded to AccessKit, arrows open and close it). `IconButton::tooltip`
  gives icon-only buttons a visible name on hover, and `controls::Button::part` registers a
  component's button as `<id>.<part>`. The gallery has a Content page; the visual regression
  suite covers the collapsible header and the banner.

### Changed

- `ashlar-ui`: dialog buttons are `controls::Button`s (primary for the default, destructive for
  a destructive one); a disabled one is drawn at the disabled opacity instead of in the sunken
  colours, and its automation entry no longer reports the default button as `selected`.

- `ashlar-ui`: `wake::wake`, `Waker::wake` and a zero-delay `wake_after`/`reschedule` ask egui for
  one frame instead of two (egui adds a second frame to every zero-delay repaint request). A command
  sent through the control port now costs 2.2 frames instead of 3.2.
- `ashlar-app`: the control port no longer asks for a frame after answering a request; the frame
  that serves it runs before the view draws and already shows the change. An engine command sent
  through the port now costs about 1.3 frames (was 2.2). `Driver::request` wakes for the
  `control` reason as the port does.
- `ashlar-update`: the check asks for this platform's build (`?platform=macos|windows-x64|linux-x86_64|linux-aarch64`,
  `PLATFORM`, `latest_url_for`), so a Windows or Linux app is never offered a Mac download.

### Fixed
- Linux: copied text and images no longer vanish when the copy call returns; one clipboard lives
  on a worker thread for the life of the process.

- `ashlar-ui` lists: `scroll_to` read the scroll offset under the wrong id, so it never scrolled
  up to a row above the view and moved the list for a row already in view. It now reads the scroll
  area's own id, as the fling does.
- `ashlar-ui` accessibility: lists, tables and trees take a `.label(text)` that screen readers
  announce; table cells are `Cell` nodes and the header row holds `ColumnHeader` nodes (with sort
  direction); resize handles, tree disclosure arrows and the fling drag surface no longer show up as
  unnamed nodes. The keymap editor's table, its actions column and its shortcut and clear buttons
  have names (new `KeymapLabels::table` and `actions`, English and German).
- `ashlar-ui` accessibility: search fields and the command palette input have a name, not only a
  placeholder (`SearchField::label`, new `UiLabels::search` with English and German); banners
  announce their text once instead of as both name and value.
- `ashlar-ui` devtools: AccessKit that the debugger turned on to answer a request (`ui.click`,
  `ui.snapshot`, a script) goes back off once nothing needs the tree, so a driven app no longer builds
  a screen reader's tree on every later frame (a caret-blink frame in the notes app: 272 allocations
  instead of 652). This happens only where no screen reader can exist (a `Driver` or `--headless`
  run, or a build without the `accesskit` feature); in a window it stays on, and
  `devtools::set_platform_accesskit` sets which applies.
- `Curve::Tween` started from rest follows its easing exactly: a linear tween is linear (it used to
  start at zero velocity, so a 2 s linear 0→100 tween read ≈12 at 0.525 s instead of 26.25).
  Retargets mid-motion still keep velocity continuous.
- Telemetry: a run whose shutdown wait ran out no longer takes back its own unsent session folder (and
  other leftover ones) as its worker finishes, so a courier delivers them instead of seeing them as in
  use until the process exits.
- `notes` example: a click on a note's title in the list selects the note (the title was
  selectable text that took the click).
- `native-menu`: picks in the OS menu bar no longer run while a dialog (such as "Save changes?") or
  the command palette is open.
- ⌘Z / Ctrl+Z in a text field undoes the text, not the document. Text-editing shortcuts (undo,
  redo, select all, copy, cut, paste) are left to a focused text field (`works_while_typing`);
  with `native-menu` they have no OS key equivalent, and menu picks of commands bound to them are
  skipped while typing.
- `native-menu` on Windows: the menu bar is attached to the window; until it is (or if that
  fails) the in-window bar shows, so the app never ends up without a menu.
- `native-menu` on macOS: Quit is in the application menu, and a Window menu has Minimize, Zoom
  and Bring All to Front.
- `ashlar-debug`: `trace start <dir>` inside `--port P batch -` (or `--app NAME`) recorded nothing,
  since the trace was kept only in a session file. The trace now lives in that batch run; outside
  a batch, `trace` with `--port`/`--app` is a usage error instead of doing nothing.
- `docs/control-protocol.md`: the MCP tools table listed `screenshot {id?}` and left out the
  debugger tools; it now lists every tool with its arguments, and a test keeps it in step with the
  code.
- `ashlar-app`: a focused text field no longer keeps an idle app drawing: egui's caret blink cost
  about 6 frames a second for as long as the field had focus. The runtime blinks the caret itself,
  one frame per phase change credited to the new `caret` wake reason, and leaves it solid after
  10 s without input (`ashlar_app::caret`); input starts it again. 30 s idle with a focused field:
  194 frames before, 21 after.
- `ashlar-ui`: a value field's typed entry lost what was typed in the frame its editor opened (a
  value typed straight after the click, as the control channel does), and while its window
  did not have the operating system's focus (an app driven without being activated) the editor
  selected all its text again every frame, so each entry replaced the last. The editor takes the
  keyboard before it is drawn, and keyboard handling here asks egui's focus alone
  (`edit::has_keyboard`): value fields, sliders, trees, swatches, resize handles, virtual lists and
  the search field. A person typing in a focused window was not affected, except that an editor
  left open while they switched windows had its text selected again when they came back.
- `ashlar-ui`: `ui.type` / `ui.fill` with a target no longer click a text field that has the
  keyboard already: two requests in a row made a double click, which selected a word for the
  next text to replace.
- `ashlar-assistant`: a saved chat reopened after a relaunch could be refused ("Invalid
  `signature` in `thinking` block … bound to a different conversation"): its thinking blocks were
  replayed even when the system prompt, the tools or the saved messages (results cut, pictures
  left out) differed from the request that produced them. Each Anthropic reply now records a
  fingerprint of its prefix (`Native::binding`, `anthropic::request_parts`, `anthropic::bind`), and
  thinking goes back only while it matches; chats saved before this replay none. A refusal anyway
  drops all earlier thinking (`Conversation::forget_thinking`) and the step is asked once more.
- `ashlar-assistant`: OpenAI-format requests carry at most `anthropic::MAX_IMAGES` pictures (the
  newest), like Anthropic ones; they carried every picture of the chat.
- `ashlar-ui`: `idle::FrameRate::fps` counts the frames of the last second (two frames 11 ms
  apart on an idle app read 91 fps); `idle::Activity::of` documents that an unfocused window is
  the background signal that arrives (winit rarely reports a covered one).
- `ashlar-native`: `memory` is listed as an FFI module (the deny-panics check refused it); it no
  longer calls macOS's `malloc_zone_pressure_relief`, which released next to nothing on macOS 26, so
  `release_free_memory` is a no-op there; `memory::space_efficient_malloc()` tells whether the
  process runs with `MallocSpaceEfficient=1`, which hands freed memory back at once.

### Added

- `ashlar-ui`: toasts v2. Each toast is an AccessKit live region (polite; assertive for errors), so
  screen readers announce it. New on `Toast`: `title`, `action(label, command)` (up to three
  buttons, reachable as `toast.<id>.action.<n>`; clicks arrive from `Toasts::take_actions` or
  `show_actions`), `key` (pushing a toast with a queued toast's key updates it in place), and
  `progress`; `Toasts::progress(key, text, fraction)` keeps a progress toast up to date and
  `dismiss_key` removes it. Escape dismisses the focused or newest toast (not while a field or a
  popup has it), keyboard focus pauses a toast's timer, close and action buttons show a focus
  ring, the close target is a full control high, toasts past `max_visible` show as "+N more", and
  the words come from `ToastLabels` (English by default). `Toast` gained public fields, so code
  that builds one with a struct literal must add them (`Toast::new` is unchanged).
- `ashlar-trace-proto`, `ashlar-trace`, `ashlar-trace-server`: telemetry counts frames over the
  120 Hz budget. `DISPLAY_BUDGET_US` (8.333 ms, already a bucket bound) and the optional
  `FrameStats::over_budget` are new within schema 1: older clients leave the field out, older
  servers ignore it, and `FrameStats::over_budget_frames` counts it from the buckets when it is
  missing. The SDK sends it and shows it in its local report; the server counts it from the
  stored buckets (no new column) and shows "Over 120 Hz budget" on the performance page and
  "Frames over 8.3 ms" on issue pages.
- `ashlar-mcp`: `app_stats {reset?}` and `app_logs {limit?, level?}` tools over the runtime's
  `app.stats` and `app.logs`, listed only when the app reports those methods.
- `ashlar-assistant`: `HistoryStore::offload_images` keeps only a chat's newest pictures in
  memory (older ones stay in its history folder as file references, which saving keeps, loading
  reads back and requests leave out); `HistoryStore::read_image` and `Image::is_offloaded` for
  showing them.
- `ashlar-telemetry`: `TelemetryConfig::build`: a build identifier (such as the commit a release was
  packaged from) sent with every report, so a server can tell release sessions from developer ones.
- `ashlar-assistant`: `Conversation::memory()`: the bytes a chat holds (pictures dominate) and how
  many pictures, for memory readouts.
- `ashlar-ui`: `idle`: `IdleTrim` fires once per quiet spell (30 s idle, 10 s in the background)
  so an app can trim its caches, `MemoryReport` (named cache sizes plus the process footprint for
  `perf.stats`), `texture_memory`, and `FrameRate`, a frame rate that reads 0 while no frames run;
  `wake::Reason::IDLE`.
- `ashlar-native`: `display` (feature, on by default): `check()` says when this process cannot show
  a window at all (Windows: a service's window station, as in session 0), where wgpu would panic
  configuring the window's surface. `ashlar-app` checks it before opening its window and returns
  `Error::NoDesktop` instead of crashing.
- `ashlar-native`: `memory` (feature, on by default): `process_memory()` reads the process's
  footprint and resident size the way the OS's tools do; `release_free_memory()` returns the
  allocator's free pages to the system (macOS keeps freed regions dirty otherwise).
- `ashlar-assistant`: saved chats (`history`: `HistoryStore`, `HistorySaver`; versioned, crash-safe,
  pictures in their own files), `@` mentions (`mention`, `Block::Mention`), `/` commands with saved
  prompts (`slash`), and `/compact` summaries and chat titles (`compact`).
- `ashlar-assistant`: `skills` (`Skills`: skills listed by name and description in the system prompt,
  loaded with the `load_skill` / `read_skill_file` tools, picked as `/name`; from docs categories,
  skill folders and integrations), `SlashAction::Skill` and `SlashAction::External`, and feature
  `integrations` (`integration`: an integration's skills and MCP prompts as commands).
- `ashlar-agents`: `skills::parse` and `skills::load_dir` read `SKILL.md` skill folders back from disk.
- `ashlar-integrations`: MCP prompts (`Integrations::prompts`, `get_prompt`) and resources
  (`skill_resources`, `read_resource`), and skill folders in a bundle (manifest `skills`, `skill_dirs`).
- `ashlar-ui`: `markdown`, Markdown for chat replies (headings, lists, tables, code blocks with Copy,
  inline styles and links), safe for text that is still streaming in.

### Security

- `ashlar-trace-server` and the trace examples use rusqlite 0.41 (bundled SQLite from
  libsqlite3-sys 0.39), fixing RUSTSEC-2026-0459 (CVE-2026-119375, stale memory read through a
  malicious database and `-wal` file pair); the yanked async-executor 1.14.0 is replaced by 1.14.1.

## [0.1.0] - 2026-10-27

First public release, on crates.io.

### Added

- The umbrella crate, published as `ashlar-rs` (imported as `ashlar`), and the `ashlar-*` crates:
  app runtime, UI components on egui, documents with undo and autosave, commands, background jobs,
  files and settings, logging and crash safety, native OS features, the agent control port, CLI,
  MCP (stdio and HTTP), agent setup, an in-app assistant harness, integrations, UI debugging,
  agent docs, update checks, printing, string obfuscation, and self-hosted telemetry.
- Examples: `notes`, `gallery`, and the telemetry demos.
- A release workflow that publishes to crates.io only from a version tag on `main`, after the full
  CI gate passes on that commit.

### Changed

- HTTPS in `ashlar-telemetry`, `ashlar-update`, `ashlar-assistant` and `ashlar-integrations` uses
  rustls with the `ring` provider, replacing the unmaintained `rustls-rustcrypto`.

### Fixed

- `ashlar-trace-server` sends `Connection: close` with a refusal made before the request body is
  read (bad key, rate limit, storage full) and with every dashboard form response, so a client
  never reuses a connection the server is about to drop.
- `ashlar-trace` on Windows reads each spooled file's size from the file itself, not the folder
  listing, which can show a stale size for a file still being written; a segment that looked empty
  was deleted.
- `ashlar-trace` keeps the count of a batch the server refuses while the tracer shuts down, so the
  next process reports it instead of the count being lost.

[Unreleased]: https://github.com/Himasnhu-AT/ashlar/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/Himasnhu-AT/ashlar/releases/tag/v0.1.0
