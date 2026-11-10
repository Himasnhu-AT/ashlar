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
   - `system` (Windows `ShellExecuteW`, `SystemParametersInfoW`, `GetUserPreferredUILanguages`
     (added 2026-11-10 so apps started from the desktop, which have no `LANG`, find the user's
     languages); and for `bring_to_front`, added 2026-11-10: `IsIconic`, `ShowWindow`,
     `SetForegroundWindow`, `BringWindowToTop`, `AttachThreadInput`)
   - `mac_shell` (macOS: replacing the implementation of winit's window delegate
     `observeValueForKeyPath:ofObject:change:context:` with a checked wrapper that calls the
     original, `class_addMethod` of `applicationShouldHandleReopen:hasVisibleWindows:` on winit's
     application delegate, and the `msg_send!`s that inspect the KVO change); added 2026-11-10 so a
     nil appearance during screen capture doesn't end the app and the Dock icon shows the window
   - `menu::popup` (muda's `show_context_menu_for_nsview` / `show_context_menu_for_hwnd` on the
     window's own view or HWND, retaining the macOS view, and posting the macOS menu to the main
     run loop) and `drag_out` (macOS: an `NSDraggingSource` class, retaining the window's view, an
     `NSDraggingItem` frame); added 2026-11-10 for context menus and dragging files out
   - `single_instance` (Windows `AllowSetForegroundWindow`, one block; added 2026-11-10 so the
     second launch lets the first come to the front)
   - `menu::native::attach_window` (muda's `init_for_hwnd`) and `menu::native::accelerators`
     (Windows: `SetWindowSubclass`, `TranslateAcceleratorW`, `DefSubclassProc`; added 2026-11-10
     so menu shortcuts work without a hook in winit's message loop)
   - `terminate` (Unix, through `libc`: `pipe`, `fcntl`, `sigaction`, and in the handler only
     `write`, `signal` and `raise`, which are async-signal-safe; added 2026-09-10 so a stdio server
     can end its telemetry session when it is asked to stop)
   - `memory` (macOS `proc_pid_rusage`, glibc `malloc_trim`, Windows `GetProcessMemoryInfo`;
     added for idle apps that report their footprint and hand freed memory back)
   - `display` (Windows `GetProcessWindowStation`, `GetUserObjectInformationW`; added 2026-10-27 so an
     app started where no window can be shown says so and exits instead of panicking)
   - `notify::macos` (the `UNUserNotificationCenterDelegate` class, its `init`, and
     `setUserInfo:`) and `notify::windows` (`SetCurrentProcessExplicitAppUserModelID`,
     `CoInitializeEx`); added 2026-11-10 so banners show while the app is frontmost, clicks are
     reported and Windows gets toasts
   - `session_end` (macOS: `class_addMethod` of `applicationShouldTerminate:` on winit's
     application delegate class, and reading that method's implementation; Windows: a hidden
     window's class, window and message loop for `WM_QUERYENDSESSION` / `WM_ENDSESSION`); added
     2026-11-10 so Dock Quit, logout and shutdown get the "Save changes?" question and the
     settings, autosave and telemetry flush

   Everything else (dialogs, clipboard, secrets, Linux notifications, macOS progress and preferences,
   single instance off Windows, tray, global shortcuts) uses safe APIs of rfd, arboard, keyring-core, objc2 and zbus.
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
