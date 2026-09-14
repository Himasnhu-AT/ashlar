//! JSON-RPC 2.0 framing and the MCP lifecycle, tool and resource methods. Hand-rolled, one
//! message per line on stdin/stdout. Nothing but protocol is ever written to the output.
//!
//! Requests are handled one at a time, in order. While one runs, a reader thread still answers
//! `ping` and takes `notifications/cancelled`: a cancelled request gets no response, and a
//! stepwise `batch` stops before its next step.

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};

use ashlar_command::CommandInfo;
use ashlar_control::{AppInfo, Reply, matches_query};
use serde_json::{Value, json};

use crate::backend::Backend;
use crate::calls::{PendingCall, ToolCx, ToolOutcome};
use crate::tools::{self, Access, ToolMode, ToolResult};

/// The newest MCP revision answered through the `initialize` handshake.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// The newest revision implemented: no `initialize`; each request names its version and client
/// capabilities in `_meta`, `server/discover` describes the server, and list and read results
/// carry `resultType` and cache hints.
pub const LATEST_PROTOCOL_VERSION: &str = "2026-07-28";
/// Revisions from this one on have no `initialize` handshake.
const NO_INITIALIZE: &str = "2026-07-28";
pub const SUPPORTED_VERSIONS: &[&str] = &[LATEST_PROTOCOL_VERSION, PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

/// Request `_meta` keys of the newer revisions.
const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// Longest accepted message line.
pub const MAX_MESSAGE: usize = 64 * 1024 * 1024;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const RESOURCE_NOT_FOUND: i64 = -32002;
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Most cancellations remembered for requests that have not arrived or finished yet.
const MAX_CANCELLED: usize = 1024;

/// How often pending tool calls are polled.
const POLL: std::time::Duration = std::time::Duration::from_millis(100);

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
    /// Refuse a tool call with an argument its input schema does not list (JSON-RPC
    /// `-32602`), instead of passing it on to be ignored.
    pub strict_arguments: bool,
    /// How long (ms) a client on a revision with cache hints may keep `tools/list` and
    /// `resources/list` results (0: always ask again).
    pub list_ttl_ms: u64,
}

