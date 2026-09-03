# ashlar-command

Everything is a command. An app declares its commands once in a `Registry`; menus, shortcuts, the
command palette, the CLI, the control port and MCP all run them through one chokepoint,
`Registry::execute`, and discover them through `Dispatch::commands`. Headless, builds for wasm32.

## Declaring commands

```rust
use ashlar_command::{always, cmd, query, CommandError, CommandSpec, Host, Journal, Param, Registry};

fn build() -> Vec<CommandSpec<Doc>> {
    vec![
        // legacy free-text params doc: accepted, unknown keys refused
        cmd!("file.new", "New…", ["File"], Some("Mod+N"), r#"{"name":str}"#, always, |d, p| { … }),
        // typed params: validation + JSON Schema
        cmd!("image.resize", "Resize…", ["Image"], None,
             [Param::int_in("width", 1, 16384).required(), Param::one_of("fit", &["contain", "cover"])],
             has_image, |d, p| { … }),
        query!("image.info", "Info", "{}", |d, _| { … }),
        // or without macros (const-friendly)
        CommandSpec::new("view.fit", "Fit", fit).menu(&["View"]).shortcut("Mod+0").query(),
    ]
}
static REGISTRY: LazyLock<Registry<Doc>> = LazyLock::new(|| Registry::new(build()));
```

`cmd!`/`query!` take the same arguments as existing `cmd!`/`query!` command tables, so those port
by search and replace. A command body returns the host's own error type (`Host::Error`, anything
`Into<CommandError>`; leave the id empty and the chokepoint fills it in), so bodies written against
an app's `Result` alias port unchanged.

## The chokepoint

`Registry::execute(&self, state: &mut S, id, params)` runs, in order:

1. lookup (hash index; `Unknown`)
2. nesting limit (`MAX_DEPTH` = 64)
3. `Host::before`: may rewrite params (resolve names to ids, settle an ongoing gesture)
4. parameter validation (`BadParams`, every problem in one message): the registry's policy
   (`Registry::with_validation(Validation::Strict | TypedOnly | Off)`, default `Strict`), then the
   `Host::validate` hook (default: nothing). With `Off`, a disabled command reports `Disabled`
   whatever its params, and the host validates where it wants (the hook, or `Registry::validate`)
5. `Host::enabled_for` (default: the spec's predicate; override to check targets named in params)
6. the command under `catch_unwind` (a panic becomes `Panicked`, the app keeps running)
7. `Host::after` (every run, ok or not, at every depth: sync persistence, log errors at depth 0…)
8. journal: mutating commands that succeeded, params passed through `Host::scrub` first

A call refused in steps 1–5 goes to `Host::rejected` instead of `after`.

`execute` converts a body's error into `CommandError`. A host whose callers match on its own error
type uses `Registry::execute_host` (needs `Host::Error: Clone`): `Err(ExecError::Host(e))` is the
body's error unchanged, `Err(ExecError::Command(e))` the chokepoint's own refusal or a panic;
`ExecError::into_command(id)` gives what `execute` would have returned.

**Where the execution state lives.** The registry is immutable and usually a `static`. The mutable
part (current nesting depth and the journal) is a `Journal` the host owns and hands out through
`Host::journal()`. One registry therefore serves any number of documents, and commands can run other
commands (`REGISTRY.execute(state, …)` from inside a body) with the depth tracked per state. Every
hook gets `depth` (0 = called from outside), so a host can log or sync once per top-level call.

The journal is a capped ring (10,000 entries, the oldest 1,000 dropped at once). By default only
top-level calls are recorded; `Journal::new().record_nested(true)` records nested calls too.
`len`, `entries`, `since`, `truncate`, `retain` and `pop` let the host keep it in step with its
undo history. The chokepoint manages `depth()` itself; a host calls `Journal::set_depth` only to
run a command from one of its hooks as part of the current call (save the depth, set it one
higher, execute, restore).

## Parameters

- `Params::Static(&[Param])` / `Params::Typed(Vec<ParamSpec>)`: unknown keys, missing required keys,
  types, integer/float ranges and enum values are checked; `params_schema` gives a JSON Schema
  (`CommandInfo.params_schema`, cached per registry).
- `Params::Text("{…}")`: legacy docs (`{"name":str,"fps":f64=23.976,"mix":"A|B"?}`,
  `{layers: [id|name], add?}`). Keys the doc does not mention are refused; the schema lists the
  documented keys with types where the doc states them plainly (`str`, `u32`, `"A|B"`, `[…]`).
- `Registry::with_common_params(&["sequence"])`: keys every command accepts;
  `with_param_aliases(&[("layers", "layer")])`: alternative spellings for legacy docs.

## Discovery

`registry.bind(&mut state)` is a `Bound` implementing `Dispatch`: `commands()` returns
`CommandInfo` with `enabled` + `disabled_reason`, shortcuts in portable form and schemas;
`execute()` goes through the chokepoint.

## Shortcuts (`shortcut`, `keymap`)

- Portable syntax `Mod+Shift+S` (`Mod` = ⌘ on macOS, Ctrl elsewhere; `Ctrl` = the literal Control
  key). Accepts `Cmd+Z`, `CmdOrCtrl+Z`, `Option+X`, glyphs `⌘⇧S`, alternatives `Mod+Y / Mod+Shift+Z`.
- `Chord {mods, key}` is UI-independent; the UI layer maps its key events onto it.
- `chord.display(Platform::Mac)` → `⇧⌘S`; `Platform::Other` → `Ctrl+Shift+S`.
- `Keymap`: defaults from the registry plus user overrides (serde `Overrides`, read leniently),
  `lookup(chord)`, `keys_for(id)`, `conflicts()`, `would_conflict(id, chord)`. Off macOS `Mod` and
  `Ctrl` are one key, so `Mod+K` and `Ctrl+K` conflict there and not on macOS.

## Palette matching

`fuzzy_match(query, candidate) -> Option<FuzzyMatch {score, positions}>` (char positions for
highlighting; word starts, camelCase and runs score higher) and `fuzzy_rank`. Bounded: queries over
64 chars never match, candidates are cut at 512 chars.
