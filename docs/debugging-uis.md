# Testing and debugging UIs with ashlar-debug

You can drive any ashlar app that serves a control port from outside the process, the way a
browser automation tool drives a page. You read the UI as an outline of elements with short refs,
act on those refs, wait for the result and check it. Three clients use the same methods:

| Who | How |
|---|---|
| You, in a terminal | `ashlar-debug` (crate `ashlar-debug`) |
| A test or CI job | `ashlar-debug batch file.ui`, or `ashlar_debug::run` from a Rust test |
| An agent | the `ui_*` tools of `ashlar-mcp` (`<tool> mcp --connect PORT`) |

The methods (`ui.snapshot`, `ui.click`, `ui.fill`, `ui.wait`, …) are specified in
[control-protocol.md](control-protocol.md).

## A session

```sh
ashlar-debug open ./target/debug/my-app      # free port, --control <port>, waits for app.ping
ashlar-debug snapshot -i
# - textbox "Title" [ref=e2]
# - button "Save" [ref=e3]
ashlar-debug fill @e2 "Groceries" --submit
ashlar-debug wait --text "Saved" --timeout 3000
ashlar-debug screenshot --annotate           # → ~/…/ashlar-debug/screenshots/shot-….png
ashlar-debug close
```

Each command runs as its own process. The session file remembers the port and the key folder, so
the next command finds the app again. Use `--session NAME` to drive two apps side by side. To reach
an app that is already running with `--control <port>`, use `ashlar-debug connect <port>` or
`--port <port>` (its key is found in the OS settings folders), or `--app <name>`.

The output is plain text by default. With `--json`, each command prints one `{"ok":true,"result":…}`
or `{"ok":false,"error":…,"detail":…}` line. Exit codes are 0 for success, 1 when the app refused,
an assertion failed or nothing answered, and 2 for a usage error.

## Refs and targets

A snapshot gives every element a ref (`e3`). Refs are stable: the same element keeps its ref
across snapshots for as long as it exists. A ref that no longer exists fails with `stale_ref`, and
you take a new snapshot. Instead of a ref you can also write `id=<automation id>` (the most robust
choice in a test script, since it survives layout changes), `text=<label>`, `<role>=<label>`
(`button=Save`), or a point `x,y`.

If you write an app and want stable test targets, give the important widgets automation ids. Use
the ids of the commands they run (`file.save`), as the menus already do.

## UI tests

Write a script with one command per line, plus assertions:

```text
# tests/ui/save.ui
open ./target/debug/my-app
fill id=title "Groceries"
press Mod+S
wait --text "Saved"
assert text id=status "Saved: Groceries"
assert enabled id=edit.delete
assert count role=listitem >= 1
close
```

`ashlar-debug batch tests/ui/save.ui` stops at the first failure:

```text
FAIL tests/ui/save.ui:7: assert enabled id=edit.delete
     expected id=edit.delete to be enabled, but enabled is false
error: tests/ui/save.ui:7: expected id=edit.delete to be enabled, but enabled is false
hint: 5 step(s) passed before line 7
```

Each step's result is in the `--json` output (`detail.steps`). If you add `trace start <dir>` at the
top, every action leaves a screenshot and a snapshot in `<dir>`, numbered in order
(`001-click.png`, `001-click.txt`, one line per step in `steps.jsonl`). That is the first place to
look when a script fails on a CI machine. With `--port` or `--app` (no session) the trace lasts for
that one batch run; outside a batch they refuse `trace` with a usage error, since there is no session
to keep it in.

Running a script from Rust gives you the same thing in-process. `ashlar_debug::run(args, &env, &mut io)`
takes an `Env` that points the state and key search at a temporary folder:

```rust
let env = ashlar_debug::Env::under(&tmp); // state in tmp/home, keys looked for in tmp/config
let code = ashlar_debug::run(vec!["--port".into(), port.to_string(), "--key-dir".into(), key_dir, "batch".into(), "-".into()], &env, &mut io);
```

`examples/notes/tests/debug.rs` does this against the windowed runtime on a headless event loop,
so it needs no display. It also has a test that opens a real window, which runs only when
`ASHLAR_DEBUG_REAL_WINDOW=1` is set.

### Scripts in process, on virtual time

`Driver::run_ui(script)` runs the same script language on a `Driver`, with no port and no thread.
`sleep N` and `wait N` let virtual time pass instead of the wall clock, and waits such as
`wait --text` run frames on the virtual clock, so a script gives the same steps on every run and
takes milliseconds:

```rust
let mut d = Driver::new(notes::app().runtime());
let report = d.run_ui("exec note.add '{\"title\": \"x\"}'\nsleep 2000\nassert exists id=notes.add\n")?;
assert_eq!(report.passed, 3); // report.steps: one {"line", "command", "ok", "result"} per step
```

A failure is a `ScriptError` naming the line, with the same detail as `batch --json`. Verbs about
sessions, processes and traces (`open`, `connect`, `close`, `sessions`, `trace`, `demo`) fail in
process. To run scripts against something else, implement `ashlar_debug::Transport` (`call`, and
`pause` for time) and call `ashlar_debug::run_script`; the TCP `Client` is one such transport.

### One command: `--headless`

Every binary whose `main` calls `App::run` takes `--headless`: no window, the app on a virtual
clock in a fresh temporary folder (never your settings or documents), real pixels when there is
a GPU (grey otherwise, with a note on stderr):

```sh
notes --headless --size 1024x700 --scale 2 --theme dark --script add.ui --at 1.5 \
      --snap out.png --tree tree.json
```

