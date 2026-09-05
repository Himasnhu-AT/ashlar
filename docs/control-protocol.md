# Control protocol

Every ashlar app can be driven by an agent in three ways, all speaking **one method vocabulary**:

| Surface | Transport | Crate |
|---|---|---|
| Control port | JSON lines over TCP `127.0.0.1:<port>`, keyed | `ashlar-control` (`Server`, `Client`) |
| Child process | JSON lines over stdin/stdout, no key (private pipe) | `ashlar-control::serve_stream`, `<app>-cli serve` |
| MCP | JSON-RPC 2.0 over stdio; tools map onto the methods below | `ashlar-mcp` |
| CLI | one-shot verbs, one-line replies | `ashlar-cli` |

Protocol version: **1** (reported by `app.version`).

## Framing

One JSON object per line (`\n`, an optional `\r` is stripped), each way, one reply per request, in
order.

Request:

```json
{"id": 7, "key": "<64 hex digits>", "method": "engine.execute", "params": {"command": "edit.undo"}}
```

- `id` (optional, any JSON): echoed in the reply; omitted from the reply when absent.
- `key`: the app's control key (TCP only; see Keys). Not needed after `auth` on the same connection.
- `method` (required string).
- `params` (optional; `null` or absent means `{}`).

Reply:

```json
{"id": 7, "ok": true, "result": …}
{"id": 7, "ok": false, "error": "human-readable message", "detail": …}
```

`detail` is optional machine-readable context: for a failed command, the command error
`{"kind": "unknown"|"disabled"|"bad_params"|"failed"|"panicked", "id": …, …}`; for a failed batch,
`{"steps": [...]}`.

## Hardening (TCP)

The server is built so that a hostile local process or web page cannot drive or crash the app:

- **Loopback only.** Binds `127.0.0.1`; the client only connects there.
- **Key.** Once the port is bound the app writes 32 random bytes (64 hex digits) to
  `<dir>/control-key.<instance>`, created with mode 0600, where `<dir>` is a folder the app chooses
  (its settings folder) and `<instance>` is the Flatpak instance id or `local-<port>`. The file is
  removed when the server is dropped; a stale one is replaced. Every request must carry the key, as
  the top-level `key` member or once per connection with `auth`. A missing or wrong key gets
  `{"ok":false,"error":"unauthorized"}` and the connection is closed. Keys are compared in constant
  time. `params.key` is an ordinary parameter for every method but `auth` (`ui.key {"key":"Enter"}`).
- **Strict lines.** A line that is not a JSON object with a string `method` (invalid UTF-8, bad JSON,
  an array, a number, an HTTP request line) gets one error reply and the connection is closed, so
  nothing after it runs. This is what stops a browser's cross-origin `fetch` POST: its request line
  `POST / HTTP/1.1` is rejected before the JSON body is read.
- **Bounded lines.** Longer than `max_line` (4 MiB default): error reply, the rest of the line is
  drained (at most 64 MiB, 2 s) so the client can read the error, then close. A peer can never make
  the server buffer more than `max_line`.
- **Connection cap.** At most `max_connections` (16) at once; extra ones get
  `{"ok":false,"error":"too many control connections"}` and are closed.
- **First-request deadline.** A connection must send an authorised request within 30 s. It is one
  overall deadline: blank lines or a slow drip of bytes do not extend it. After that, no idle limit.
- **Reply timeout.** The app thread answers within 60 s or the client gets `timeout: …`.
- **Never panics.** No `unwrap`/indexing on input; junk never reaches the app thread; a request the
  app drops unanswered replies with an error by itself.

## Standard methods

Answered by `ashlar_control::handle` from any `Dispatch` (headless or windowed):

| Method | Params | Result |
|---|---|---|
| `auth` | `{key}` or top-level `key` | `{}` (transport-level) |
| `app.ping` | — | `"pong"` |
| `app.version` | — | `{name, version, protocol, methods: [...]}`: `methods` lists every method this app answers, so clients know whether `ui.*` works |
| `engine.commands` | `{query?}` (case-insensitive, matches id, label, menu path) | `[CommandInfo]`: `{id, label, menu, shortcut, params_schema, params_text, enabled, disabled_reason, mutates}` |
| `engine.execute` | `{command, params?}` | the command's result |
| `engine.batch` | `{commands: [{command, params?}], keepGoing?}` | `[{command, ok, result}]`; on failure an error with `detail.steps` (stops at the first failure unless `keepGoing`; at most 10 000 steps) |