impl ServerOptions {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            title: None,
            version: version.into(),
            instructions: String::new(),
            tools: ToolMode::Fixed,
            strict_arguments: false,
            list_ttl_ms: 0,
        }
    }

    pub fn list_ttl_ms(mut self, ms: u64) -> Self {
        self.list_ttl_ms = ms;
        self
    }

    pub fn strict_arguments(mut self, on: bool) -> Self {
        self.strict_arguments = on;
        self
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

/// A tool of the app's own, beyond the command vocabulary ([`McpServer::tool`]).
type RunTool = Box<dyn FnMut(&Value, &mut ToolCx<'_>) -> ToolOutcome>;

struct CustomTool {
    def: Value,
    run: RunTool,
}

/// A resource of the app's own ([`McpServer::resource`]).
type ReadResource = Box<dyn FnMut(&mut ToolCx<'_>) -> Result<String, String>>;

struct CustomResource {
    def: Value,
    read: ReadResource,
}

/// The app's say over `execute` ([`McpServer::on_execute`]).
type ExecuteHook = Box<dyn FnMut(&str, &Value, &mut ToolCx<'_>) -> Option<ToolOutcome>>;

/// A tool call answered later.
struct Pending {
    id: Value,
    /// Answered on a revision with `resultType` (the response is decorated).
    modern: bool,
    key: String,
    token: Option<Value>,
    last: f64,
    call: Box<dyn PendingCall>,
}

/// What `tools/call` produced.
enum CallOutcome {
    Now(Value),
    Later(Box<dyn PendingCall>),
}

/// An MCP server bound to one backend.
pub struct McpServer {
    backend: Backend,
    opts: ServerOptions,
    app: AppInfo,
    initialized: bool,
    /// `(tool name, command id)` of the per-command tools last listed.
    command_tools: Vec<(String, String)>,
    custom: Vec<CustomTool>,
    resources: Vec<CustomResource>,
    docs: Vec<crate::docs::Doc>,
    on_execute: Option<ExecuteHook>,
    pending: Vec<Pending>,
    /// The tools last listed (for strict arguments).
    listed: Vec<Value>,
    /// Ids (as JSON text) of requests the client cancelled.
    cancelled: Arc<Mutex<HashSet<String>>>,
    /// The request being handled (its id as JSON text).
    current: Option<String>,
    /// Lines to write while a request runs (progress notifications), when serving.
    notify: Option<Sender<String>>,
}

fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

/// Whether a revision has no `initialize` handshake (the date strings sort in order).
fn without_initialize(version: &str) -> bool {
    version >= NO_INITIALIZE
}

type RpcResult = Result<Value, (i64, String)>;

impl McpServer {
    pub fn new(backend: Backend, opts: ServerOptions) -> Self {
        let app = AppInfo::new(&opts.name, &opts.version);
        Self {
            backend,
            opts,
            app,
            initialized: false,
            command_tools: Vec::new(),
            custom: Vec::new(),
            resources: Vec::new(),
            docs: Vec::new(),
            on_execute: None,
            pending: Vec::new(),
            listed: Vec::new(),
            cancelled: Arc::default(),
            current: None,
            notify: None,
        }
    }

    /// Add a tool of the app's own: `run` gets the call's arguments and a [`ToolCx`] (the
    /// backend, progress, cancellation), and returns a result now or a [`PendingCall`] the server
    /// polls. A name the fixed or UI vocabulary already uses is refused (logged) and the tool is
    /// not added.
    pub fn tool<R: Into<ToolOutcome>>(
        mut self,
        name: &str,
        title: &str,
        description: &str,
        input_schema: Value,
        access: Access,
        mut run: impl FnMut(&Value, &mut ToolCx<'_>) -> R + 'static,
    ) -> Self {
        let fixed = tools::core_tools().into_iter().chain(tools::ui_tools(&tools::all_ui_methods()));
        let taken = fixed.chain(self.custom.iter().map(|t| t.def.clone())).any(|t| t.get("name").and_then(Value::as_str) == Some(name));
        if taken || name.is_empty() {
            log::warn!("tool `{name}` not added: the name is empty or taken");
            return self;
        }
        let def = tools::custom_tool(name, title, description, input_schema, access);
        self.custom.push(CustomTool { def, run: Box::new(move |a, cx| run(a, cx).into()) });
        self
    }

    /// Add a resource of the app's own (`resources/list` / `resources/read`); `read` returns its
    /// text. A URI already offered is refused (logged).
    pub fn resource(
        mut self,
        uri: &str,
        name: &str,
        title: &str,
        description: &str,
        mime_type: &str,
        read: impl FnMut(&mut ToolCx<'_>) -> Result<String, String> + 'static,
    ) -> Self {
        let taken = [RES_APP, RES_COMMANDS, RES_UI].contains(&uri) || self.resources.iter().any(|r| r.def["uri"] == uri);
        if taken || uri.is_empty() {
            log::warn!("resource `{uri}` not added: the URI is empty or taken");
            return self;
        }
        let def = json!({"uri": uri, "name": name, "title": title, "description": description, "mimeType": mime_type,
            "annotations": {"audience": ["assistant"], "priority": 0.8}});
        self.resources.push(CustomResource { def, read: Box::new(read) });
        self
    }

    /// Add the markdown files under `dir` as documents (read now; symbolic links skipped). For a
    /// directory the caller chose explicitly: a shipped app embeds its pages with [`Self::docs`].
    pub fn docs_dir(mut self, dir: impl AsRef<std::path::Path>) -> Self {
        self.docs.extend(crate::docs::load_docs_from_dir(dir.as_ref()));
        self
    }

    /// Add documents, exposed as `docs://` resources and the `list_docs` / `read_doc` tools. The
    /// first one is the entry point: the instructions name it and it is listed with top priority.
    pub fn docs(mut self, docs: impl IntoIterator<Item = crate::docs::Doc>) -> Self {
        self.docs.extend(docs);
        self
    }

    /// Add a single document to be exposed as an MCP resource.
    pub fn doc(mut self, doc: crate::docs::Doc) -> Self {
        self.docs.push(doc);
        self
    }

    /// Let the app take over `execute` (and per-command tools) for some commands, e.g. a long
    /// export it runs as a job with progress. `hook` gets the command id and its params and
    /// returns `None` to let the command run as usual.
    pub fn on_execute(mut self, hook: impl FnMut(&str, &Value, &mut ToolCx<'_>) -> Option<ToolOutcome> + 'static) -> Self {
        self.on_execute = Some(Box::new(hook));
        self
    }

    /// Whether the client cancelled the request being handled.
    fn is_cancelled(&self) -> bool {
        self.current.as_ref().is_some_and(|k| self.cancelled.lock().unwrap_or_else(PoisonError::into_inner).contains(k))
    }

    /// Forget a cancellation; whether there was one.
    fn take_cancelled(&self, key: &str) -> bool {
        self.cancelled.lock().unwrap_or_else(PoisonError::into_inner).remove(key)
    }

    pub fn backend(&mut self) -> &mut Backend {
        &mut self.backend
    }

    /// Whether the client has sent `notifications/initialized`.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// Serve newline-delimited JSON-RPC until `input` ends. Only protocol is written to `output`.
    /// Reading and writing run on their own threads, so `ping` and cancellations are taken while
    /// a request runs.
    pub fn serve(&mut self, input: impl BufRead + Send, output: impl Write + Send) -> std::io::Result<()> {
        let (out_tx, out_rx) = channel::<String>();
        let (in_tx, in_rx) = channel::<std::io::Result<String>>();
        let cancelled = Arc::clone(&self.cancelled);
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || write_lines(output, out_rx));
            let direct = out_tx.clone();
            scope.spawn(move || read_lines(input, &in_tx, &direct, &cancelled));
            self.notify = Some(out_tx.clone());
            let mut result = Ok(());
            loop {
                // with calls pending, wake up to poll them; otherwise wait for input
                let line = if self.pending.is_empty() { in_rx.recv().map_err(|_| RecvTimeoutError::Disconnected) } else { in_rx.recv_timeout(POLL) };
                let reply = match line {
                    Ok(Ok(line)) => self.handle_line(&line),
                    Ok(Err(e)) => {
                        result = Err(e);
                        break;
                    }
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                let done = self.poll_pending();
                if reply.into_iter().chain(done).any(|r| out_tx.send(r).is_err()) {
                    break; // the writer stopped: its error is reported below
                }
            }
            // input ended with calls still running: finish them (their clients may still read)
            while !self.pending.is_empty() {
                std::thread::sleep(POLL);
                for r in self.poll_pending() {
                    let _ = out_tx.send(r);
                }
            }
            self.notify = None;
            drop(out_tx);
            let written = writer.join().unwrap_or_else(|_| Err(std::io::Error::other("the output thread panicked")));
            result.and(written)
        })
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
        // the newer revisions name their version and client capabilities on every request
        let meta = params.get("_meta");
        let version = meta.and_then(|m| m.get(META_VERSION)).and_then(Value::as_str).map(str::to_string);
        let modern = match version.as_deref() {
            Some(v) if without_initialize(v) || method == "server/discover" => {
                let mut missing = Vec::new();
                if meta.and_then(|m| m.get(META_CLIENT_CAPABILITIES)).is_none_or(|c| !c.is_object()) {
                    missing.push(META_CLIENT_CAPABILITIES);
                }
                if !missing.is_empty() {
                    return Some(error(
                        id,
                        INVALID_PARAMS,
                        format!("request _meta is missing or has malformed required fields: {}", missing.join(", ")),
                    ));
                }
                if method != "server/discover" && !SUPPORTED_VERSIONS.contains(&v) {
                    let mut e = error(id, UNSUPPORTED_PROTOCOL_VERSION, "Unsupported protocol version");
                    e["error"]["data"] = json!({"requested": v, "supported": SUPPORTED_VERSIONS});
                    return Some(e);
                }
                without_initialize(v)
            }
            _ if method == "server/discover" => {
                return Some(error(
                    id,
                    INVALID_PARAMS,
                    format!("request _meta is missing or has malformed required fields: {META_VERSION}, {META_CLIENT_CAPABILITIES}"),
                ));
            }
            _ => false,
        };
        // a cancelled request gets no response, whether it was cancelled before or while it ran
        let key = id.to_string();
        if self.take_cancelled(&key) {
            return None;
        }
        self.current = Some(key.clone());
        let r = if method == "tools/call" {
            match self.call_tool(&params) {
                Ok(CallOutcome::Now(v)) => Ok(v),
                Ok(CallOutcome::Later(call)) => {
                    self.current = None;
                    let token = progress_token(&params).cloned();
                    self.pending.push(Pending { id: id.clone(), modern, key, token, last: f64::NEG_INFINITY, call });
                    // served: the loop polls it; handled directly: finish it here
                    return if self.notify.is_some() { None } else { self.finish_pending() };
                }
                Err(e) => Err(e),
            }
        } else {
            self.request(method, &params)
        };
        self.current = None;
        if self.take_cancelled(&key) {
            return None;
        }
        Some(match r {
            Ok(r) => response(id, if modern { self.decorate(method, r) } else { r }),
            Err((code, m)) => error(id, code, m),
        })
    }

    /// A result as the newer revisions want it: `resultType`, and cache hints on lists and reads.
    fn decorate(&self, method: &str, mut r: Value) -> Value {
        let cacheable = matches!(method, "tools/list" | "resources/list" | "resources/templates/list" | "resources/read" | "prompts/list");
        if !cacheable && method != "tools/call" && method != "prompts/get" {
            return r;
        }
        if let Some(o) = r.as_object_mut() {
            o.entry("resultType").or_insert(json!("complete"));
            if cacheable {
                let ttl = if method.ends_with("/list") { self.opts.list_ttl_ms } else { 0 };
                o.entry("ttlMs").or_insert(json!(ttl));
                o.entry("cacheScope").or_insert(json!("private"));
            }
        }
        r
    }

    /// `server/discover`: what an initialize result says, for clients without the handshake.
    fn discover(&mut self) -> Value {
        let mut info = json!({"name": self.opts.name, "version": self.opts.version});
        if let Some(t) = &self.opts.title {
            info["title"] = json!(t);
        }
        json!({
            "resultType": "complete",
            "supportedVersions": SUPPORTED_VERSIONS,
            "capabilities": self.capabilities(),
            "instructions": self.instructions(),
            "ttlMs": 0,
            "cacheScope": "private",
            "_meta": {META_SERVER_INFO: info},
        })
    }

    fn capabilities(&self) -> Value {
        json!({"tools": {"listChanged": false}, "resources": {"listChanged": false, "subscribe": false}})
    }

    fn instructions(&self) -> String {
        let base = if self.opts.instructions.is_empty() { DEFAULT_INSTRUCTIONS } else { self.opts.instructions.as_str() };
        let docs = match self.docs.first() {
            Some(first) => format!(" Documentation and skills for agents: `list_docs`, then `read_doc` (start with `{}`).", first.path),
            None => String::new(),
        };
        format!("{base}{docs} Backend: {}.", self.backend.describe())
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
            "server/discover" => Ok(self.discover()),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": self.list_tools()})),
            "resources/list" => Ok(json!({"resources": self.list_resources()})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "resources/read" => self.read_resource(params),
            other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
        }
    }

    fn initialize(&mut self, params: &Value) -> Value {
        // the handshake only answers revisions that have one; a newer client falls back to it
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
        let version = SUPPORTED_VERSIONS.iter().find(|v| **v == asked && !without_initialize(v)).copied().unwrap_or(PROTOCOL_VERSION);
        let mut info = json!({"name": self.opts.name, "version": self.opts.version});
        if let Some(t) = &self.opts.title {
            info["title"] = json!(t);
        }
        json!({
            "protocolVersion": version,
            "capabilities": self.capabilities(),
            "serverInfo": info,
            "instructions": self.instructions(),
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
        list.extend(self.custom.iter().map(|t| t.def.clone()));
        if !self.docs.is_empty() {
            list.push(json!({
                "name": "list_docs",
                "title": "List documentation and skills",
                "description": "List the documentation and skills this app ships for agents: path, title, description, section. Optional filters by keyword (path, title, description or text) and by section.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Keyword in the path, title, description or text"
                        },
                        "section": {
                            "type": "string",
                            "description": "Section (the top folder, or skills, styles, reference)"
                        }
                    },
                    "additionalProperties": false
                },
                "annotations": {
                    "title": "List documentation and skills",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                }
            }));
            list.push(json!({
                "name": "read_doc",
                "title": "Read documentation or skill",
                "description": "Read one document's markdown by path or docs:// URI (from list_docs).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The document's path (e.g. 'skills/review.md') or URI; a unique file name also works"
                        }
                    },
                    "required": ["path"],
                    "additionalProperties": false
                },
                "annotations": {
                    "title": "Read documentation or skill",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                }
            }));
        }
        self.listed.clone_from(&list);
        list
    }

    /// Poll the pending calls once: the responses of those that finished (cancelled ones get none).
    fn poll_pending(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            let cancelled = self.cancelled.lock().unwrap_or_else(PoisonError::into_inner).contains(&self.pending[i].key);
            let p = &mut self.pending[i];
            let mut cx = ToolCx::new(&mut self.backend, &self.app, self.notify.as_ref(), p.token.as_ref(), cancelled);
            cx.set_last_progress(p.last);
            let finished = if cancelled {
                p.call.cancel(&mut cx);
                Some(None)
            } else {
                p.call.poll(&mut cx).map(Some)
            };
            p.last = cx.last_progress();
            match finished {
                None => i += 1,
                Some(result) => {
                    let p = self.pending.remove(i);
                    self.take_cancelled(&p.key);
                    if let Some(r) = result {
                        let v = if p.modern { self.decorate("tools/call", r.to_value()) } else { r.to_value() };
                        out.push(response(p.id, v).to_string());
                    }
                }
            }
        }
        out
    }

    /// Run the pending calls to the end (handling one message without the serve loop).
    fn finish_pending(&mut self) -> Option<Value> {
        let mut last = None;
        while !self.pending.is_empty() {
            for r in self.poll_pending() {
                last = serde_json::from_str(&r).ok();
            }
            if !self.pending.is_empty() {
                std::thread::sleep(POLL);
            }
        }
        last
    }

    /// Refuse arguments the tool's input schema does not list (strict mode).
    fn check_arguments(&mut self, name: &str, args: &Value) -> Result<(), (i64, String)> {
        if !self.opts.strict_arguments {
            return Ok(());
        }
        let find = |l: &[Value]| l.iter().find(|t| t["name"] == name).map(|t| t["inputSchema"].clone());
        let schema = match find(&self.listed) {
            Some(s) => s,
            None => {
                let l = self.list_tools();
                match find(&l) {
                    Some(s) => s,
                    None => return Ok(()),
                }
            }
        };
        let Some(props) = schema.get("properties").and_then(Value::as_object) else { return Ok(()) };
        if schema.get("additionalProperties") == Some(&Value::Bool(true)) {
            return Ok(());
        }
        let unknown: Vec<&str> = args.as_object().into_iter().flatten().map(|(k, _)| k.as_str()).filter(|k| !props.contains_key(*k)).collect();
        if unknown.is_empty() {
            return Ok(());
        }
        let known: Vec<&str> = props.keys().map(String::as_str).collect();
        Err((
            INVALID_PARAMS,
            format!(
                "unknown argument{} {} for `{name}` (it takes: {})",
                if unknown.len() > 1 { "s" } else { "" },
                unknown.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", "),
                known.join(", ")
            ),
        ))
    }

    /// Run `command` through the app's [`McpServer::on_execute`] hook, or as usual.
    fn run_command(&mut self, command: &str, params: Value, token: Option<&Value>) -> ToolOutcome {
        if let Some(hook) = self.on_execute.as_mut() {
            let cancelled = self.current.as_ref().is_some_and(|k| self.cancelled.lock().unwrap_or_else(PoisonError::into_inner).contains(k));
            let mut cx = ToolCx::new(&mut self.backend, &self.app, self.notify.as_ref(), token, cancelled);
            if let Some(o) = hook(command, &params, &mut cx) {
                return o;
            }
        }
        ToolResult::from_reply(self.call("engine.execute", json!({"command": command, "params": params}))).into()
    }

    fn call_tool(&mut self, params: &Value) -> Result<CallOutcome, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "tools/call needs a string `name`".to_string()))?;
        let args = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => return Err((INVALID_PARAMS, "`arguments` must be an object".to_string())),
        };
        self.check_arguments(name, &args)?;
        let token = progress_token(params).cloned();
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let outcome: ToolOutcome = match name {
            "list_commands" => match self.commands() {
                Ok(list) => {
                    let q = s("query").unwrap_or_default().to_lowercase();
                    ToolResult::json(&Value::Array(list.iter().filter(|c| matches_query(c, &q)).map(tools::compact).collect())).into()
                }
                Err(e) => ToolResult::error(e).into(),
            },
            "describe_command" => match (s("command"), self.commands()) {
                (None, _) => ToolResult::error("describe_command needs a string `command`").into(),
                (_, Err(e)) => ToolResult::error(e).into(),
                (Some(id), Ok(list)) => match list.into_iter().find(|c| c.id == id) {
                    Some(c) => ToolResult::json(&serde_json::to_value(c).unwrap_or_default()).into(),
                    None => ToolResult::error(format!("no command `{id}`; see list_commands")).into(),
                },
            },
            "execute" => match s("command") {
                Some(c) => {
                    let p = args.get("params").cloned().unwrap_or(Value::Null);
                    self.run_command(&c, p, token.as_ref())
                }
                None => ToolResult::error("execute needs a string `command`").into(),
            },
            "batch" => match token.as_ref() {
                Some(token) => self.batch_in_steps(&args, token).into(),
                None => ToolResult::from_reply(self.call("engine.batch", args)).into(),
            },
            "screenshot" => match tools::normalize_targets(args) {
                Ok(a) => tools::screenshot_result(self.call("ui.screenshot", a)).into(),
                Err(e) => ToolResult::error(e).into(),
            },
            "ui_snapshot" => match tools::normalize_targets(args) {
                Ok(mut a) => {
                    // `elements` shapes the tool result; the app is not asked for it.
                    let with_elements = a.as_object_mut().and_then(|o| o.remove("elements")).and_then(|v| v.as_bool()).unwrap_or(false);
                    tools::snapshot_result(self.call("ui.snapshot", a), with_elements).into()
                }
                Err(e) => ToolResult::error(e).into(),
            },
            "list_docs" => {
                let (q, sec) = (args.get("query").and_then(Value::as_str), args.get("section").and_then(Value::as_str));
                let matched: Vec<Value> = self
                    .docs
                    .iter()
                    .filter(|d| d.matches(q, sec))
                    .map(|d| {
                        json!({
                            "path": d.path,
                            "title": d.title,
                            "description": d.description,
                            "section": d.section,
                            "uri": d.uri
                        })
                    })
                    .collect();
                ToolResult::text(serde_json::to_string_pretty(&matched).unwrap_or_default()).into()
            }
            "read_doc" => match args.get("path").and_then(Value::as_str) {
                Some(path) => match crate::docs::find(&self.docs, path) {
                    Ok(d) => match d.read_content() {
                        Ok(content) => ToolResult::text(content).into(),
                        Err(e) => ToolResult::error(format!("reading {}: {e}", d.path)).into(),
                    },
                    Err(e) => ToolResult::error(format!("{e}; use list_docs to find documents")).into(),
                },
                None => ToolResult::error("read_doc needs a string `path`").into(),
            },
            other => {
                if let Some(method) = tools::ui_method(other) {
                    match tools::normalize_targets(args) {
                        Ok(a) => ToolResult::from_reply(self.call(method, a)).into(),
                        Err(e) => ToolResult::error(e).into(),
                    }
                } else if let Some(t) = self.custom.iter_mut().find(|t| t.def.get("name").and_then(Value::as_str) == Some(other)) {
                    let cancelled = self.current.as_ref().is_some_and(|k| self.cancelled.lock().unwrap_or_else(PoisonError::into_inner).contains(k));
                    let mut cx = ToolCx::new(&mut self.backend, &self.app, self.notify.as_ref(), token.as_ref(), cancelled);
                    (t.run)(&args, &mut cx)
                } else if let Some(id) = self.command_for_tool(other) {
                    self.run_command(&id, args, token.as_ref())
                } else {
                    return Err((INVALID_PARAMS, format!("unknown tool: {other}")));
                }
            }
        };
        Ok(match outcome {
            ToolOutcome::Done(r) => CallOutcome::Now(r.to_value()),
            ToolOutcome::Pending(call) => CallOutcome::Later(call),
        })
    }

    /// `batch` one step at a time, for a client that asked for progress: a
    /// `notifications/progress` before each step and at the end, and a cancellation stops it
    /// before the next step. Same steps, results and errors as `engine.batch`.
    fn batch_in_steps(&mut self, args: &Value, token: &Value) -> ToolResult {
        let Some(steps) = args.get("commands").and_then(Value::as_array) else {
            return ToolResult::from_reply(self.call("engine.batch", args.clone()));
        };
        let keep_going = args.get("keepGoing").and_then(Value::as_bool).unwrap_or(false);
        let total = steps.len();
        let mut out = Vec::with_capacity(total);
        let (mut first, mut failed) = (None::<String>, 0usize);
        for (i, step) in steps.iter().enumerate() {
            if self.is_cancelled() {
                return ToolResult::error("cancelled");
            }
            let id = step.get("command").and_then(Value::as_str).unwrap_or("?");
            self.progress(token, i, total, &format!("step {} of {total}: {id}", i.saturating_add(1)));
            let entry = match self.call("engine.batch", json!({"commands": [step]})) {
                Reply::Ok(Value::Array(mut v)) if v.len() == 1 => v.remove(0),
                Reply::Ok(other) => json!({"command": id, "ok": false, "error": format!("unexpected batch reply: {other}")}),
                Reply::Err { error, detail } => {
                    let e = detail
                        .and_then(|d| d.get("steps").and_then(|s| s.get(0)).cloned())
                        .unwrap_or_else(|| json!({"command": id, "ok": false, "error": error}));
                    let msg = e.get("error").and_then(Value::as_str).unwrap_or("failed").to_string();
                    first.get_or_insert(format!("step {} (`{id}`) failed: {msg}", i.saturating_add(1)));
                    failed = failed.saturating_add(1);
                    e
                }
            };
            let ok = entry.get("ok").and_then(Value::as_bool).unwrap_or(false);
            out.push(entry);
            if !ok && !keep_going {
                break;
            }
        }
        self.progress(token, total, total, "done");
        ToolResult::from_reply(match first {
            None => Reply::ok(Value::Array(out)),
            Some(f) if keep_going => Reply::err_with(format!("{failed} of {total} steps failed; first: {f}"), json!({"steps": out})),
            Some(f) => Reply::err_with(f, json!({"steps": out})),
        })
    }

    fn progress(&self, token: &Value, done: usize, total: usize, message: &str) {
        if let Some(tx) = &self.notify {
            let n = json!({"jsonrpc": "2.0", "method": "notifications/progress",
                "params": {"progressToken": token, "progress": done, "total": total, "message": message}});
            let _ = tx.send(n.to_string());
        }
    }

    /// What the app offers to read: its identity, its command table, and its UI (when the
    /// backend draws one).
    fn list_resources(&mut self) -> Vec<Value> {
        let res = |uri: &str, name: &str, title: &str, description: &str, mime: &str, priority: f64| {
            json!({"uri": uri, "name": name, "title": title, "description": description, "mimeType": mime,
                "annotations": {"audience": ["assistant"], "priority": priority}})
        };
        let mut list = vec![
            res(RES_APP, "app", "App", "Name, version and the control methods the app answers.", "application/json", 0.5),
            res(
                RES_COMMANDS,
                "commands",
                "Commands",
                "Every command: id, label, enabled now, whether it changes the document.",
                "application/json",
                0.8,
            ),
        ];
        list.extend(self.resources.iter().map(|r| r.def.clone()));
        for (i, d) in self.docs.iter().enumerate() {
            let priority = if i == 0 {
                1.0
            } else if d.section == "skills" {
                0.9
            } else {
                0.7
            };
            list.push(res(&d.uri, &d.name, &d.title, &d.description, "text/markdown", priority));
        }
        if self.methods().iter().any(|m| m == "ui.snapshot") {
            list.push(res(
                RES_UI,
                "ui",
                "UI snapshot",
                "The window as an accessibility tree with element refs (changes as the app is used).",
                "text/plain",
                0.6,
            ));
        }
        list
    }

    fn read_resource(&mut self, params: &Value) -> RpcResult {
        let uri = params.get("uri").and_then(Value::as_str).ok_or((INVALID_PARAMS, "resources/read needs a string `uri`".to_string()))?;
        let failed = |e: String| (INVALID_PARAMS, format!("reading {uri}: {e}"));
        let (mime, text) = match uri {
            RES_APP => match self.call("app.version", json!({})) {
                Reply::Ok(v) => ("application/json", v.to_string()),
                Reply::Err { error, .. } => return Err(failed(error)),
            },
            RES_COMMANDS => match self.commands() {
                Ok(list) => ("application/json", Value::Array(list.iter().map(tools::compact).collect()).to_string()),
                Err(e) => return Err(failed(e)),
            },
            RES_UI => match self.call("ui.snapshot", json!({})) {
                Reply::Ok(v) => ("text/plain", v.get("tree").and_then(Value::as_str).map_or_else(|| v.to_string(), str::to_string)),
                Reply::Err { error, .. } => return Err(failed(error)),
            },
            other => {
                if let Some(d) = self.docs.iter().find(|d| d.uri == other || d.path == other) {
                    let text = match d.read_content() {
                        Ok(t) => t,
                        Err(e) => return Err(failed(e)),
                    };
                    return Ok(json!({"contents": [{"uri": d.uri, "mimeType": "text/markdown", "text": text}]}));
                }
                match self.resources.iter_mut().find(|r| r.def["uri"] == other) {
                    Some(r) => {
                        let mut cx = ToolCx::new(&mut self.backend, &self.app, None, None, false);
                        let mime = r.def["mimeType"].as_str().unwrap_or("text/plain").to_string();
                        match (r.read)(&mut cx) {
                            Ok(text) => return Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": text}]})),
                            Err(e) => return Err(failed(e)),
                        }
                    }
                    None => return Err((RESOURCE_NOT_FOUND, format!("no resource {uri}; see resources/list"))),
                }
            }
        };
        Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": text}]}))
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

/// The `_meta.progressToken` of a request, when it is a string or an integer.
fn progress_token(params: &Value) -> Option<&Value> {
    params.get("_meta").and_then(|m| m.get("progressToken")).filter(|t| t.is_string() || t.is_i64() || t.is_u64())
}

const RES_APP: &str = "app://info";
const RES_COMMANDS: &str = "app://commands";
const RES_UI: &str = "app://ui/snapshot";

/// Read lines until `input` ends. `ping` is answered here (also while a request runs) and a
/// `notifications/cancelled` recorded; everything else goes to the server, in order.
fn read_lines(mut input: impl BufRead, to_server: &Sender<std::io::Result<String>>, direct: &Sender<String>, cancelled: &Mutex<HashSet<String>>) {
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let limit = u64::try_from(MAX_MESSAGE).unwrap_or(u64::MAX).saturating_add(1);
        let n = match std::io::Read::take(&mut input, limit).read_until(b'\n', &mut buf) {
            Ok(n) => n,
            Err(e) => {
                let _ = to_server.send(Err(e));
                return;
            }
        };
        if n == 0 {
            return;
        }
        if buf.len() > MAX_MESSAGE && buf.last() != Some(&b'\n') {
            if let Err(e) = skip_line(&mut input) {
                let _ = to_server.send(Err(e));
                return;
            }
            let _ = direct.send(error(Value::Null, PARSE_ERROR, format!("message longer than {MAX_MESSAGE} bytes")).to_string());
            continue;
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            let _ = direct.send(error(Value::Null, PARSE_ERROR, "parse error: not UTF-8").to_string());
            continue;
        };
        if take_out_of_band(line, direct, cancelled) {
            continue;
        }
        if to_server.send(Ok(line.to_string())).is_err() {
            return;
        }
    }
}

