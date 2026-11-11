# Testing apps

An ashlar app can be tested at three levels: commands on a headless session, the whole UI on a
virtual clock with no window, and the real binary from the command line. The full reference is
[Testing and debugging UIs](../docs/debugging-uis.md).

## The binary: `--headless`

Every binary whose `main` calls `App::run` takes `--headless` (the `headless` feature, on by
default). It runs the app with no window, on a virtual clock, in a fresh temporary folder (never
your real settings or documents), and writes real pixels when there is a GPU:

```sh
notes --headless --size 1024x700 --scale 2 --theme dark --scene two-notes --script add.ui --at 1.5 \
      --snap out.png --tree tree.json
```

- `--size` is in points (default 1280x800); the PNG is that times `--scale`.
- `--script` runs a `.ui` script first; `--at` lets that many seconds pass before the picture.
- `--tree` writes the UI outline (`ui.snapshot`) as JSON.
- Exit codes: 0 done, 1 a step failed, 2 bad arguments.

## `.ui` scripts

One command per line, with assertions:

```text
exec note.add '{"title": "Groceries"}'
sleep 500
fill id=notes.title "Groceries and milk"
press Mod+S
wait --text "Saved"
assert exists id=notes.add
assert text id=status "Saved"
assert count role=listitem >= 1
assert screenshot notes-two
```

Elements are found by automation id (`id=…`), text (`text=…`), role, or a ref from the last
snapshot (`@e3`). The same scripts run in process, with `--headless`, or against a running app with
`ashlar-debug batch file.ui`.

## In-process tests: `Driver`

`App::runtime()` builds the app without a window and `Driver` runs its frames on a virtual clock.
Point the app folders at a temporary directory so tests never touch real settings:

```rust,ignore
use ashlar::app::Driver;
use ashlar::fs::AppDirs;
use serde_json::json;

#[test]
fn adding_a_note_shows_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut d = Driver::new(notes::app().dirs(AppDirs::under(dir.path())).runtime());
    d.settle(60);
    d.call("engine.execute", json!({"command": "note.add", "params": {"title": "Groceries"}})).unwrap();
    d.settle(60);
    let tree = d.call("ui.snapshot", json!({})).unwrap();
    assert!(tree["tree"].as_str().unwrap().contains("Groceries"));
}
```

| `Driver` call | What it does |
| --- | --- |
| `frame()` | Run one frame |
| `settle(max)` | Run frames until nothing asks for one (at most `max`) |
| `advance(secs)` | Let virtual time pass, running every frame that is asked for |
| `call(method, params)` | Any control method: `engine.execute`, `ui.click`, `ui.snapshot`… |
| `run_ui(script)` | Run a `.ui` script; `sleep` and `wait` use virtual time |
| `scene(name)` | Apply a named scene |
| `assert_snapshot(name)` | Compare a screenshot with `tests/snapshots/<name>.png` |
| `push_event(e)`, `drop_files(paths)` | Inject egui input or dropped files |
| `set_screen(w, h)`, `pixels_per_point(s)` | Window size and scale |
| `replay(path)` | Play a recorded session |
| `record(f)` | Record motion while `f` runs, for the motion gate |

For headless tests of commands alone, `App::headless()` gives a `Headless` session with the
registry and no UI.

## Scenes

A scene is a named starting state, declared on the app:

```rust,ignore
App::new("notes", env!("CARGO_PKG_VERSION"))
    .scene("empty", |_: &mut Session<Notes>| {})
    .scene("two-notes", |s: &mut Session<Notes>| {
        s.edit("Two notes", |d| { /* fill the document */ });
    })
```

`notes --list-scenes` prints them, `--scene two-notes` starts in one (with or without a window),
`Driver::scene("two-notes")` applies one in a test, and agents use `ui.scene` or the MCP tool
`ui_scene`.

## Golden snapshots

For real pixels, build the driver with the wgpu renderer:

```rust,ignore
use ashlar::app::{Driver, Renderer};

let rt = notes::app().dirs(AppDirs::under(dir.path())).runtime();
let mut d = Driver::with_renderer(rt, Renderer::wgpu());
d.set_screen(640.0, 400.0);
d.pixels_per_point(1.0);
d.scene("two-notes").unwrap();
d.settle(60);
d.assert_snapshot("notes-two").unwrap();
```

The comparison is exact unless a tolerance is given. On a mismatch it writes `<name>-new.png` and
`<name>-diff.png`. `ASHLAR_UPDATE_SNAPSHOTS=1` records new baselines, with a `.meta.json` naming the
GPU adapter; a baseline from another adapter is skipped with a note instead of failing. The result
type (`SnapshotOutcome`) is in the `ashlar-debug` crate.

## Time: `app.clock`

`app.clock` freezes the time the app's frames see, so you can look at an animation at an exact
moment: `{"pause": true}`, `{"step_ms": 16}`, `{"resume": true}`. It is also a `.ui` script line
(`clock pause`, `clock step 16`), an `ashlar-debug clock` command and the MCP tool `ui_clock`.

## Record and replay

`ASHLAR_RECORD=session.jsonl` (or `Runtime::record_to(path)` in a test) records every frame's input
and every control request. `Driver::replay` plays it into a fresh driver, which turns a bug report
into a test:

```rust,ignore
let mut d = Driver::new(notes::app().runtime());
d.replay("session.jsonl")?;
```

## The motion gate

`Driver::record` records values animated through `ashlar::ui::motion` and the position of every
element per frame. `check` flags overshoot, reversals, pops, flicker and frames requested after
everything settled:

```rust,ignore
use ashlar::ui::motion_gate::MotionRules;

let rec = d.record(|d| drop(d.run_ui("click overlays.open\nwait 1")));
let report = rec.check(&MotionRules::default());
assert!(report.is_clean(), "{report}");
```

The notes example has a test for each of these in
[examples/notes/tests](https://github.com/Himasnhu-AT/ashlar/tree/main/examples/notes/tests).
