# Changelog

All notable changes to ashlar are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, a minor version
(0.x) may contain breaking changes.

## [Unreleased]

### Fixed

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

- `ashlar-assistant`: `HistoryStore::offload_images` keeps only a chat's newest pictures in
  memory (older ones stay in its history folder as file references, which saving keeps, loading
  reads back and requests leave out); `HistoryStore::read_image` and `Image::is_offloaded` for
  showing them.
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
