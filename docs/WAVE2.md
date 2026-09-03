# Wave 2 notes (integrator)

Carry-overs from wave 1 reports, to resolve in ashlar-app / follow-ups:
- control `Client` retries once on a dead kept connection: a mutating command could run twice if the app
  crashed mid-request. Retry only when the request was never written (connect/write failure), never after.
- One menu model: `ashlar_native::menu::MenuSpec` and the ashlar-ui menu model overlap. Keep one (serde,
  structure/state hash) and have the other render it. `menu::Accelerator` must defer to
  `ashlar_command::shortcut`.
- rfd: ashlar-native uses `default-features=false, features=["xdg-portal"]` (no libwayland build dep).
  Any other rfd user must match, or feature unification re-enables `wayland`.
- macOS: `OpenFiles::listen_os` must run on the main thread before the event loop; untested in a bundle.
- Notifications need a bundled app (macOS) / AUMID (Windows).
- Autosave uses std fs locks: web needs a different crash-detection strategy over `FileOps`.
- History branch switching only from the fork state (no goto-any-node).
- Undo history and telemetry are host concerns: ashlar-app's host glues `Registry::execute` to `Store`
  (one undo step per mutating top-level command) and to tracer.

## Open after wave 2
- ashlar-ui components (toasts, status_bar, printing, keymap editor) still call `request_repaint` directly → route through
  `ashlar_ui::wake` with proper reasons and extend the wake source scan to crates/ui.
- `FormatError::Corrupt(String)` has no structured `JsonError` detail (tuple variant; change at next breaking release).
- ashlar-mcp gaps before a large app can drop rmcp: `notifications/progress`, `notifications/cancelled`,
  answering `ping` while a call runs (server is synchronous), resources (+ cache hints), destructive/idempotent
  tool annotations (only read-only today), app-defined custom tools beyond the command vocabulary.
- ashlar-control: optional public `KeyFile` constructor / configurable key path.
- ashlar-command: let `Host::before` return `S::Error` through `execute_host` (an `ExecError::Host` path for
  before-hook errors), so hosts need no side stash to keep their error variant.
