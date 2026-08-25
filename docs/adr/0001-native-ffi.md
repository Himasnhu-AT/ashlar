# ADR 0001: `unsafe` OS FFI in `ashlar-native`, and nowhere else

- **Status:** accepted (2026-08-18, wave 1)

## Context

The workspace sets `unsafe_code = "forbid"` (AGENTS.md §0). Some OS features every desktop app
needs have no safe binding: registering Apple event handlers (files opened from Finder),
ending an `NSProcessInfo` activity, `SetThreadExecutionState`, `ITaskbarList3`, `ShellExecuteW`,
`SystemParametersInfoW`, and attaching a muda menu to a Win32 window. The bindings used
(`objc2-*`, the official `windows` crate, muda) are generated
declarations.

## Decision

Allow `unsafe` in exactly one crate, `ashlar-native`, for OS FFI. Containment rules (the same as
the video editor's platform crate these were adapted from):

1. The crate does not use `lints.workspace = true`. Its own `[lints]` table copies the workspace
   lints with `unsafe_code = "deny"` (not `forbid`) and adds `clippy::undocumented_unsafe_blocks
   = "deny"`. Only FFI modules carry `#[allow(unsafe_code)]`:
   - `power` (macOS `endActivity:`, Windows `SetThreadExecutionState`)
   - `open_files` (macOS: the Apple event handler class and its registration)
   - `progress` (Windows `ITaskbarList3`)
   - `system` (Windows `ShellExecuteW`, `SystemParametersInfoW`)
   - `menu::native::attach_window` (one block: muda's `init_for_hwnd`)

   Everything else (dialogs, clipboard, secrets, notifications, macOS progress and preferences,
   single instance) uses safe APIs of rfd, arboard, keyring-core, objc2 and zbus.
2. Every `unsafe` block has a `// SAFETY:` comment saying why it is sound.
3. The public API is safe: no `pub unsafe fn`, no raw pointers or FFI types in signatures.
   Window handles come in through `raw_window_handle::HasWindowHandle`, so a caller can't pass
   a dangling HWND. Failures are `Result<_, NativeError>`.
4. No panic crosses an FFI boundary: the Apple event handler methods and the notification
   completion block run under `catch_unwind`; calls into third-party bindings that could panic
   (rfd, arboard, muda, keyring) go through `guarded`, which turns a panic into
   `NativeError::Os`.
5. The crate compiles on every target. OS bindings are target-specific optional dependencies
   enabled per cargo feature; elsewhere a call returns `NativeError::Unsupported`. With default
   features it passes `cargo check --target wasm32-unknown-unknown`.

## Consequences

- Changes to the FFI modules need the same scrutiny as any FFI code. A new use of `unsafe`
  goes into one of the listed modules with a SAFETY comment, or needs an update to this record.
- Main-thread requirements (macOS menus, Dock tile, Apple event handlers) are checked at run
  time with `MainThreadMarker` and reported as errors instead of crashing.
