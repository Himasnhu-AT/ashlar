//! One integration's server: started on demand, its tool list cached, restarted after a crash.
//!
//! A server that stops unexpectedly is started again on the next call, unless it crashed
//! [`MAX_CRASHES`] times within [`CRASH_WINDOW`]: then it stays in the error state until it is
//! started by hand (enable, or [`Server::restart`]).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::mcp::{McpClient, McpError, ServerCommand};
use crate::tool::{CallOutput, Tool};

pub const MAX_CRASHES: usize = 3;
pub const CRASH_WINDOW: Duration = Duration::from_secs(300);

/// How a server is doing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServerStatus {
    Stopped,
    Starting,
    Running { pid: Option<u32>, tools: usize },
    Error { message: String },
}

/// How to run one integration's server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub id: String,
    pub command: ServerCommand,
    pub startup_timeout: Duration,
    pub call_timeout: Duration,
    /// Tools never offered (by the server's own name).
    pub hidden: Vec<String>,
    pub client_name: String,
    pub client_version: String,
}

enum State {
    Stopped,
    Starting,
    Running { client: Arc<McpClient>, tools: Arc<Vec<Tool>> },
    Error { message: String, retry: bool },
}

/// A supervised server. Every method may be called from any thread.
pub struct Server {
    config: ServerConfig,
    state: Mutex<State>,
    crashes: Mutex<Vec<Instant>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("id", &self.config.id).field("status", &self.status()).finish()
    }
}

impl Server {
    pub fn new(config: ServerConfig) -> Server {
        Server { config, state: Mutex::new(State::Stopped), crashes: Mutex::new(Vec::new()) }
    }

    pub fn id(&self) -> &str {
        &self.config.id
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub fn status(&self) -> ServerStatus {
        match &*self.state.lock().unwrap_or_else(PoisonError::into_inner) {
            State::Stopped => ServerStatus::Stopped,
            State::Starting => ServerStatus::Starting,
            State::Running { client, tools } => {
                if client.is_alive() {
                    ServerStatus::Running { pid: client.pid(), tools: tools.len() }
                } else {
                    ServerStatus::Error { message: McpError::Exited(client.exit_reason()).to_string() }
                }
            }
            State::Error { message, .. } => ServerStatus::Error { message: message.clone() },
        }
    }

    /// Start it unless it's running (blocks for the handshake and the tool list).
    pub fn start(&self) -> Result<(), String> {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            match &*state {
                State::Running { client, .. } if client.is_alive() => return Ok(()),
                State::Starting => return Err("still starting".into()),
                _ => *state = State::Starting,
            }
        }
        let started = (|| {
            let client = McpClient::start(&self.config.command, &self.config.client_name, &self.config.client_version, self.config.startup_timeout)
                .map_err(|e| e.to_string())?;
            let tools = client.list_tools(self.config.startup_timeout).map_err(|e| e.to_string())?;
            let tools: Vec<Tool> = tools.into_iter().filter(|t| !self.config.hidden.contains(&t.name)).collect();
            Ok::<_, String>((client, tools))
        })();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match started {
            Ok((client, tools)) => {
                log::info!(target: "integrations", "[{}] running: {} {} with {} tools", self.config.id, client.info().name, client.info().version, tools.len());
                *state = State::Running { client: Arc::new(client), tools: Arc::new(tools) };
                Ok(())
            }
            Err(message) => {
                log::warn!(target: "integrations", "[{}] couldn't start: {message}", self.config.id);
                *state = State::Error { message: message.clone(), retry: false };
                Err(message)
            }
        }
    }

    /// Stop it (closing stdin, then killing it).
    pub fn stop(&self) {
        let old = std::mem::replace(&mut *self.state.lock().unwrap_or_else(PoisonError::into_inner), State::Stopped);
        if let State::Running { client, .. } = old {
            client.shutdown(Duration::from_secs(2));
        }
    }

    /// Stop, forget the crash count, start.
    pub fn restart(&self) -> Result<(), String> {
        self.stop();
        self.crashes.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.start()
    }

    /// The running client, starting or restarting the server when that is allowed.
    fn client(&self) -> Result<(Arc<McpClient>, Arc<Vec<Tool>>), String> {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            match &*state {
                State::Running { client, tools } if client.is_alive() => {
                    if client.take_tools_changed() {
                        let client = Arc::clone(client);
                        drop(state);
                        return self.refresh_tools(client);
                    }
                    return Ok((Arc::clone(client), Arc::clone(tools)));
                }
                State::Running { client, .. } => {
                    let message = McpError::Exited(client.exit_reason()).to_string();
                    let retry = self.note_crash();
                    log::warn!(target: "integrations", "[{}] {message}", self.config.id);
                    *state = State::Error { message, retry };
                }
                State::Starting => return Err("the server is starting".into()),
                State::Stopped | State::Error { .. } => {}
            }
            match &*state {
                State::Error { message, retry: false } => return Err(message.clone()),
                State::Stopped => return Err("the server isn't running".into()),
                _ => {}
            }
        }
        self.start()?;
        match &*self.state.lock().unwrap_or_else(PoisonError::into_inner) {
            State::Running { client, tools } => Ok((Arc::clone(client), Arc::clone(tools))),
            _ => Err("the server isn't running".into()),
        }
    }

    fn refresh_tools(&self, client: Arc<McpClient>) -> Result<(Arc<McpClient>, Arc<Vec<Tool>>), String> {
        let tools = client.list_tools(self.config.startup_timeout).map_err(|e| e.to_string())?;
        let tools = Arc::new(tools.into_iter().filter(|t| !self.config.hidden.contains(&t.name)).collect::<Vec<_>>());
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let State::Running { client: c, tools: t } = &mut *state
            && Arc::ptr_eq(c, &client)
        {
            *t = Arc::clone(&tools);
        }
        Ok((client, tools))
    }

    /// Count a crash; true when another restart is allowed.
    fn note_crash(&self) -> bool {
        let mut crashes = self.crashes.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        crashes.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
        crashes.push(now);
        crashes.len() < MAX_CRASHES
    }

    /// The tools of the running server (none when it isn't running).
    pub fn tools(&self) -> Vec<Tool> {
        match &*self.state.lock().unwrap_or_else(PoisonError::into_inner) {
            State::Running { client, tools } if client.is_alive() => tools.as_ref().clone(),
            _ => Vec::new(),
        }
    }

    /// Call one of its tools by the server's own name.
    pub fn call(&self, tool: &str, input: &Value, cancel: Option<&AtomicBool>) -> CallOutput {
        if self.config.hidden.iter().any(|h| h == tool) {
            return CallOutput::error(format!("{tool} isn't available"));
        }
        let (client, tools) = match self.client() {
            Ok(c) => c,
            Err(e) => return CallOutput::error(format!("{} isn't available: {e}", self.config.id)),
        };
        if !tools.iter().any(|t| t.name == tool) {
            return CallOutput::error(format!("{} has no tool {tool:?}", self.config.id));
        }
        match client.call_tool(tool, input, self.config.call_timeout, cancel) {
            Ok(out) => out,
            Err(e) => {
                if e.is_exit() {
                    let retry = self.note_crash();
                    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if matches!(&*state, State::Running { client: c, .. } if Arc::ptr_eq(c, &client)) {
                        *state = State::Error { message: e.to_string(), retry };
                    }
                    log::warn!(target: "integrations", "[{}] {tool}: {e}", self.config.id);
                    let again = if retry { " It restarts on the next call." } else { "" };
                    return CallOutput::error(format!("{}: {e}.{again}", self.config.id));
                }
                CallOutput::error(format!("{tool}: {e}"))
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}
