//! JSON-RPC 2.0 framing and the MCP lifecycle and tool methods. Hand-rolled, synchronous, one
//! message per line on stdin/stdout. Nothing but protocol is ever written to the output.

use std::io::{BufRead, Write};

use ashlar_command::CommandInfo;
use ashlar_control::{AppInfo, Reply, matches_query};
use serde_json::{Value, json};

use crate::backend::Backend;
use crate::tools::{self, ToolMode, ToolResult};

/// The newest MCP revision implemented; older ones are accepted when a client asks for them.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

/// Longest accepted message line.
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// How the server presents itself.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// `serverInfo.name` and the `app.version` name for a headless backend.
    pub name: String,
    pub title: Option<String>,
    pub version: String,
    /// What the app is and how to drive it; the backend's description is appended.
    pub instructions: String,
    pub tools: ToolMode,
}

impl ServerOptions {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self { name: name.into(), title: None, version: version.into(), instructions: String::new(), tools: ToolMode::Fixed }
    }

    pub fn title(mut self, t: impl Into<String>) -> Self {
        self.title = Some(t.into());
        self
    }

    pub fn instructions(mut self, t: impl Into<String>) -> Self {
        self.instructions = t.into();
        self
    }

    pub fn tools(mut self, m: ToolMode) -> Self {
        self.tools = m;
        self
    }
}

const DEFAULT_INSTRUCTIONS: &str = "Every action in this app is a command. Find ids with list_commands, read parameters with \
describe_command, run one with execute or several with batch.";

/// An MCP server bound to one backend.
pub struct McpServer {
    backend: Backend,
    opts: ServerOptions,
    app: AppInfo,
    initialized: bool,
    /// `(tool name, command id)` of the per-command tools last listed.
    command_tools: Vec<(String, String)>,
}

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

type RpcResult = Result<Value, (i64, String)>;

impl McpServer {
    pub fn new(backend: Backend, opts: ServerOptions) -> Self {
        let app = AppInfo::new(&opts.name, &opts.version);
        Self { backend, opts, app, initialized: false, command_tools: Vec::new() }
    }

    pub fn backend(&mut self) -> &mut Backend {
        &mut self.backend
    }

