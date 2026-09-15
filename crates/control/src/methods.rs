//! The standard method set, answered from any [`Dispatch`], and the routing of `ui.*` methods to
//! the app's [`UiHandler`].

use std::panic::{AssertUnwindSafe, catch_unwind};

use ashlar_command::{CommandError, CommandInfo, Dispatch};
use serde_json::{Value, json};

use crate::protocol::{PROTOCOL_VERSION, Reply, Request};

/// Methods every app answers, headless or not (`auth` is handled by the transport).
pub const STANDARD_METHODS: &[&str] = &["app.ping", "app.version", "engine.commands", "engine.execute", "engine.batch"];

/// The canonical UI vocabulary. A [`UiHandler`] answers the ones it supports (and may add more
/// `ui.*` methods). See `docs/control-protocol.md` for the parameters.
pub const UI_METHODS: &[&str] =
    &["ui.inspect", "ui.click", "ui.drag", "ui.scroll", "ui.key", "ui.type", "ui.screenshot", "ui.menu.list", "ui.menu.invoke", "ui.set", "app.quit"];

/// The debugger vocabulary (element snapshots with refs, actions on targets, waits; see
/// `docs/control-protocol.md`). Not part of [`UI_METHODS`]: a [`UiHandler`] reports each one only
/// once it answers it, so clients never list a tool the app cannot serve.
pub const DEBUG_METHODS: &[&str] = &[
    "ui.snapshot",
    "ui.hover",
    "ui.focus",
    "ui.fill",
    "ui.press",
    "ui.scroll_into_view",
    "ui.get",
    "ui.wait",
    "ui.highlight",
    "devtools.toggle",
    "devtools.state",
];

/// Most steps one `engine.batch` may hold.
pub const MAX_BATCH: usize = 10_000;

/// Who is answering: reported by `app.version`.
#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
}

impl AppInfo {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self { name: name.into(), version: version.into() }
    }
}

/// The app's answer to everything outside [`STANDARD_METHODS`]: `ui.*`, `app.quit` and any
/// app-specific methods. Implemented by the UI runtime.
pub trait UiHandler {
    /// The methods this handler answers (reported by `app.version`, so clients such as the MCP
    /// server know whether screenshots and input work).
    fn methods(&self) -> Vec<String>;
    /// Answer `req` now, or keep it and answer on a later frame (input must be processed first, a
    /// screenshot needs the next paint). Unknown methods: [`Request::unknown`].
    fn handle(&mut self, req: Request);
}

/// No UI: a headless session. `ui.*` methods are refused with a hint.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoUi;

impl UiHandler for NoUi {
    fn methods(&self) -> Vec<String> {
        Vec::new()
    }
    fn handle(&mut self, req: Request) {
        if req.method().starts_with("ui.") || req.method() == "app.quit" {
            let m = req.method().to_string();
            req.fail(format!("`{m}` needs the app window; this is a headless session (connect to the running app instead)"));
        } else {
            req.unknown();
        }
    }
}

/// Answer one request on the app thread: standard methods from `dispatch`, everything else from
/// `ui` (which may keep the request for a later frame).
pub fn handle(req: Request, dispatch: &mut dyn Dispatch, ui: &mut dyn UiHandler, app: &AppInfo) {
    match standard(req.method(), req.params(), dispatch, ui, app) {
        Some(r) => req.reply(r),
        None => ui.handle(req),
    }
}

/// Answer one request synchronously (headless serving, tests). A request the UI handler keeps for
/// later is answered with an error.
pub fn answer(method: &str, params: Value, dispatch: &mut dyn Dispatch, ui: &mut dyn UiHandler, app: &AppInfo) -> Reply {
    let (req, rx) = Request::new(method, params);
    handle(req, dispatch, ui, app);
    rx.try_recv().unwrap_or_else(|_| Reply::err(format!("`{method}` cannot be answered synchronously here")))
}

fn standard(method: &str, p: &Value, dispatch: &mut dyn Dispatch, ui: &dyn UiHandler, app: &AppInfo) -> Option<Reply> {
    Some(match method {
        "app.ping" => Reply::ok(json!("pong")),
        "app.version" => {
            let mut methods: Vec<String> = STANDARD_METHODS.iter().map(|m| m.to_string()).collect();
            methods.extend(ui.methods());
            Reply::ok(json!({"name": app.name, "version": app.version, "protocol": PROTOCOL_VERSION, "methods": methods}))
        }
        "engine.commands" => {
            let q = p.get("query").and_then(Value::as_str).unwrap_or("").to_lowercase();
            let list: Vec<CommandInfo> = commands(dispatch).into_iter().filter(|c| matches_query(c, &q)).collect();
            Reply::ok(serde_json::to_value(list).unwrap_or_default())
        }
        "engine.execute" => match p.get("command").and_then(Value::as_str) {
            Some(id) => match execute(dispatch, id, params_of(p)) {
                Ok(v) => Reply::Ok(v),
                Err(e) => crate::explain::explain(&e, &commands(dispatch)),
            },
            None => Reply::err("engine.execute needs a string `command`"),
        },
        "engine.batch" => batch(dispatch, p),
        _ => return None,
    })
}

