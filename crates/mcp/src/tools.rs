//! MCP tools: one fixed vocabulary over the control methods, `ui_*` passthrough when the backend
//! answers them, and optionally one tool per command.

use ashlar_command::CommandInfo;
use ashlar_control::Reply;
use serde_json::{Map, Value, json};

/// Which tools are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolMode {
    /// `list_commands`, `describe_command`, `execute`, `batch` (+ UI tools). Best for apps with many
    /// commands: the tool list stays small.
    #[default]
    Fixed,
    /// Also one tool per enabled command, with its real input schema.
    PerCommand,
}

/// The result of one tool call (`content` + `isError`).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub content: Vec<Value>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(t: impl Into<String>) -> Self {
        Self { content: vec![json!({"type": "text", "text": t.into()})], is_error: false }
    }

    pub fn json(v: &Value) -> Self {
        Self::text(v.to_string())
    }

    pub fn error(t: impl Into<String>) -> Self {
        Self { content: vec![json!({"type": "text", "text": t.into()})], is_error: true }
    }

    /// A control reply: the result as JSON text, or the error (with its detail) flagged.
    pub fn from_reply(r: Reply) -> Self {
        match r {
            Reply::Ok(v) => Self::json(&v),
            Reply::Err { error, detail: None } => Self::error(error),
            Reply::Err { error, detail: Some(d) } => Self::error(format!("{error}\n{d}")),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({"content": self.content, "isError": self.is_error})
    }
}

fn obj(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

fn tool(name: &str, title: &str, description: &str, schema: Value, read_only: bool) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": schema,
        "annotations": {"title": title, "readOnlyHint": read_only, "openWorldHint": false},
    })
}

/// The fixed tools every backend has.
pub fn core_tools() -> Vec<Value> {
    let step = json!({"type": "object", "properties": {"command": {"type": "string"}, "params": {"type": "object"}}, "required": ["command"]});
    vec![
        tool(
            "list_commands",
            "List commands",
            "Every command of the app: id, label, whether it is enabled now and whether it changes the document. Filter with `query` (matches id, label and menu). Use describe_command for parameters.",
            obj(json!({"query": {"type": "string", "description": "Case-insensitive filter on id, label and menu path"}}), &[]),
            true,
        ),
        tool(
            "describe_command",
            "Describe a command",
            "One command in full: label, menu, shortcut, parameter schema, why it is disabled.",
            obj(json!({"command": {"type": "string", "description": "Command id"}}), &["command"]),
            true,
        ),
        tool(
            "execute",
            "Run a command",
            "Run one command by id, e.g. {command: \"edit.undo\"} or {command: \"…\", params: {…}}.",
            obj(
                json!({"command": {"type": "string", "description": "Command id"}, "params": {"type": "object", "description": "Command parameters"}}),
                &["command"],
            ),
            false,
        ),
        tool(
            "batch",
            "Run several commands",
            "Run commands in order; stops at the first error unless keepGoing. Returns each step's result.",
            obj(json!({"commands": {"type": "array", "items": step}, "keepGoing": {"type": "boolean"}}), &["commands"]),
            false,
        ),
    ]
}

/// The schema of an element target: a ref string from `ui_snapshot` or an object.
fn target_schema(what: &str) -> Value {
    json!({
        "description": format!("{what}: \"@e3\" (a ref from ui_snapshot), \"id=<automation id>\", \"text=<label>\", or an object {{ref}} | {{id}} | {{text, role?}} | {{x, y}} (window points)"),
        "anyOf": [
            {"type": "string"},
            {"type": "object", "properties": {
                "ref": {"type": "string"}, "id": {"type": "string"}, "text": {"type": "string"}, "role": {"type": "string"},
                "x": {"type": "number"}, "y": {"type": "number"}}}
        ]
    })
}

