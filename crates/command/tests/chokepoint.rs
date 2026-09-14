#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The chokepoint modelled on a video editor's session: names resolved to ids before running,
//! enablement re-checked against targets named in params, gesture settling before top-level edits,
//! persistence sync after every run, warnings/errors logged once per top-level call, secrets
//! scrubbed from the journal. The command table is a port of that editor's `cmd!`/`query!` table.

use std::sync::LazyLock;

use ashlar_command::{CommandError, CommandSpec, Dispatch, ExecError, Host, Journal, MAX_DEPTH, Param, Registry, Validation, always, cmd, query};
use serde_json::{Value, json};

// ---------- the app's own error and result, as before the port ----------

#[derive(Debug, Clone, PartialEq)]
enum EngineError {
    NoSequence,
    Bad(String),
    Other(String),
}

impl From<EngineError> for CommandError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::NoSequence => CommandError::failed("", "no sequence is open"),
            EngineError::Bad(m) => CommandError::bad_params("", m),
            EngineError::Other(m) => CommandError::failed("", m),
        }
    }
}

type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Clone, PartialEq)]
struct Clip {
    id: u64,
    name: String,
}

#[derive(Default)]
struct Session {
    journal: Journal,
    clips: Vec<Clip>,
    sequence_open: bool,
    selection: Vec<u64>,
    clipboard: Vec<Clip>,
    undo: Vec<Vec<Clip>>,
    redo: Vec<Vec<Clip>>,
    api_key: Option<String>,
    project_name: String,
    log: Vec<(&'static str, String, String)>,
    syncs: usize,
    graphics_synced: usize,
    trim_active: bool,
    trim_commits: Vec<String>,
    /// validate params in the `Host::validate` hook, top-level calls only
    validate_top_level: bool,
}

fn session() -> Session {
    Session {
        journal: Journal::new().record_nested(true),
        clips: vec![Clip { id: 1, name: "hero".into() }, Clip { id: 2, name: "b-roll".into() }],
        sequence_open: true,
        ..Default::default()
    }
}

// ---------- helpers the table uses ----------

fn str_p<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}

fn named_clips(p: &Value) -> Option<Vec<u64>> {
    if let Some(a) = p.get("clips").and_then(Value::as_array) {
        return Some(a.iter().filter_map(Value::as_u64).collect());
    }
    p.get("clip").and_then(Value::as_u64).map(|c| vec![c])
}

fn edit(s: &mut Session, f: impl FnOnce(&mut Vec<Clip>) -> Result<()>) -> Result<()> {
    let mut clips = s.clips.clone();
    f(&mut clips)?;
    s.undo.push(std::mem::replace(&mut s.clips, clips));
    s.redo.clear();
    Ok(())
}

fn has_seq(s: &Session) -> std::result::Result<(), String> {
    if s.sequence_open { Ok(()) } else { Err("no sequence is open".into()) }
}
fn has_selection(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.selection.is_empty() { Err("select a clip first".into()) } else { Ok(()) }
}
fn has_clipboard(s: &Session) -> std::result::Result<(), String> {
    if s.clipboard.is_empty() { Err("the clipboard is empty".into()) } else { Ok(()) }
}
fn can_undo(s: &Session) -> std::result::Result<(), String> {
    if s.undo.is_empty() { Err("nothing to undo".into()) } else { Ok(()) }
}
fn can_redo(s: &Session) -> std::result::Result<(), String> {
    if s.redo.is_empty() { Err("nothing to redo".into()) } else { Ok(()) }
}

fn clear(s: &mut Session, p: &Value) -> Result<Value> {
    let targets = named_clips(p).unwrap_or_else(|| s.selection.clone());
    edit(s, |clips| {
        clips.retain(|c| !targets.contains(&c.id));
        Ok(())
    })?;
    s.selection.clear();
    Ok(json!({"removed": targets.len()}))
}

// ---------- the table: ported by search-and-replace, bodies unchanged ----------