/// Answer a `ping` or record a cancellation right away; whether the line was one of them.
fn take_out_of_band(line: &str, direct: &Sender<String>, cancelled: &Mutex<HashSet<String>>) -> bool {
    // cheap test first: most lines are neither
    if !line.contains("ping") && !line.contains("notifications/cancelled") {
        return false;
    }
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(line) else { return false };
    if o.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return false;
    }
    match o.get("method").and_then(Value::as_str) {
        Some("ping") => match o.get("id") {
            Some(id) if id.is_string() || id.is_i64() || id.is_u64() => {
                let _ = direct.send(response(id.clone(), json!({})).to_string());
                true
            }
            _ => false,
        },
        Some("notifications/cancelled") if !o.contains_key("id") => {
            if let Some(id) = o.get("params").and_then(|p| p.get("requestId")) {
                let mut set = cancelled.lock().unwrap_or_else(PoisonError::into_inner);
                if set.len() >= MAX_CANCELLED {
                    set.clear();
                }
                set.insert(id.to_string());
            }
            true
        }
        _ => false,
    }
}

/// Write each line as it comes, until every sender is gone.
fn write_lines(mut output: impl Write, lines: std::sync::mpsc::Receiver<String>) -> std::io::Result<()> {
    for line in lines {
        output.write_all(line.as_bytes())?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
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