/// `(tool, method, title, description, schema, read-only)` for the UI vocabulary.
fn ui_table() -> Vec<(&'static str, &'static str, &'static str, &'static str, Value, bool)> {
    let point = json!({"type": "object", "description": "{id} (element centre) or {x, y} (window points)",
        "properties": {"id": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"}}});
    let mods = json!({"type": "object", "properties": {"command": {"type": "boolean"}, "shift": {"type": "boolean"}, "alt": {"type": "boolean"}, "ctrl": {"type": "boolean"}}});
    let target = target_schema("The element");
    vec![
        (
            "screenshot",
            "ui.screenshot",
            "Screenshot",
            "A PNG of the app window, or of one element (`target`, `pad` points around it). `annotate` draws each interactive element's ref on the picture (and lists them); `highlight` outlines one element.",
            obj(
                json!({"id": {"type": "string", "description": "Legacy: crop to this automation id"}, "target": target_schema("Crop to this element"),
                    "annotate": {"type": "boolean"}, "highlight": target_schema("Outline this element"), "pad": {"type": "number"}}),
                &[],
            ),
            true,
        ),
        ("ui_inspect", "ui.inspect", "Inspect UI", "UI state and every element: id, label, rect [x,y,w,h].", obj(json!({}), &[]), true),
        (
            "ui_snapshot",
            "ui.snapshot",
            "Snapshot UI",
            "The UI as an indented outline, one element per line with a ref: `- button \"Save\" [ref=e3]`. Act on elements with their ref (\"@e3\") in the other ui_* tools; refs stay valid while the element exists. `interactive` keeps only elements you can act on; `scope` limits to one element's subtree; `elements` also returns every element as JSON (rect, value, state).",
            obj(
                json!({"interactive": {"type": "boolean"}, "scope": target_schema("Only this element's subtree"), "depth": {"type": "integer", "minimum": 0},
                    "compact": {"type": "boolean"}, "elements": {"type": "boolean", "description": "Also return the element list as JSON"}}),
                &[],
            ),
            true,
        ),
        (
            "ui_click",
            "ui.click",
            "Click",
            "Click an element (`target`, e.g. \"@e3\"), or a point.",
            obj(
                json!({"target": target, "id": {"type": "string"}, "x": {"type": "number"}, "y": {"type": "number"},
                    "button": {"type": "string", "enum": ["primary", "secondary", "middle"]}, "count": {"type": "integer", "minimum": 1, "maximum": 3}, "modifiers": mods}),
                &[],
            ),
            false,
        ),
        ("ui_hover", "ui.hover", "Hover", "Move the pointer over an element.", obj(json!({"target": target}), &["target"]), false),
        ("ui_focus", "ui.focus", "Focus", "Give an element keyboard focus.", obj(json!({"target": target}), &["target"]), false),
        (
            "ui_fill",
            "ui.fill",
            "Fill",
            "Replace a text field's contents (focus, select all, type); `submit` presses Enter after.",
            obj(json!({"target": target, "text": {"type": "string"}, "submit": {"type": "boolean"}}), &["target", "text"]),
            false,
        ),
        (
            "ui_press",
            "ui.press",
            "Press keys",
            "Press a chord, e.g. \"Mod+S\" (Mod = Cmd on macOS, Ctrl elsewhere), or a sequence \"Tab Tab Enter\".",
            obj(json!({"keys": {"type": "string"}}), &["keys"]),
            false,
        ),
        (
            "ui_drag",
            "ui.drag",
            "Drag",
            "Press, move and release: from one element or point to another.",
            obj(json!({"from": point, "to": point, "steps": {"type": "integer"}, "modifiers": mods}), &["from", "to"]),
            false,
        ),
        (
            "ui_scroll",
            "ui.scroll",
            "Scroll",
            "Scroll by dx, dy points (dy > 0 scrolls down), over an element (`target`) or a point.",
            obj(
                json!({"target": target, "x": {"type": "number"}, "y": {"type": "number"}, "dx": {"type": "number"}, "dy": {"type": "number"}}),
                &["dx", "dy"],
            ),
            false,
        ),
        (
            "ui_scroll_into_view",
            "ui.scroll_into_view",
            "Scroll into view",
            "Scroll until an element is visible.",
            obj(json!({"target": target}), &["target"]),
            false,
        ),
        (
            "ui_get",
            "ui.get",
            "Read element",
            "Read one property of an element: text, value, rect, enabled, visible, focused or checked.",
            obj(
                json!({"target": target, "what": {"type": "string", "enum": ["text", "value", "rect", "enabled", "visible", "focused", "checked"]}}),
                &["target", "what"],
            ),
            true,
        ),
        (
            "ui_wait",
            "ui.wait",
            "Wait",
            "Wait until an element appears (for: element), disappears (gone), becomes enabled (enabled), some text shows (text), or the app is idle (idle). Fails after timeout_ms (default 5000).",
            obj(
                json!({"for": {"type": "string", "enum": ["element", "gone", "text", "enabled", "idle"]}, "target": target, "text": {"type": "string"},
                    "timeout_ms": {"type": "integer", "minimum": 0}}),
                &["for"],
            ),
            true,
        ),
        (
            "ui_highlight",
            "ui.highlight",
            "Highlight",
            "Flash an outline around an element in the live app (to show a human what you mean).",
            obj(json!({"target": target, "ms": {"type": "integer"}}), &["target"]),
            true,
        ),
        (
            "ui_key",
            "ui.key",
            "Press key",
            "Press a key, e.g. {key: \"S\", modifiers: {command: true}}.",
            obj(json!({"key": {"type": "string"}, "modifiers": mods}), &["key"]),
            false,
        ),
        ("ui_type", "ui.type", "Type text", "Type text into the focused field.", obj(json!({"text": {"type": "string"}}), &["text"]), false),
        ("ui_menu_list", "ui.menu.list", "List menus", "The menu tree with enablement.", obj(json!({}), &[]), true),
        (
            "ui_menu_invoke",
            "ui.menu.invoke",
            "Invoke menu item",
            "Invoke a menu item by command id.",
            obj(json!({"id": {"type": "string"}}), &["id"]),
            false,
        ),
        ("ui_set", "ui.set", "Set view state", "Change view state (theme, workspace…; app-defined keys).", json!({"type": "object"}), false),
    ]
}

/// A target written as a string (`"@e3"`, `"e3"`, `"id=…"`, `"text=…"`) as the protocol's object;
/// objects pass through.
pub fn target_value(v: &Value) -> Result<Value, String> {
    let Value::String(s) = v else { return Ok(v.clone()) };
    let s = s.trim();
    let is_ref = |r: &str| r.strip_prefix('e').is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
    if let Some(r) = s.strip_prefix('@').filter(|r| !r.is_empty()) {
        return Ok(json!({"ref": r}));
    }
    if is_ref(s) {
        return Ok(json!({"ref": s}));
    }
    if let Some(id) = s.strip_prefix("id=").filter(|i| !i.is_empty()) {
        return Ok(json!({"id": id}));
    }
    if let Some(t) = s.strip_prefix("text=") {
        return Ok(json!({"text": t}));
    }
    Err(format!("`{s}` is not a target: use a ref like \"@e3\" (from ui_snapshot), \"id=…\", \"text=…\" or an object"))
}

/// Tool arguments with every string target (`target`, `scope`, `highlight`, `from`, `to`) made an
/// object.
pub fn normalize_targets(mut args: Value) -> Result<Value, String> {
    if let Some(o) = args.as_object_mut() {
        for k in ["target", "scope", "highlight", "from", "to"] {
            if let Some(v) = o.get(k).filter(|v| v.is_string()) {
                let t = target_value(v).map_err(|e| format!("{k}: {e}"))?;
                o.insert(k.into(), t);
            }
        }
    }
    Ok(args)
}

/// A `ui.snapshot` reply as a tool result: the outline (and the generation), plus the element
/// list as JSON when asked for.
pub fn snapshot_result(r: Reply, with_elements: bool) -> ToolResult {
    match r {
        Reply::Ok(v) => {
            let tree = v.get("tree").and_then(Value::as_str).unwrap_or("").trim_end().to_string();
            let generation = v.get("generation").cloned().unwrap_or(Value::Null);
            let mut content = vec![json!({"type": "text", "text": format!("{tree}\n(snapshot {generation})")})];
            if with_elements {
                content.push(json!({"type": "text", "text": json!({"generation": generation, "elements": v.get("elements")}).to_string()}));
            }
            ToolResult { content, is_error: false }
        }
        e => ToolResult::from_reply(e),
    }
}

/// The UI tools for the methods a backend answers.
pub fn ui_tools(methods: &[String]) -> Vec<Value> {
    ui_table().into_iter().filter(|t| methods.iter().any(|m| m == t.1)).map(|(n, _, title, d, s, ro)| tool(n, title, d, s, ro)).collect()
}

/// The control method behind a UI tool name.
pub fn ui_method(tool: &str) -> Option<&'static str> {
    ui_table().into_iter().find(|t| t.0 == tool).map(|t| t.1)
}

/// A command id as a tool name: `[A-Za-z0-9_-]`, at most 64 characters.
pub fn tool_name(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).take(64).collect()
}

/// The input schema of a command: its own when it is an object schema, otherwise any object.
pub fn input_schema(c: &CommandInfo) -> Value {
    match &c.params_schema {
        Value::Object(o) if o.get("type").and_then(Value::as_str) == Some("object") => Value::Object(o.clone()),
        _ => json!({"type": "object"}),
    }
}

/// One tool per command, skipping names that collide with the fixed tools or each other. Returns
/// the tool list and the `(tool name, command id)` pairs.
pub fn command_tools(commands: &[CommandInfo], taken: &[String]) -> (Vec<Value>, Vec<(String, String)>) {
    let mut tools = Vec::new();
    let mut names: Vec<(String, String)> = Vec::new();
    for c in commands {
        let name = tool_name(&c.id);
        if name.is_empty() || taken.contains(&name) || names.iter().any(|(n, _)| *n == name) {
            log::warn!("command `{}` has no tool: the name `{name}` is taken", c.id);
            continue;
        }
        let mut description = c.label.clone();
        if let Some(t) = &c.params_text {
            description.push_str(&format!(". Parameters: {t}"));
        }
        if !c.enabled
            && let Some(r) = &c.disabled_reason
        {
            description.push_str(&format!(" (currently unavailable: {r})"));
        }
        tools.push(tool(&name, &c.label, &description, input_schema(c), !c.mutates));
        names.push((name, c.id.clone()));
    }
    (tools, names)
}

/// The compact form `list_commands` returns.
pub fn compact(c: &CommandInfo) -> Value {
    let mut o = Map::new();
    o.insert("id".into(), json!(c.id));
    o.insert("label".into(), json!(c.label));
    o.insert("enabled".into(), json!(c.enabled));
    o.insert("mutates".into(), json!(c.mutates));
    if let Some(r) = &c.disabled_reason {
        o.insert("disabled_reason".into(), json!(r));
    }
    Value::Object(o)
}

/// An image result from a `ui.screenshot` reply (`{png, width, height, annotations?}`).
pub fn screenshot_result(r: Reply) -> ToolResult {
    match r {
        Reply::Ok(v) => match v.get("png").and_then(Value::as_str) {
            Some(png) if !png.is_empty() => {
                let mut info = json!({"width": v.get("width"), "height": v.get("height")});
                if let Some(a) = v.get("annotations") {
                    info["annotations"] = a.clone();
                }
                ToolResult {
                    content: vec![json!({"type": "image", "data": png, "mimeType": "image/png"}), json!({"type": "text", "text": info.to_string()})],
                    is_error: false,
                }
            }
            _ => ToolResult::error("the reply carried no picture"),
        },
        e => ToolResult::from_reply(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_safe() {
        assert_eq!(tool_name("file.save_as"), "file_save_as");
        assert_eq!(tool_name("a b/c"), "a_b_c");
        assert_eq!(tool_name(&"x".repeat(100)).len(), 64);
        assert_eq!(tool_name("é"), "_");
    }

    #[test]
    fn every_ui_tool_maps_to_a_ui_method() {
        for (name, method, ..) in ui_table() {
            assert_eq!(ui_method(name), Some(method));
            assert!(ashlar_control::UI_METHODS.contains(&method) || ashlar_control::DEBUG_METHODS.contains(&method), "{method}");
        }
        assert_eq!(ui_tools(&[]).len(), 0);
        assert_eq!(ui_tools(&["ui.screenshot".into()]).len(), 1);
        // Every debugger method has a tool (devtools.* are for humans, not agents).
        for m in ashlar_control::DEBUG_METHODS.iter().filter(|m| m.starts_with("ui.")) {
            assert!(ui_table().iter().any(|t| t.1 == *m), "{m} has no tool");
        }
    }

    #[test]
    fn string_targets_become_objects() {
        assert_eq!(target_value(&json!("@e3")), Ok(json!({"ref": "e3"})));
        assert_eq!(target_value(&json!("e12")), Ok(json!({"ref": "e12"})));
        assert_eq!(target_value(&json!("id=file.save")), Ok(json!({"id": "file.save"})));
        assert_eq!(target_value(&json!("text=Save")), Ok(json!({"text": "Save"})));
        assert_eq!(target_value(&json!({"x": 1, "y": 2})), Ok(json!({"x": 1, "y": 2})));
        assert!(target_value(&json!("Save")).is_err());
        let a = normalize_targets(json!({"target": "@e1", "from": "e2", "to": {"id": "x"}, "text": "@e9"})).unwrap();
        assert_eq!(a, json!({"target": {"ref": "e1"}, "from": {"ref": "e2"}, "to": {"id": "x"}, "text": "@e9"}));
        assert!(normalize_targets(json!({"scope": "nope"})).unwrap_err().starts_with("scope:"));
    }
}