fn build() -> Vec<CommandSpec<Session>> {
    vec![
        cmd!("file.newProject", "Project…", ["File", "New"], Some("Cmd+Alt+N"), r#"{"name":str}"#, always, |s, p| {
            s.project_name = str_p(p, "name").unwrap_or("Untitled").to_string();
            s.clips.clear();
            s.undo.clear();
            Ok(Value::Null)
        }),
        cmd!("edit.undo", "Undo", ["Edit"], Some("Cmd+Z"), "{}", can_undo, |s, _| {
            let prev = s.undo.pop().ok_or_else(|| EngineError::Other("nothing to undo".into()))?;
            s.redo.push(std::mem::replace(&mut s.clips, prev));
            Ok(json!({"undone": true}))
        }),
        cmd!("edit.redo", "Redo", ["Edit"], Some("Cmd+Shift+Z / Cmd+Y"), "{}", can_redo, |s, _| {
            let next = s.redo.pop().ok_or_else(|| EngineError::Other("nothing to redo".into()))?;
            s.undo.push(std::mem::replace(&mut s.clips, next));
            Ok(json!({"redone": true}))
        }),
        cmd!("edit.copy", "Copy", ["Edit"], Some("Cmd+C"), "{}", has_selection, |s, _| {
            s.clipboard = s.clips.iter().filter(|c| s.selection.contains(&c.id)).cloned().collect();
            Ok(json!({"copied": s.clipboard.len()}))
        }),
        cmd!("edit.paste", "Paste", ["Edit"], Some("Cmd+V"), "{}", has_clipboard, |s, _| {
            let add = s.clipboard.clone();
            let next = s.clips.iter().map(|c| c.id).max().unwrap_or(0) + 1;
            edit(s, |clips| {
                clips.extend(add.into_iter().enumerate().map(|(i, c)| Clip { id: next + i as u64, name: c.name }));
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("edit.clear", "Clear", ["Edit"], Some("Backspace"), r#"{"clips":[id]?}"#, has_selection, clear),
        cmd!("edit.rippleDelete", "Ripple Delete", ["Edit"], Some("Shift+Delete"), r#"{"clips":[id]?}"#, has_selection, clear),
        cmd!("edit.selectAll", "Select All", ["Edit"], Some("Cmd+A"), "{}", has_seq, |s, _| {
            s.selection = s.clips.iter().map(|c| c.id).collect();
            Ok(json!({"selected": s.selection.len()}))
        }),
        cmd!("edit.cut", "Cut", ["Edit"], Some("Cmd+X"), r#"{"clips":[id]?}"#, has_selection, |s, _| {
            // a command made of other commands: both go through the chokepoint again
            REGISTRY.execute(s, "edit.copy", json!({})).map_err(|e| EngineError::Other(e.to_string()))?;
            REGISTRY.execute(s, "edit.clear", json!({})).map_err(|e| EngineError::Other(e.to_string()))
        }),
        cmd!("clip.rename", "Rename…", ["Clip"], None, r#"{"clip":id,"name":str}"#, has_selection, |s, p| {
            let id = p.get("clip").and_then(Value::as_u64).ok_or_else(|| EngineError::Bad("`clip` is required".into()))?;
            let name = str_p(p, "name").ok_or_else(|| EngineError::Bad("`name` is required".into()))?.to_string();
            edit(s, |clips| {
                let c = clips.iter_mut().find(|c| c.id == id).ok_or_else(|| EngineError::Other(format!("no clip {id}")))?;
                c.name = name;
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        cmd!("clip.speed", "Speed…", ["Clip"], None, [Param::int_in("percent", 1, 1000).required()], has_selection, |_, _| Ok(Value::Null)),
        cmd!("graphics.bringToFront", "Bring to Front", ["Graphics", "Arrange"], Some("Cmd+Shift+]"), "{}", has_selection, |_, _| Ok(Value::Null)),
        cmd!("prefs.setApiKey", "Set API Key", [], None, r#"{"key":str}"#, always, |s, p| {
            s.api_key = str_p(p, "key").map(str::to_string);
            Ok(Value::Null)
        }),
        cmd!("sequence.close", "Close Sequence", ["Sequence"], Some("Ctrl+W"), "{}", has_seq, |s, _| {
            s.sequence_open = false;
            Ok(Value::Null)
        }),
        cmd!("sequence.nudge", "Nudge", [], None, "{}", always, |s, _| if s.sequence_open { Ok(Value::Null) } else { Err(EngineError::NoSequence) }),
        query!("clip.list", "List Clips", "{}", |s, _| Ok(json!(s.clips.iter().map(|c| json!({"id": c.id, "name": c.name})).collect::<Vec<_>>()))),
        query!("prefs.get", "Get Preferences", r#"{"key":str?}"#, |s, p| match str_p(p, "key") {
            Some("projectName") => Ok(json!(s.project_name)),
            Some(k) => Err(EngineError::Bad(format!("unknown preference `{k}`"))),
            None => Ok(json!({"projectName": s.project_name})),
        }),
        cmd!("debug.panic", "Panic", [], None, "{}", always, |s, _| {
            s.clips.clear();
            let v: Vec<u8> = vec![];
            let i = v.len() + 3;
            Ok(json!(v[i]))
        }),
        cmd!("debug.recurse", "Recurse", [], None, "{}", always, |s, _| {
            REGISTRY.execute(s, "debug.recurse", json!({})).map_err(|e| EngineError::Other(e.to_string()))
        }),
    ]
}

static REGISTRY: LazyLock<Registry<Session>> = LazyLock::new(|| Registry::new(build()).with_common_params(&["sequence"]));
/// The same table the way the editor runs it: no validation at the chokepoint.
static UNCHECKED: LazyLock<Registry<Session>> =
    LazyLock::new(|| Registry::new(build()).with_common_params(&["sequence"]).with_validation(Validation::Off));
static TYPED_ONLY: LazyLock<Registry<Session>> =
    LazyLock::new(|| Registry::new(build()).with_common_params(&["sequence"]).with_validation(Validation::TypedOnly));

impl Host for Session {
    type Error = EngineError;

    fn journal(&mut self) -> &mut Journal {
        &mut self.journal
    }

    fn before(&mut self, spec: &CommandSpec<Self>, params: &mut Value, depth: usize) -> Result<()> {
        // another edit arrives while a trim gesture plays: commit it first (one undo step)
        if depth == 0 && spec.mutates && self.trim_active && !spec.id.starts_with("trim.") {
            self.trim_active = false;
            self.trim_commits.push(spec.id.to_string());
        }
        // a clip given by its name instead of its id becomes the id here
        if let Some(name) = params.get("clip").and_then(Value::as_str) {
            let id = self.clips.iter().find(|c| c.name == name).map(|c| c.id).ok_or_else(|| EngineError::Bad(format!("no clip named `{name}`")))?;
            params["clip"] = json!(id);
        }
        Ok(())
    }

    fn validate(&mut self, spec: &CommandSpec<Self>, params: &Value, depth: usize) -> std::result::Result<(), CommandError> {
        if self.validate_top_level && depth == 0 { REGISTRY.validate(spec, params) } else { Ok(()) }
    }

    fn enabled_for(&mut self, spec: &CommandSpec<Self>, params: &Value) -> std::result::Result<(), String> {
        let by_selection = (spec.enabled)(self);
        if by_selection.is_ok() {
            return by_selection;
        }
        let Some(mut targets) = named_clips(params) else { return by_selection };
        std::mem::swap(&mut self.selection, &mut targets);
        let by_params = (spec.enabled)(self);
        self.selection = targets;
        by_params
    }

    fn after(&mut self, spec: &CommandSpec<Self>, _params: &Value, result: &std::result::Result<Value, CommandError>, depth: usize) {
        if result.is_ok() && spec.mutates && spec.id.starts_with("graphics.") {
            self.graphics_synced += 1;
        }
        if depth == 0
            && let Err(e) = result
        {
            self.log.push(("error", spec.id.to_string(), e.to_string()));
        }
        self.syncs += 1;
    }

    fn rejected(&mut self, id: &str, error: &CommandError, depth: usize) {
        if depth == 0 && matches!(error, CommandError::Disabled { .. }) {
            self.log.push(("warning", id.to_string(), error.to_string()));
        }
    }

    fn scrub(&self, id: &str, params: &mut Value) {
        if id == "prefs.setApiKey" && params.get("key").is_some() {
            params["key"] = json!("<redacted>");
        }
    }
}

fn run(s: &mut Session, id: &str, p: Value) -> std::result::Result<Value, CommandError> {
    REGISTRY.execute(s, id, p)
}

#[test]
fn table_is_well_formed() {
    assert!(REGISTRY.duplicates().is_empty());
    let km = ashlar_command::Keymap::from_registry(&REGISTRY, ashlar_command::shortcut::Platform::Mac);
    assert!(km.invalid_defaults().is_empty(), "{:?}", km.invalid_defaults());
    assert!(km.conflicts().is_empty(), "{:?}", km.conflicts());
}

#[test]
fn names_resolve_before_running_and_journal_sees_ids() {
    let mut s = session();
    let r = run(&mut s, "clip.rename", json!({"clip": "hero", "name": "Hero"}));
    assert_eq!(r, Ok(Value::Null));
    assert_eq!(s.clips[0].name, "Hero");
    let last = s.journal.last().unwrap();
    assert_eq!((last.id.as_str(), &last.params), ("clip.rename", &json!({"clip": 1, "name": "Hero"})));

    let r = run(&mut s, "clip.rename", json!({"clip": "nope", "name": "x"}));
    assert_eq!(r, Err(CommandError::bad_params("clip.rename", "no clip named `nope`")));
}

#[test]
fn targets_in_params_enable_a_command_the_selection_does_not() {
    let mut s = session();
    assert!(s.selection.is_empty());
    assert_eq!(run(&mut s, "edit.clear", json!({"clips": [2]})), Ok(json!({"removed": 1})));
    assert_eq!(s.clips.len(), 1);
    // without targets it is disabled, with the predicate's reason, and the warning is logged
    let e = run(&mut s, "edit.clear", json!({})).unwrap_err();
    assert_eq!(e, CommandError::Disabled { id: "edit.clear".into(), reason: "select a clip first".into() });
    assert_eq!(s.log.last().unwrap().0, "warning");
    // the selection is put back after the check
    assert!(s.selection.is_empty());
}

#[test]
fn after_runs_at_every_depth_logging_only_at_the_top() {
    let mut s = session();
    run(&mut s, "edit.selectAll", json!({})).unwrap();
    let syncs = s.syncs;
    assert_eq!(run(&mut s, "edit.cut", json!({})), Ok(json!({"removed": 2})));
    // cut + copy + clear: three runs, three syncs
    assert_eq!(s.syncs - syncs, 3);
    // nested calls are journaled too (this host asked for it), in completion order
    let ids: Vec<&str> = s.journal.entries().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["edit.selectAll", "edit.copy", "edit.clear", "edit.cut"]);

    // a nested call that is disabled is not logged by itself; the top-level failure is, once
    // (targets in params enable `cut`, but the nested `copy` reads the empty selection)
    s.log.clear();
    let e = run(&mut s, "edit.cut", json!({"clips": [1]})).unwrap_err();
    assert!(matches!(e, CommandError::Failed { .. }), "{e:?}");
    assert_eq!(s.log.len(), 1, "{:?}", s.log);
    assert_eq!(s.log[0].0, "error");
    assert_eq!(s.journal.depth(), 0);
}

#[test]
fn secrets_never_reach_the_journal() {
    let mut s = session();
    run(&mut s, "prefs.setApiKey", json!({"key": "sk-live-123"})).unwrap();
    assert_eq!(s.api_key.as_deref(), Some("sk-live-123"));
    assert_eq!(s.journal.last().unwrap().params, json!({"key": "<redacted>"}));
    assert!(!s.journal.to_json().to_string().contains("sk-live"));
}

#[test]
fn a_gesture_settles_before_the_next_top_level_edit_only() {
    let mut s = session();
    s.trim_active = true;
    run(&mut s, "clip.list", json!({})).unwrap();
    assert!(s.trim_active, "queries leave the gesture alone");
    run(&mut s, "edit.selectAll", json!({})).unwrap();
    assert_eq!(s.trim_commits, ["edit.selectAll"]);
}

#[test]
fn queries_are_not_journaled_and_host_errors_carry_the_id() {
    let mut s = session();
    assert_eq!(run(&mut s, "prefs.get", json!({"key": "projectName"})), Ok(json!("")));
    assert!(s.journal.is_empty());
    assert_eq!(run(&mut s, "prefs.get", json!({"key": "x"})), Err(CommandError::bad_params("prefs.get", "unknown preference `x`")));
    s.sequence_open = false;
    assert_eq!(run(&mut s, "sequence.nudge", Value::Null), Err(CommandError::failed("sequence.nudge", "no sequence is open")));
    assert!(s.journal.is_empty(), "failed commands are not journaled");
}

#[test]
fn unknown_keys_are_refused_before_running() {
    let mut s = session();
    let syncs = s.syncs;
    let e = run(&mut s, "file.newProject", json!({"name": "A", "nmae": "B"})).unwrap_err();
    assert_eq!(
        e,
        CommandError::bad_params("file.newProject", r#"unknown parameter(s) `nmae` (did you mean `name`?); accepted: name (doc: {"name":str})"#)
    );
    assert_eq!(s.syncs, syncs, "a refused call does not run `after`");
    // a key every command accepts
    assert!(run(&mut s, "file.newProject", json!({"name": "A", "sequence": 3})).is_ok());
    // a command documented as `{}` takes no keys
    assert!(matches!(run(&mut s, "edit.selectAll", json!({"all": true})), Err(CommandError::BadParams { .. })));
    assert_eq!(run(&mut s, "no.such", json!({})), Err(CommandError::Unknown { id: "no.such".into() }));
}

#[test]
fn a_panicking_command_becomes_an_error_and_the_session_survives() {
    let mut s = session();
    let e = run(&mut s, "debug.panic", json!({})).unwrap_err();
    assert_eq!(e, CommandError::Panicked { id: "debug.panic".into() });
    assert_eq!(s.journal.depth(), 0);
    assert_eq!(s.log.last().unwrap().1, "debug.panic");
    // still usable
    assert!(run(&mut s, "file.newProject", json!({"name": "after"})).is_ok());
    assert_eq!(s.project_name, "after");
}

#[test]
fn runaway_recursion_is_bounded() {
    let mut s = session();
    let e = run(&mut s, "debug.recurse", json!({})).unwrap_err();
    assert!(e.to_string().contains(&format!("nested more than {MAX_DEPTH} deep")), "{e}");
    assert_eq!(s.journal.depth(), 0);
    assert_eq!(s.log.len(), 1);
}

#[test]
fn hostile_params_never_panic() {
    let huge_key = "k".repeat(100_000);
    let mut deep = json!(1);
    for _ in 0..100 {
        deep = json!([deep]);
    }
    let junk = [
        Value::Null,
        json!(1),
        json!(-1.5e300),
        json!("x"),
        json!([]),
        json!({}),
        json!({"clips": "all"}),
        json!({"clips": [-1, 1e30, "a", null]}),
        json!({"clip": {"nested": true}}),
        json!({"clip": ""}),
        json!({"name": 7, "key": [1]}),
        json!({ huge_key.clone(): 1 }),
        deep,
    ];
    for spec in REGISTRY.specs().iter().filter(|c| !c.id.starts_with("debug.")) {
        for p in &junk {
            let mut s = session();
            s.selection = vec![1];
            s.clipboard = vec![Clip { id: 9, name: "x".into() }];
            s.undo = vec![vec![]];
            s.redo = vec![vec![]];
            let r = run(&mut s, spec.id, p.clone());
            assert!(!matches!(r, Err(CommandError::Panicked { .. })), "{} panicked on {p}", spec.id);
            if let Err(e) = r {
                assert!(e.to_string().len() < 2_000, "{} gave a huge error", spec.id);
            }
        }
    }
}

#[test]
fn dispatch_discovers_with_reasons_and_portable_shortcuts() {
    let mut s = session();
    let mut d = REGISTRY.bind(&mut s);
    let cmds = d.commands();
    let new = cmds.iter().find(|c| c.id == "file.newProject").unwrap();
    assert_eq!(new.shortcut.as_deref(), Some("Mod+Alt+N"));
    assert_eq!(new.menu, ["File", "New"]);
    assert_eq!(new.params_text.as_deref(), Some(r#"{"name":str}"#));
    assert_eq!(new.params_schema["required"], json!(["name"]));
    assert!(new.mutates && new.enabled);
    let redo = cmds.iter().find(|c| c.id == "edit.redo").unwrap();
    assert_eq!(redo.shortcut.as_deref(), Some("Mod+Shift+Z / Mod+Y"));
    assert_eq!((redo.enabled, redo.disabled_reason.as_deref()), (false, Some("nothing to redo")));
    let list = cmds.iter().find(|c| c.id == "clip.list").unwrap();
    assert!(!list.mutates);
    // and runs through the same chokepoint
    assert_eq!(d.execute("edit.selectAll", json!({})), Ok(json!({"selected": 2})));
    assert_eq!(s.journal.len(), 1);
}

#[test]
fn journal_is_a_capped_ring() {
    let mut j = Journal::with_cap(10, 3);
    for i in 0..25 {
        j.push("x", json!(i));
    }
    assert!(j.len() <= 10);
    assert_eq!(j.last().unwrap().seq, 24);
    assert_eq!(j.next_seq(), 25);
    let first = j.entries().next().unwrap().seq;
    assert_eq!(j.since(first + 2).count(), j.len() - 2);
    // defaults: 10k entries, 1k dropped at once
    let mut j = Journal::new();
    for i in 0..10_001 {
        j.push("x", json!(i));
    }
    assert_eq!(j.len(), 9_001);
    assert_eq!(j.entries().next().unwrap().params, json!(1000));
}

#[test]
fn top_level_only_journal_by_default() {
    let mut s = session();
    s.journal = Journal::new();
    run(&mut s, "edit.selectAll", json!({})).unwrap();
    run(&mut s, "edit.cut", json!({})).unwrap();
    let ids: Vec<&str> = s.journal.entries().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["edit.selectAll", "edit.cut"]);
}

#[test]
fn validation_off_reports_disabled_and_lets_undocumented_keys_through() {
    assert_eq!(UNCHECKED.validation(), Validation::Off);
    assert_eq!(REGISTRY.validation(), Validation::Strict);
    let mut s = session();
    s.sequence_open = false;
    // explicit targets on a command documented `{}`: strict refuses the key first…
    assert!(matches!(run(&mut s, "graphics.bringToFront", json!({"clips": [1]})), Err(CommandError::BadParams { .. })));
    // …the editor's chokepoint says why the command cannot run
    let e = UNCHECKED.execute(&mut s, "graphics.bringToFront", json!({"clips": [1]})).unwrap_err();
    assert_eq!(e, CommandError::Disabled { id: "graphics.bringToFront".into(), reason: "no sequence is open".into() });
    assert_eq!(s.log.last().unwrap().0, "warning");
    // undocumented keys from an agent run; typed params are not checked either
    assert!(UNCHECKED.execute(&mut s, "file.newProject", json!({"name": "A", "nmae": "B"})).is_ok());
    assert_eq!(s.project_name, "A");
    s.sequence_open = true;
    s.selection = vec![1];
    assert!(UNCHECKED.execute(&mut s, "clip.speed", json!({"percent": 0})).is_ok());
}

#[test]
fn typed_only_checks_typed_declarations_and_trusts_text_docs() {
    let mut s = session();
    s.selection = vec![1];
    let e = TYPED_ONLY.execute(&mut s, "clip.speed", json!({"percent": 0})).unwrap_err();
    assert!(matches!(&e, CommandError::BadParams { msg, .. } if msg.contains("at least 1")), "{e:?}");
    assert_eq!(TYPED_ONLY.execute(&mut s, "clip.speed", json!({"percent": 50})), Ok(Value::Null));
    assert!(TYPED_ONLY.execute(&mut s, "file.newProject", json!({"name": "A", "nmae": "B"})).is_ok());
    assert!(TYPED_ONLY.execute(&mut s, "edit.selectAll", json!({"all": true})).is_ok());
}

#[test]
fn the_host_validate_hook_checks_where_the_host_wants() {
    let mut s = session();
    s.validate_top_level = true;
    let syncs = s.syncs;
    // top level: refused by the hook, as `rejected`, with the command id filled in
    let e = UNCHECKED.execute(&mut s, "file.newProject", json!({"name": "A", "nmae": "B"})).unwrap_err();
    assert!(matches!(&e, CommandError::BadParams { id, .. } if id == "file.newProject"), "{e:?}");
    assert_eq!(s.syncs, syncs);
    // nested: not checked (cut passes its targets on unchanged)
    run(&mut s, "edit.selectAll", json!({})).unwrap();
    assert!(UNCHECKED.execute(&mut s, "edit.cut", json!({})).is_ok());
}

#[test]
fn execute_host_keeps_the_hosts_error_variant() {
    let mut s = session();
    s.sequence_open = false;
    let r = REGISTRY.execute_host(&mut s, "sequence.nudge", Value::Null);
    assert_eq!(r, Err(ExecError::Host(EngineError::NoSequence)));
    // `after` still saw it, converted and with its id, and logged it at the top level
    assert_eq!(s.log.last().unwrap(), &("error", "sequence.nudge".to_string(), "no sequence is open".to_string()));
    let r = REGISTRY.execute_host(&mut s, "prefs.get", json!({"key": "x"}));
    assert!(matches!(r, Err(ExecError::Host(EngineError::Bad(ref m))) if m == "unknown preference `x`"), "{r:?}");
    assert_eq!(r.unwrap_err().into_command("prefs.get"), CommandError::bad_params("prefs.get", "unknown preference `x`"));
    // the chokepoint's own refusals and failures are told apart
    let r = REGISTRY.execute_host(&mut s, "no.such", json!({}));
    assert_eq!(r, Err(ExecError::Command(CommandError::Unknown { id: "no.such".into() })));
    let r = REGISTRY.execute_host(&mut s, "sequence.close", json!({}));
    assert!(matches!(r, Err(ExecError::Command(CommandError::Disabled { .. }))), "{r:?}");
    // an error from `before` (a clip name nothing has) is the host's own too
    let r = REGISTRY.execute_host(&mut s, "clip.rename", json!({"clip": "nope", "name": "x"}));
    assert_eq!(r, Err(ExecError::Host(EngineError::Bad("no clip named `nope`".into()))));
    let r = REGISTRY.execute_host(&mut s, "file.newProject", json!({"nmae": 1}));
    assert!(r.as_ref().unwrap_err().command().is_some_and(|e| matches!(e, CommandError::BadParams { .. })), "{r:?}");
    // success: same value and journal entry as `execute`
    s.sequence_open = true;
    assert_eq!(REGISTRY.execute_host(&mut s, "edit.selectAll", json!({})), Ok(json!({"selected": 2})));
    assert_eq!(s.journal.last().unwrap().id, "edit.selectAll");
    let r = REGISTRY.execute_host(&mut s, "debug.panic", json!({}));
    assert_eq!(r, Err(ExecError::Command(CommandError::Panicked { id: "debug.panic".into() })));
    assert!(r.unwrap_err().host().is_none());
    assert_eq!(s.journal.depth(), 0);
}

#[test]
fn journal_truncate_retain_pop() {
    let mut s = session();
    for name in ["a", "b", "c", "d"] {
        run(&mut s, "file.newProject", json!({"name": name})).unwrap();
    }
    assert_eq!(s.journal.len(), 4);
    let seq = s.journal.next_seq();
    // undo two, then a new edit replaces them
    s.journal.truncate(2);
    let names: Vec<&Value> = s.journal.entries().map(|e| &e.params["name"]).collect();
    assert_eq!(names, [&json!("a"), &json!("b")]);
    run(&mut s, "file.newProject", json!({"name": "e"})).unwrap();
    assert_eq!(s.journal.last().unwrap().seq, seq, "sequence numbers are not reused");
    s.journal.truncate(99);
    assert_eq!(s.journal.len(), 3);
    s.journal.retain(|e| e.params["name"] != "b");
    assert_eq!(s.journal.entries().map(|e| e.params["name"].as_str().unwrap()).collect::<Vec<_>>(), ["a", "e"]);
    assert_eq!(s.journal.pop().unwrap().params["name"], "e");
    assert_eq!(s.journal.len(), 1);
}

#[test]
fn a_command_run_from_a_hook_counts_as_nested_when_the_host_sets_the_depth() {
    let mut s = session();
    s.journal = Journal::new(); // top-level calls only
    // what a host does inside `after` to run a follow-up command as part of the current call
    let saved = s.journal.depth();
    s.journal.set_depth(saved + 1);
    run(&mut s, "edit.selectAll", json!({})).unwrap();
    s.journal.set_depth(saved);
    assert!(s.journal.is_empty(), "nested calls are not journaled by this host");
    run(&mut s, "edit.selectAll", json!({})).unwrap();
    assert_eq!(s.journal.len(), 1);
}
