# Changelog

All notable changes to ashlar are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, a minor version
(0.x) may contain breaking changes.

## [Unreleased]

### Added

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

### Changed

- `ashlar-app`: a command run from the keyboard (or anything else before the view draws) no longer
  asks for a `document` frame: the frame that ran it already shows the change. The runtime also
  stops copying and comparing the view state and formatting the window title on frames that did
  not change them.
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
  crafted database and `-wal` file pair); the yanked async-executor 1.14.0 is replaced by 1.14.1.

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