    /// Whether the client has sent `notifications/initialized`.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Serve newline-delimited JSON-RPC until `input` ends. Only replies are written to `output`.
    pub fn serve(&mut self, mut input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let limit = u64::try_from(MAX_MESSAGE).unwrap_or(u64::MAX).saturating_add(1);
            let n = std::io::Read::take(&mut input, limit).read_until(b'\n', &mut buf)?;
            if n == 0 {
                return Ok(());
            }
            let reply = if buf.len() > MAX_MESSAGE && buf.last() != Some(&b'\n') {
                skip_line(&mut input)?;
                Some(error(Value::Null, PARSE_ERROR, format!("message longer than {MAX_MESSAGE} bytes")).to_string())
            } else {
                match std::str::from_utf8(&buf) {
                    Ok(line) => self.handle_line(line),
                    Err(_) => Some(error(Value::Null, PARSE_ERROR, "parse error: not UTF-8").to_string()),
                }
            };
            if let Some(r) = reply {
                output.write_all(r.as_bytes())?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
    }

    /// Handle one line; the reply line, or `None` for notifications, responses and blank lines.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        let reply = match serde_json::from_str::<Value>(line) {
            // Batches were removed in 2025-06-18; older clients may still send them.
            Ok(Value::Array(batch)) if batch.is_empty() => Some(error(Value::Null, INVALID_REQUEST, "empty batch")),
            Ok(Value::Array(batch)) => {
                let replies: Vec<Value> = batch.into_iter().filter_map(|m| self.handle(m)).collect();
                (!replies.is_empty()).then_some(Value::Array(replies))
            }
            Ok(msg) => self.handle(msg),
            Err(e) => Some(error(Value::Null, PARSE_ERROR, format!("parse error: {e}"))),
        };
        reply.map(|r| r.to_string())
    }

    /// Handle one JSON-RPC message; `None` for notifications and responses.
    pub fn handle(&mut self, msg: Value) -> Option<Value> {
        let Value::Object(o) = &msg else { return Some(error(Value::Null, INVALID_REQUEST, "a message must be a JSON object")) };
        let id = o.get("id").cloned();
        let valid_id = id.as_ref().is_some_and(|i| i.is_string() || i.is_i64() || i.is_u64());
        let reply_id = || if valid_id { id.clone().unwrap_or(Value::Null) } else { Value::Null };
        if o.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error(reply_id(), INVALID_REQUEST, "`jsonrpc` must be \"2.0\""));
        }
        let Some(method) = o.get("method").and_then(Value::as_str) else {
            // A response to a server→client request (we send none): ignore it.
            if o.contains_key("result") || o.contains_key("error") {
                return None;
            }
            return Some(error(reply_id(), INVALID_REQUEST, "missing string `method`"));
        };
        let params = match o.get("params") {
            None | Some(Value::Null) => json!({}),
            Some(p @ Value::Object(_)) => p.clone(),
            Some(_) if id.is_none() => return None,
            Some(_) => return Some(error(reply_id(), INVALID_PARAMS, "`params` must be an object")),
        };
        let Some(id) = id else {
            self.notification(method);
            return None;
        };
        if !valid_id {
            return Some(error(Value::Null, INVALID_REQUEST, "`id` must be a string or an integer"));
        }
        Some(match self.request(method, &params) {
            Ok(r) => response(id, r),
            Err((code, m)) => error(id, code, m),
        })
    }

    fn notification(&mut self, method: &str) {
        match method {
            "notifications/initialized" => self.initialized = true,
            other => log::debug!("ignoring notification {other}"),
        }
    }

    fn request(&mut self, method: &str, params: &Value) -> RpcResult {
        match method {
            "initialize" => Ok(self.initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": self.list_tools()})),
            "tools/call" => self.call_tool(params),
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
        let version = SUPPORTED_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSION);
        let mut info = json!({"name": self.opts.name, "version": self.opts.version});
        if let Some(t) = &self.opts.title {
            info["title"] = json!(t);
        }
        let base = if self.opts.instructions.is_empty() { DEFAULT_INSTRUCTIONS } else { self.opts.instructions.as_str() };
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": info,
            "instructions": format!("{base} Backend: {}.", self.backend.describe()),
        })
    }

    fn call(&mut self, method: &str, params: Value) -> Reply {
        self.backend.call(method, params, &self.app)
    }

    /// The methods the backend answers (`app.version`); empty when it cannot say.
    fn methods(&mut self) -> Vec<String> {
        match self.call("app.version", json!({})) {
            Reply::Ok(v) => {
                v.get("methods").and_then(Value::as_array).map(|m| m.iter().filter_map(Value::as_str).map(String::from).collect()).unwrap_or_default()
            }
            Reply::Err { .. } => Vec::new(),
        }
    }

    fn commands(&mut self) -> Result<Vec<CommandInfo>, String> {
        match self.call("engine.commands", json!({})) {
            Reply::Ok(v) => serde_json::from_value(v).map_err(|e| format!("the app listed its commands in an unknown shape: {e}")),
            Reply::Err { error, .. } => Err(error),
        }
    }

    fn list_tools(&mut self) -> Vec<Value> {
        let mut list = tools::core_tools();
        let methods = self.methods();
        list.extend(tools::ui_tools(&methods));
        if self.opts.tools == ToolMode::PerCommand {
            let taken: Vec<String> = list.iter().filter_map(|t| t.get("name").and_then(Value::as_str)).map(String::from).collect();
            let commands = self.commands().unwrap_or_default();
            let (more, names) = tools::command_tools(&commands, &taken);
            list.extend(more);
            self.command_tools = names;
        }
        list
    }

    fn call_tool(&mut self, params: &Value) -> RpcResult {
        let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "tools/call needs a string `name`".to_string()))?;
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => return Err((INVALID_PARAMS, "`arguments` must be an object".to_string())),
        };
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let r = match name {
            "list_commands" => match self.commands() {
                Ok(list) => {
                    let q = s("query").unwrap_or_default().to_lowercase();
                    ToolResult::json(&Value::Array(list.iter().filter(|c| matches_query(c, &q)).map(tools::compact).collect()))
                }
                Err(e) => ToolResult::error(e),
            },
            "describe_command" => match (s("command"), self.commands()) {
                (None, _) => ToolResult::error("describe_command needs a string `command`"),
                (_, Err(e)) => ToolResult::error(e),
                (Some(id), Ok(list)) => match list.into_iter().find(|c| c.id == id) {
                    Some(c) => ToolResult::json(&serde_json::to_value(c).unwrap_or_default()),
                    None => ToolResult::error(format!("no command `{id}`; see list_commands")),
                },
            },
            "execute" => match s("command") {
                Some(c) => {
                    let p = args.get("params").cloned().unwrap_or(Value::Null);
                    ToolResult::from_reply(self.call("engine.execute", json!({"command": c, "params": p})))
                }
                None => ToolResult::error("execute needs a string `command`"),
            },
            "batch" => ToolResult::from_reply(self.call("engine.batch", args)),
            "screenshot" => match tools::normalize_targets(args) {
                Ok(a) => tools::screenshot_result(self.call("ui.screenshot", a)),
                Err(e) => ToolResult::error(e),
            },
            "ui_snapshot" => match tools::normalize_targets(args) {
                Ok(mut a) => {
                    // `elements` shapes the tool result; the app is not asked for it.
                    let with_elements = a.as_object_mut().and_then(|o| o.remove("elements")).and_then(|v| v.as_bool()).unwrap_or(false);
                    tools::snapshot_result(self.call("ui.snapshot", a), with_elements)
                }
                Err(e) => ToolResult::error(e),
            },
            other => {
                if let Some(method) = tools::ui_method(other) {
                    match tools::normalize_targets(args) {
                        Ok(a) => ToolResult::from_reply(self.call(method, a)),
                        Err(e) => ToolResult::error(e),
                    }
                } else if let Some(id) = self.command_for_tool(other) {
                    ToolResult::from_reply(self.call("engine.execute", json!({"command": id, "params": args})))
                } else {
                    return Err((INVALID_PARAMS, format!("unknown tool: {other}")));
                }
            }
        };
        Ok(r.to_value())
    }

    /// The command behind a per-command tool (listing the tools again once when unknown).
    fn command_for_tool(&mut self, name: &str) -> Option<String> {
        if self.opts.tools != ToolMode::PerCommand {
            return None;
        }
        let find = |names: &[(String, String)]| names.iter().find(|(n, _)| n == name).map(|(_, id)| id.clone());
        find(&self.command_tools).or_else(|| {
            self.list_tools();
            find(&self.command_tools)
        })
    }
}

/// Drop input up to and including the next newline.
fn skip_line(input: &mut impl BufRead) -> std::io::Result<()> {
    loop {
        let (used, done) = {
            let b = input.fill_buf()?;
            if b.is_empty() {
                return Ok(());
            }
            match b.iter().position(|&c| c == b'\n') {
                Some(i) => (i.saturating_add(1), true),
                None => (b.len(), false),
            }
        };
        input.consume(used);
        if done {
            return Ok(());
        }
    }
}