/// Whether a command matches a lower-case filter on its id, label or menu path.
pub fn matches_query(c: &CommandInfo, q: &str) -> bool {
    q.is_empty() || c.id.to_lowercase().contains(q) || c.label.to_lowercase().contains(q) || c.menu.iter().any(|m| m.to_lowercase().contains(q))
}

/// `params` of a request, `{}` when absent or null.
fn params_of(p: &Value) -> Value {
    match p.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(v) => v.clone(),
    }
}

/// `dispatch.commands()`; a panicking implementation lists nothing.
fn commands(dispatch: &mut dyn Dispatch) -> Vec<CommandInfo> {
    catch_unwind(AssertUnwindSafe(|| dispatch.commands())).unwrap_or_default()
}

/// `dispatch.execute()`; a panic becomes [`CommandError::Panicked`].
fn execute(dispatch: &mut dyn Dispatch, id: &str, params: Value) -> Result<Value, CommandError> {
    catch_unwind(AssertUnwindSafe(|| dispatch.execute(id, params))).unwrap_or_else(|_| Err(CommandError::Panicked { id: id.to_string() }))
}

/// `engine.batch {commands: [{command, params?}], keepGoing?}`: runs in order, stops at the first
/// error unless `keepGoing`. All ok: the step results. Otherwise an error whose `detail.steps`
/// holds every step that ran.
fn batch(dispatch: &mut dyn Dispatch, p: &Value) -> Reply {
    let Some(steps) = p.get("commands").and_then(Value::as_array) else {
        return Reply::err("engine.batch needs a `commands` array of {command, params}");
    };
    if steps.len() > MAX_BATCH {
        return Reply::err(format!("engine.batch takes at most {MAX_BATCH} commands, got {}", steps.len()));
    }
    let keep_going = p.get("keepGoing").and_then(Value::as_bool).unwrap_or(false);
    let mut out = Vec::with_capacity(steps.len());
    let mut first_failure: Option<String> = None;
    let mut failed = 0usize;
    for (i, step) in steps.iter().enumerate() {
        let Some(id) = step.get("command").and_then(Value::as_str) else {
            let e = format!("step {} has no string `command`", i.saturating_add(1));
            out.push(json!({"ok": false, "error": e}));
            first_failure.get_or_insert(e);
            failed = failed.saturating_add(1);
            if keep_going {
                continue;
            }
            break;
        };
        match execute(dispatch, id, params_of(step)) {
            Ok(v) => out.push(json!({"command": id, "ok": true, "result": v})),
            Err(e) => {
                let (msg, detail) = match crate::explain::explain(&e, &commands(dispatch)) {
                    Reply::Err { error, detail } => (error, detail.unwrap_or(Value::Null)),
                    Reply::Ok(_) => (e.to_string(), serde_json::to_value(&e).unwrap_or(Value::Null)),
                };
                out.push(json!({"command": id, "ok": false, "error": msg, "detail": detail}));
                first_failure.get_or_insert(format!("step {} (`{id}`) failed: {msg}", i.saturating_add(1)));
                failed = failed.saturating_add(1);
                if !keep_going {
                    break;
                }
            }
        }
    }
    match first_failure {
        None => Reply::ok(Value::Array(out)),
        Some(first) if keep_going => Reply::err_with(format!("{failed} of {} steps failed; first: {first}", steps.len()), json!({"steps": out})),
        Some(first) => Reply::err_with(first, json!({"steps": out})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A counter with one mutating command, one failing, one panicking.
    #[derive(Default)]
    struct Counter(i64);

    impl Dispatch for Counter {
        fn commands(&self) -> Vec<CommandInfo> {
            ["count.add", "count.fail", "count.boom"]
                .iter()
                .map(|id| CommandInfo {
                    id: id.to_string(),
                    label: id.to_string(),
                    menu: vec!["Edit".into()],
                    shortcut: None,
                    params_schema: Value::Null,
                    params_text: None,
                    enabled: true,
                    disabled_reason: None,
                    mutates: true,
                })
                .collect()
        }
        fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
            match id {
                "count.add" => {
                    self.0 += params.get("by").and_then(Value::as_i64).unwrap_or(1);
                    Ok(json!(self.0))
                }
                "count.fail" => Err(CommandError::Failed { id: id.into(), msg: "no".into() }),
                "count.boom" => panic!("boom"),
                _ => Err(CommandError::Unknown { id: id.into() }),
            }
        }
    }

    fn call(d: &mut Counter, m: &str, p: Value) -> Reply {
        answer(m, p, d, &mut NoUi, &AppInfo::new("test", "1.2.3"))
    }

    #[test]
    fn standard_methods() {
        let mut d = Counter::default();
        assert_eq!(call(&mut d, "app.ping", json!({})), Reply::ok(json!("pong")));
        let Reply::Ok(v) = call(&mut d, "app.version", json!({})) else { panic!() };
        assert_eq!((v["name"].clone(), v["version"].clone(), v["protocol"].clone()), (json!("test"), json!("1.2.3"), json!(1)));
        assert_eq!(v["methods"].as_array().unwrap().len(), STANDARD_METHODS.len());
        let Reply::Ok(v) = call(&mut d, "engine.commands", json!({"query": "FAIL"})) else { panic!() };
        assert_eq!(v.as_array().unwrap().len(), 1);
        assert_eq!(call(&mut d, "engine.execute", json!({"command": "count.add", "params": {"by": 5}})), Reply::ok(json!(5)));
        assert_eq!(call(&mut d, "engine.execute", json!({"command": "count.add"})), Reply::ok(json!(6)));
        let Reply::Err { error, detail } = call(&mut d, "engine.execute", json!({"command": "nope"})) else { panic!() };
        assert_eq!(detail, Some(json!({"kind": "unknown", "id": "nope", "suggestions": []})));
        assert!(error.starts_with("unknown command `nope`;"), "{error}");
        let Reply::Err { error, detail } = call(&mut d, "engine.execute", json!({"command": "count.ad"})) else { panic!() };
        assert!(error.contains("did you mean `count.add`"), "{error}");
        assert_eq!(detail.unwrap()["suggestions"][0]["id"], "count.add");
        assert!(!call(&mut d, "engine.execute", json!({"command": 3})).is_ok());
    }

    #[test]
    fn a_panicking_command_is_an_error() {
        let mut d = Counter::default();
        let Reply::Err { detail, .. } = call(&mut d, "engine.execute", json!({"command": "count.boom"})) else { panic!() };
        assert_eq!(detail, Some(json!({"kind": "panicked", "id": "count.boom"})));
    }

    #[test]
    fn batches_stop_at_the_first_error_unless_keep_going() {
        let mut d = Counter::default();
        let steps = json!([{"command": "count.add"}, {"command": "count.fail"}, {"command": "count.add"}]);
        let Reply::Err { error, detail } = call(&mut d, "engine.batch", json!({"commands": steps})) else { panic!() };
        assert!(error.starts_with("step 2 (`count.fail`) failed"), "{error}");
        assert_eq!(detail.unwrap()["steps"].as_array().unwrap().len(), 2);
        assert_eq!(d.0, 1);
        let Reply::Err { error, detail } = call(&mut d, "engine.batch", json!({"commands": steps, "keepGoing": true})) else { panic!() };
        assert!(error.starts_with("1 of 3 steps failed"), "{error}");
        assert_eq!(detail.unwrap()["steps"].as_array().unwrap().len(), 3);
        assert_eq!(d.0, 3);
        assert_eq!(
            call(&mut d, "engine.batch", json!({"commands": [{"command": "count.add"}]})),
            Reply::ok(json!([{"command": "count.add", "ok": true, "result": 4}]))
        );
        assert!(!call(&mut d, "engine.batch", json!({})).is_ok());
        assert!(!call(&mut d, "engine.batch", json!({"commands": [5]})).is_ok());
        let huge: Vec<Value> = (0..=MAX_BATCH).map(|_| json!({"command": "count.add"})).collect();
        assert!(!call(&mut d, "engine.batch", json!({"commands": huge})).is_ok());
        assert_eq!(d.0, 4, "an oversize batch runs nothing");
    }

    #[test]
    fn ui_methods_go_to_the_handler() {
        let mut d = Counter::default();
        let Reply::Err { error, .. } = call(&mut d, "ui.click", json!({})) else { panic!() };
        assert!(error.contains("needs the app window"), "{error}");
        let Reply::Err { error, .. } = call(&mut d, "frobnicate", json!({})) else { panic!() };
        assert!(error.contains("unknown method"), "{error}");

        struct Later(Vec<Request>);
        impl UiHandler for Later {
            fn methods(&self) -> Vec<String> {
                vec!["ui.screenshot".into()]
            }
            fn handle(&mut self, req: Request) {
                self.0.push(req);
            }
        }
        let mut ui = Later(Vec::new());
        let app = AppInfo::new("t", "1");
        let (req, rx) = Request::new("ui.screenshot", json!({}));
        handle(req, &mut d, &mut ui, &app);
        assert!(rx.try_recv().is_err(), "kept for a later frame");
        ui.0.pop().unwrap().ok(json!({"png": ""}));
        assert!(rx.try_recv().unwrap().is_ok());
        let Reply::Ok(v) = answer("app.version", json!({}), &mut d, &mut ui, &app) else { panic!() };
        assert!(v["methods"].as_array().unwrap().contains(&json!("ui.screenshot")));
        // A kept request cannot be answered synchronously.
        assert!(!answer("ui.screenshot", json!({}), &mut d, &mut ui, &app).is_ok());
    }
}
