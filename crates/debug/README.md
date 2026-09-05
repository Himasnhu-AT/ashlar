# ashlar-debug

Drive a running ashlar app's UI from the shell, the way a browser automation CLI drives a web
page. You take a snapshot of the UI as an outline with short refs, act on those refs, wait, read
values back and take (annotated) screenshots. A text file of these commands is a UI test. Every
command makes one or two control-protocol calls (`docs/control-protocol.md`) through
`ashlar_control::Client`.

## Quickstart

```sh
cargo install --path crates/debug        # or: cargo run -p ashlar-debug -- …

ashlar-debug open ./target/debug/notes   # launch it with a control port; becomes the session
ashlar-debug snapshot -i                 # interactive elements only
# - textbox "Title" [ref=e2]
# - button "Save" [ref=e3]
# - button "Delete" [ref=e5] [disabled]
ashlar-debug fill @e2 "Groceries"
ashlar-debug click @e3
ashlar-debug wait --text "Saved"
ashlar-debug get text id=status          # Saved: Groceries
ashlar-debug screenshot --annotate       # prints the PNG's path and each ref drawn on it
ashlar-debug close                       # quits the app
```

You can try this without an app: `ashlar-debug open ashlar-debug -- demo` starts the built-in demo
(a fake app that answers the same methods). Use the full path to the binary if it is not on `PATH`.

## Targets

| Written | Means |
|---|---|
| `@e3`, `e3` | a ref from the latest snapshot (it stays valid while the element exists) |
| `id=file.save`, `#file.save` | an automation id |
| `text=Save`, `button=Save` | the first visible element with that name (and role) |
| `120,40` | a point in window coordinates (logical points) |

When a ref is stale, the command fails with `stale_ref` and a hint to take a new snapshot.

## Commands

```text
open <exe> [--wait MS] [-- args…]     connect <port>      close [--quit]      status      sessions
snapshot [-i] [--scope T] [--depth N] [-c]
click <T> [--button …] [--count N] [--mods Shift+Mod]    dblclick <T>    hover <T>    focus <T>
fill <T> <text> [--submit]    type <text>    press Mod+S | "Tab Tab Enter"
scroll [T] [up|down|left|right] [px] | --dx N --dy N    scrollintoview <T>    drag <A> <B> [--steps N]
get text|value|rect|enabled|visible|focused|checked <T>
wait <T> | <ms> | --text S | --gone T | --enabled T | --idle   [--timeout MS]
screenshot [out.png] [--target T] [--annotate] [--highlight T] [--pad N] [--no-raise]
highlight <T> [--ms N]
exec <command> [json|-]    commands [query]    menu [id]    logs [-n N]    stats    devtools [open|close|state]
call <method> [json|-]     batch <file|-> [--verbose]    trace start <dir> | trace stop    demo
```

Global options: `--session NAME` (default `default`, or `ASHLAR_DEBUG_SESSION`), `--port N`
`[--key-dir D]` or `--app NAME` (talk to a running app without a session), `--json` (one
`{"ok":…}` line on stdout), `--call-timeout MS`. Exit codes: 0 ok, 1 failed, 2 usage.

To take a screenshot, the app raises a covered window once. Automated runs that must not steal
focus can pass `--no-raise` to `screenshot`, open the session with `open --no-raise` (or
`connect --no-raise`), or set `ASHLAR_DEBUG_NO_RAISE=1`. Each of these sends `raise: false`, and
traces respect them too.

## Sessions and the control key

The control port needs the app's key, a file `control-key.local-<port>` that the app writes when it
serves the port.

- **`open`** picks a free port, starts `<exe> [args] --control <port>` with
  `ASHLAR_CONTROL_KEY_DIR=<session folder>/key`, sends the app's output to `<session folder>/app.log`,
  waits until it answers `app.ping`, then records the session. Apps built on ashlar-app write their
  key where `ASHLAR_CONTROL_KEY_DIR` says. If an app ignores the variable, `open` looks for its key
  in the OS settings folders.
- **`connect <port>`** and **`--port N`** use `--key-dir`, then `ASHLAR_CONTROL_KEY_DIR`, then
  search the OS settings folders a few levels deep for `control-key.local-<port>`.
  **`--app NAME`** searches the same folders for any key whose path contains NAME and uses the
  first one that answers.
- Sessions live in `<user cache>/ashlar-debug/sessions/<name>.json`. Set `ASHLAR_DEBUG_HOME` to
  keep them somewhere else. `close` quits an app that `open` started (and kills it if it does not
  quit within 5 s). For an app you only connected to, `close` forgets the session and leaves the
  app running; add `--quit` to quit it as well.

## UI tests: `batch`

```text
# test/save.ui: one command per line, as on the command line
fill id=title "Groceries"
click button=Save
wait --text "Saved"
assert text id=status "Saved: Groceries"
assert enabled button=Delete
assert count role=listitem >= 3
```

`ashlar-debug batch test/save.ui` runs the lines in order and stops at the first failure. It reports
the line, what was expected and what it found, and exits 1. Assertions:
`text|value|contains <T> <expected>`,
`visible|hidden|enabled|disabled|focused|checked|unchecked <T>`,
`count <filters> <op> <n>`, `exists|missing <filters>` (filters: `role=`, `name=`, `id=`,
`value=`; `~=` matches a substring). `expect` works the same as `assert`. `echo` and `sleep <ms>`
also work in a script.

## Traces

`trace start <dir>` records, after every action (click, fill, press, …), `NNN-<verb>.png`,
`NNN-<verb>.txt` (the snapshot) and a line in `steps.jsonl` (the command, ok or error).
`trace stop` ends the recording. Use it to see what a failing script saw at each step.

## MCP

The same capabilities are available as tools in `ashlar-mcp` (`ui_snapshot`, `ui_click`, `ui_fill`,
`ui_get`, `ui_wait`, `screenshot` with `annotate`, …). A tool is listed only when the app reports
its method in `app.version`.

See [`docs/debugging-uis.md`](../../docs/debugging-uis.md) for the guide.