## UI methods

Routed to the app's `UiHandler`. A handler may keep a request and answer it on a later frame.
Headless sessions refuse them with a hint. The windowed runtime answers the app-level ones itself
and hands the debugger methods (next section) to `ashlar_ui::devtools`:

| Method | Params | Result |
|---|---|---|
| `ui.inspect` | — | UI state plus every registered element `{id, label, role, rect:[x,y,w,h], enabled, value?, selected?}` |
| `ui.menu.list` | — | the menu tree with enablement |
| `ui.menu.invoke` | `{id}` | as `engine.execute` |
| `ui.set` | app-defined view state (`{theme?, workspace?, …}`) | `{}` |
| `app.quit` | `{discard?}` | `{closing}`, then the app quits |
| `app.stats` | `{reset?}` | frame times, passes, jobs, `wakes` (frames and requests per reason) |
| `app.logs` | `{limit?, level?}` | `{lines: [{unix, level, target, message}]}` |

Apps may add their own methods (`ui.timeline.hit`…); list them in `UiHandler::methods` so they show
in `app.version`.

## Debugger methods

Drive the UI the way a browser automation tool drives a page: snapshot the element tree with short
refs, act on refs, wait, read, screenshot. Implemented by `ashlar_ui::devtools` (an egui plugin),
so an app that does not use `ashlar-app` gets the same methods by passing requests to
`devtools::handle(ctx, request)` (see that module's docs). `app.version` lists them when available.

### Elements and refs

The element tree is egui's AccessKit output (every egui widget: role, label, value, state, bounds)
merged with the ashlar automation registry (dotted `id`s, component roles). An element:

```json
{"ref": "e12", "id": "notes.title", "role": "textbox", "name": "Title", "value": "Groceries",
 "enabled": true, "focused": false, "selected": false, "checked": null, "visible": true,
 "rect": [x, y, w, h], "parent": "e3"}
```

`id`, `value`, `selected`, `checked` and `parent` are omitted when they do not apply. `rect` is the
on-screen part in logical points. Roles: `button checkbox switch radio radiogroup textbox searchbox
slider spinbutton combobox link label image colorwell panel window dialog scrollbar separator
progressbar menu menubar menuitem tab tablist list listitem tree treeitem table row cell
columnheader toolbar status alert tooltip group generic`.

A ref stays the same for an element across snapshots while it exists (keyed by its automation id,
else by its egui widget id and role); every `ui.snapshot` increments the `generation`. The first
debugger request turns egui's AccessKit output on for the rest of the session.

### Targets

Every method that acts on an element takes a target, as `target: {...}` or flat in the params:

| Target | Matches |
|---|---|
| `{ref: "e12"}` (also `"@e12"`, `12`, or the bare string `"@e12"`) | the element of a snapshot |
| `{id: "file.save"}` | the (first visible) element with that automation id |
| `{text: "Save", role?: "button"}` | the first visible element with that name: exact, then ignoring case, then containing it; interactive elements first. Not accepted flat (`text` is the typed text of `ui.fill`/`ui.type`) |
| `{x, y}` | that point (logical points); the element there, if any |

Errors carry `detail.kind`: `stale_ref` (`{kind, ref, generation}`: the element of that ref is
gone; take a new snapshot), `not_found` (`{kind, generation}`), `timeout` (`{kind, elapsed_ms}`).

### Methods

All answer once the frames they need have run; input methods return `{}` after the injected input
has been processed by a frame.

| Method | Params | Result |
|---|---|---|
| `ui.snapshot` | `{interactive?: false, compact?: false, scope?: target, depth?}` | `{generation, tree, elements: [element], focused: ref│null}` |
| `ui.click` | target, `button?` (`primary`/`secondary`/`middle`), `count?` (1–3), `modifiers?` | `{}` |
| `ui.hover` / `ui.move` | target (`ui.move`: `{x, y}`) | `{}` |
| `ui.focus` | target | `{}`: AccessKit focus, or a click for text fields |
| `ui.fill` | target?, `text`, `submit?` | `{}`: click the target, select all, type `text` (or delete when empty), Enter when `submit`; without a target, the focused field |
| `ui.type` | target?, `text` | `{}`: typed into the target (clicked first) or the focused widget |
| `ui.press` | `keys` (`"Mod+Shift+S"`, sequences `"G G"`; `ashlar_command::shortcut` syntax; `key` is accepted too) | `{}` |
| `ui.key` | `{key, modifiers?}` | `{}` (one key; `modifiers`: `{command, shift, alt, ctrl}`, `command` = ⌘ on macOS, Ctrl elsewhere) |
| `ui.scroll` | target? (default: the window centre), `dx`, `dy` | `{}` |
| `ui.scroll_into_view` | target | `{}` |
| `ui.drag` | `{from: target, to: target, steps?: 8, modifiers?}` | `{}` |
| `ui.get` | target, `what`: `text` (a field's content, else the name), `name`, `value`, `rect`, `enabled`, `visible`, `focused`, `checked`, `selected`, `role`, `element` | `{value}` |
| `ui.wait` | `{for: element│gone│text│enabled│idle, target?, text?, timeout_ms?: 5000}` (`for` defaults to `text` when `text` is given, else `element`) | `{elapsed_ms}`; error `timeout` |
| `ui.screenshot` | target? (crop; also the legacy `{id}`), `pad?`, `annotate?`, `highlight?: target` | `{png: <base64>, width, height, annotations?: [{ref, rect}]}` |
| `ui.highlight` | target, `ms?: 1500` | `{rect}`: the element is outlined in the live window |
| `devtools.toggle` | `{open?}` | the state below |
| `devtools.state` | — | `{open, generation, elements, pending: {input_frames, waits, screenshots}, repaints_while_open}` |

The text outline of `ui.snapshot`, one element per line, two spaces of indent per level:

```text
- button "New Note" [ref=e5] [id=notes.add]
- list [ref=e8] [id=notes.list]
  - listitem "Groceries" [ref=e9] [id=notes.list.row.0] [selected]
- textbox "Title" [ref=e14] [id=notes.title] [value="Groceries"] [focused]
```

Flags: `[disabled] [focused] [selected] [checked]│[unchecked] [offscreen]`; names and values are
cut at 80 characters. `interactive` lists only visible controls; `compact` leaves out unnamed
non-interactive and off-screen elements.

Waits are checked on frames the app runs anyway plus one wake at the deadline: a pending wait
never makes the app repaint in a loop. `idle` means a frame asked for no repaint within 200 ms, no
injected input is pending and the app reports no work of its own (`devtools::set_busy`; the
runtime reports its background jobs). Screenshot `annotations[].rect` is in image pixels relative
to the returned image; annotate and highlight are painted into the captured frame. Pictures travel
inline, never through a predictable temp file.

### The inspector

F12 (or `devtools.toggle`) opens an inspector window in the app: Pick (hover outlines an element
with its role, name, ref, id and rect; a click selects it and copies `@eN`), the element tree,
frame and wake counters with a live repaint counter, and panels the app provides
(`devtools::section`: the runtime adds its recent logs and the undo history). The inspector is not
part of snapshots. Closed, it costs nothing: no frames, no allocations.

## MCP tools

`ashlar-mcp` exposes the same vocabulary as tools:

| Tool | Method |
|---|---|
| `list_commands {query?}` | `engine.commands` (compact: id, label, enabled, mutates) |
| `describe_command {command}` | `engine.commands`, one entry in full, with its parameter schema |
| `execute {command, params?}` | `engine.execute` |
| `batch {commands, keepGoing?}` | `engine.batch` |
| `screenshot {id?}` | `ui.screenshot`, returned as an MCP image |
| `ui_inspect`, `ui_click`, `ui_drag`, `ui_scroll`, `ui_key`, `ui_type`, `ui_menu_list`, `ui_menu_invoke`, `ui_set` | the `ui.*` method of the same name |

The `ui_*` tools and `screenshot` are listed only when the backend's `app.version` reports the
method. With `ToolMode::PerCommand` every command is also its own tool (dots become `_`) with its
real input schema.

## CLI reply contract

`<app>-cli exec <id> [json]`, `describe`, `commands --json`, `run`, `version --json` print one line:
`{"ok":true,"result":…}` or `{"ok":false,"error":"…"}` (plus `detail`). Exit codes: 0 ok, 1 the
command failed, 2 usage error.