`--size` is in points (default 1280x800) and the PNG is that times `--scale`. `--script` runs a
`.ui` script first, `--at` lets that many seconds pass before the picture, and `--tree` writes the
`ui.snapshot` JSON. Exit codes match `ashlar-debug`: 0 done, 1 a step failed, 2 bad arguments.
`examples/notes/tests/headless.rs` checks the PNG size and the tree.

### Real pixels with no window

A `Driver` gives flat grey screenshots by default, which keeps tests that need no GPU fast. For
real pixels, build it with `Driver::with_renderer(runtime, Renderer::wgpu())`: frames render on a
wgpu device with no window or surface, so `ui.screenshot`, annotate and trace show what a window
would, at the scale set with `Driver::pixels_per_point`. A software adapter is preferred when
there is one. On a machine with no adapter, `Renderer::wgpu()` falls back to grey and
`Renderer::reason()` says why. Every screenshot reply says which was used: `"renderer": "wgpu"` or
`"renderer": "none"`. Renders of the same state are byte-identical on the same machine, but can
differ between GPUs, so compare pixels across machines only with a tolerance.
`crates/app/tests/headless_render.rs` shows the pattern, including how to skip without a GPU.

## Without an app: the demo

`ashlar-debug demo --control <port>` serves a small fake app with a text field, buttons, a
checkbox, a list and a delayed "Load". It answers every debugger method deterministically. The
crate's own tests drive it, and you can use it to try the CLI or to check an MCP client:
`ashlar-debug open ashlar-debug -- demo`.

## Debugging a live app

- `snapshot` (full tree) or `snapshot --scope @e8` shows what is on screen and why a target does
  not match: a disabled element shows `[disabled]` and a focused one `[focused]`.
- `highlight @e3` flashes an outline in the app window, which helps when you are pairing with a
  human.
- `screenshot --annotate` draws each interactive element's ref on the picture, next to the element
  rather than over its text. A covered or minimized window, or one on a locked screen, presents no
  frames; `ashlar-app` then runs a UI pass of its own while debugger work waits and renders
  screenshots off screen, so snapshots, input and screenshots work without bringing it to the
  front. An app without an off-screen renderer has its window raised once for a screenshot, and
  the request fails after a few seconds if that does not help. Use `screenshot --no-raise`, a
  session opened with `open --no-raise` (or `connect --no-raise`), or `ASHLAR_DEBUG_NO_RAISE=1` for
  CI and background runs that must never steal focus. The MCP `screenshot` tool takes
  `raise: false` for the same purpose.
- `logs -n 50`, `stats` and `devtools open` show the app's recent log lines, its frame and job
  statistics, and the in-app inspector.
- `clock pause`, `clock step 16.7` and `clock resume` hold the app's animation clock: pause, take
  screenshots or `get rect` at an exact moment, step one frame at a time, then resume with no jump.
  Input still works while paused. The same lines work in `.ui` scripts. See
  [control-protocol.md](control-protocol.md#ui-methods) for what else stands still.
- `call <method> '{json}'` sends any control method as is.

## Launch time and memory

- `stats` (`app.stats`) reports `launch`: `to_window_ms` (from the first line of `run()` until
  eframe has built the window and its wgpu surface), `to_first_frame_ms` (the end of the first
  frame) and `launch_to_present_ms` (that frame painted and presented). The app also logs them
  once at `info`. Headless runs report none.
- `<PREFIX>_READY_FILE=path` (the prefix is the app name in capitals, like `<PREFIX>_CONTROL_PORT`)
  makes the app write `{"pid": …, "ready_ms": …}` to `path`, atomically, once its first frame is
  presented: a launcher or a test waits for the file instead of sleeping.
- `cargo xtask launch` builds `examples/hello` in release, launches it 9 times (`--runs N`) and
  prints `bench: launch_to_present_p50/p95` and `spawn_to_ready_p50/p95` (from spawning the
  process). `cargo xtask launch --memory` lets it idle 10 s (`--settle SECS`) and prints the RSS
  and the physical footprint and its peak (`vmmap`, macOS). Both open real windows, so they are
  manual and never run in CI; results swing with machine load, so compare runs made back to back.

## Agents (MCP)

`ashlar-mcp` lists `ui_snapshot`, `ui_click`, `ui_fill`, `ui_hover`, `ui_focus`, `ui_press`,
`ui_get`, `ui_wait`, `ui_scroll_into_view`, `ui_highlight` and `screenshot` (with `target`,
`annotate`, `highlight`, `pad` and `raise`) when the app reports these methods in `app.version`,
along with `ui_inspect`, `ui_drag`, `ui_scroll`, `ui_key`, `ui_type`, `ui_menu_list`,
`ui_menu_invoke` and `ui_set`. `app_stats {reset?}`, `app_logs {limit?, level?}` and
`ui_clock {pause?, step_ms?, resume?}` are the CLI's `stats`, `logs` and `clock`. The full table with every argument is in
[control-protocol.md](control-protocol.md#mcp-tools). Targets are ref strings (`"@e3"`,
`"id=file.save"`) or objects. `ui_snapshot` returns the outline as text and adds the element list
as JSON when you pass `elements: true`. A typical loop is: snapshot, act on a ref, wait, then
snapshot or `ui_get` to check.

## The control key

The control port accepts only clients that hold the app's key. The app writes it to
`<key folder>/control-key.local-<port>` with mode 0600. The key folder is the app's settings
folder, unless whoever started the app set `ASHLAR_CONTROL_KEY_DIR`; `ashlar-debug open` sets it to
a private folder inside its session. The CLI never sends the key anywhere but 127.0.0.1.
