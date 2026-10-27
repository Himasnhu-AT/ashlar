//! The [`App`] builder, the headless session behind the CLI and MCP, and `run()`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ashlar_command::{CommandError, CommandInfo, CommandSpec, Dispatch, Registry};
use ashlar_fs::AppDirs;
use ashlar_store::Format;
use serde_json::Value;

use crate::builtins;
use crate::runtime::{Cx, Runtime};
use crate::session::{Document, Session};
use crate::telemetry::TelemetryConfig;

/// A view: draws the app's UI each frame.
pub type ViewFn<D> = Box<dyn FnMut(&mut egui::Ui, &mut Cx<'_, D>)>;
/// Runs once with the egui context before the first frame (fonts, tokens).
pub type SetupFn = Box<dyn FnOnce(&egui::Context)>;

/// Why the app could not start.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not open the window: {0}")]
    Window(String),
    /// This process cannot show windows at all (`ashlar_native::display::check`): started by a
    /// service or as SYSTEM on Windows. Nothing was opened; say so and exit.
    #[error("no window can be shown here: {0}")]
    NoDesktop(String),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// What everything built from one [`App`] shares (immutable).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct Shared<D: Document> {
    pub name: String,
    pub version: String,
    pub title: String,
    /// `NOTES` for an app called `notes`: `<PREFIX>_CONTROL_PORT`, `<PREFIX>_LOG`.
    pub env_prefix: String,
    pub registry: Registry<Session<D>>,
    pub format: Arc<Format<D>>,
    pub extensions: Vec<String>,
    pub dirs: Option<AppDirs>,
    pub autosave: Option<Duration>,
    pub telemetry: Option<TelemetryConfig>,
    pub instructions: String,
    pub window_size: [f32; 2],
    pub control_port: Option<u16>,
}

/// An app: name, document type and format, commands, views. `run()` opens the window; the same
/// definition serves the CLI and MCP ([`App::cli`]) and tests ([`App::headless`]).
///
/// ```no_run
/// use ashlar_app::{App, Cx, Session};
/// use ashlar_command::{CommandError, CommandSpec};
///
/// #[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
/// struct Doc { count: i64 }
///
/// fn bump(s: &mut Session<Doc>, _: &serde_json::Value) -> Result<serde_json::Value, CommandError> {
///     s.edit("Bump", |d| d.count += 1);
///     Ok(serde_json::Value::Null)
/// }
///
/// fn main() -> Result<(), ashlar_app::Error> {
///     App::<Doc>::new("counter", "0.1.0")
///         .command(CommandSpec::new("count.bump", "Bump", bump).menu(&["Edit"]).shortcut("Mod+B"))
///         .view(|ui, cx: &mut Cx<'_, Doc>| {
///             ui.label(format!("{}", cx.doc().count));
///             if ui.button("Bump").clicked() {
///                 cx.dispatch("count.bump", serde_json::json!({}));
///             }
///         })
///         .run()
/// }
/// ```
pub struct App<D: Document> {
    name: String,
    version: String,
    title: Option<String>,
    qualifier: String,
    org: String,
    format: Option<Format<D>>,
    extensions: Vec<String>,
    commands: Vec<CommandSpec<Session<D>>>,
    view: Option<ViewFn<D>>,
    setup: Option<SetupFn>,
    autosave: Option<Duration>,
    telemetry: Option<TelemetryConfig>,
    instructions: String,
    dirs: Option<AppDirs>,
    window_size: [f32; 2],
    control_port: Option<u16>,
}

impl<D: Document> App<D> {
    /// An app called `name` (lower case, used for folders, env vars and the CLI).
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        App {
            name: name.into(),
            version: version.into(),
            title: None,
            qualifier: "org".into(),
            org: "ashlar".into(),
            format: None,
            extensions: Vec::new(),
            commands: Vec::new(),
            view: None,
            setup: None,
            autosave: Some(Duration::from_secs(60)),
            telemetry: None,
            instructions: String::new(),
            dirs: None,
            window_size: [960.0, 640.0],
            control_port: None,
        }
    }

    /// Reverse-domain qualifier and organisation for the OS folders (`"com", "Acme"`).
    pub fn organization(mut self, qualifier: impl Into<String>, org: impl Into<String>) -> Self {
        self.qualifier = qualifier.into();
        self.org = org.into();
        self
    }

    /// Window title (default: the name).
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The document's file format, with its migrations.
    pub fn format(mut self, format: Format<D>) -> Self {
        self.format = Some(format);
        self
    }

    /// A file extension offered by Open and Save dialogs (without the dot).
    pub fn file_extension(mut self, ext: impl Into<String>) -> Self {
        self.extensions.push(ext.into());
        self
    }

    /// Add commands (menus follow declaration order).
    pub fn commands(mut self, specs: impl IntoIterator<Item = CommandSpec<Session<D>>>) -> Self {
        self.commands.extend(specs);
        self
    }

    pub fn command(mut self, spec: CommandSpec<Session<D>>) -> Self {
        self.commands.push(spec);
        self
    }

    /// The UI, drawn inside the window's central area each frame.
    pub fn view(mut self, view: impl FnMut(&mut egui::Ui, &mut Cx<'_, D>) + 'static) -> Self {
        self.view = Some(Box::new(view));
        self
    }

    /// Runs once before the first frame (install fonts, tokens).
    pub fn setup(mut self, f: impl FnOnce(&egui::Context) + 'static) -> Self {
        self.setup = Some(Box::new(f));
        self
    }

    /// Autosave interval (`None`: no autosave or recovery). Default 60 s.
    pub fn autosave(mut self, every: Option<Duration>) -> Self {
        self.autosave = every;
        self
    }

    /// Telemetry (with the `telemetry` feature): write key and server. Always on when `endpoint`
    /// is set (no notice, no switches); `TRACER_DISABLED` in the environment turns it off.
    pub fn telemetry(mut self, key: impl Into<String>, endpoint: Option<&str>) -> Self {
        self.telemetry = Some(TelemetryConfig { key: key.into(), endpoint: endpoint.map(str::to_string) });
        self
    }

    /// What MCP clients are told about the app.
    pub fn instructions(mut self, text: impl Into<String>) -> Self {
        self.instructions = text.into();
        self
    }

    /// Put config, data, cache and logs here instead of the OS folders (tests, portable).
    pub fn dirs(mut self, dirs: AppDirs) -> Self {
        self.dirs = Some(dirs);
        self
    }

    pub fn window_size(mut self, width: f32, height: f32) -> Self {
        self.window_size = [width, height];
        self
    }

    /// Serve the control port on this port (also `--control <port>` and `<APP>_CONTROL_PORT`).
    pub fn control_port(mut self, port: u16) -> Self {
        self.control_port = Some(port);
        self
    }

    pub(crate) fn split(self) -> (Arc<Shared<D>>, Option<ViewFn<D>>, Option<SetupFn>) {
        let env_prefix: String = self.name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
        let format = self.format.unwrap_or_else(|| Format::new(format!("{}.document", self.name), format!("{} {}", self.name, self.version)));
        let mut specs = builtins::leading::<D>();
        specs.extend(self.commands);
        specs.extend(builtins::trailing::<D>());
        let registry = Registry::new(specs);
        for dup in registry.duplicates() {
            log::warn!("command `{dup}` is declared twice; the first one wins");
        }
        let dirs = self.dirs.or_else(|| AppDirs::new(&self.qualifier, &self.org, &self.name));
        let shared = Shared {
            title: self.title.unwrap_or_else(|| self.name.clone()),
            name: self.name,
            version: self.version,
            env_prefix,
            registry,
            format: Arc::new(format),
            extensions: self.extensions,
            dirs,
            autosave: self.autosave,
            telemetry: self.telemetry,
            instructions: self.instructions,
            window_size: self.window_size,
            control_port: self.control_port,
        };
        (Arc::new(shared), self.view, self.setup)
    }

    /// A session without a window, driving the same commands (tests, scripts).
    pub fn headless(self) -> Headless<D> {
        let (shared, _, _) = self.split();
        Headless::new(shared)
    }

    /// The runtime without a window, for tests and benches: drive it with [`crate::Driver`].
    pub fn runtime(self) -> Runtime<D> {
        let (shared, view, setup) = self.split();
        let mut launch = crate::runtime::Launch::default();
        if let (Some(cfg), Some(d)) = (&shared.telemetry, &shared.dirs) {
            launch.telemetry = crate::telemetry::Telemetry::start(cfg, &shared.name, &shared.version, &d.data, true);
        }
        Runtime::new(shared, view, setup, launch)
    }

    /// The app's `-cli` binary: `commands`, `exec`, `run`, `mcp`, `serve`, `version`, each on a
    /// fresh headless session (or the running app with `--connect PORT`). Add app verbs to the
    /// returned [`ashlar_cli::Cli`] before `run()`.
    pub fn cli(self) -> ashlar_cli::Cli {
        let (shared, _, _) = self.split();
        let factory = Arc::clone(&shared);
        let mut cli = ashlar_cli::Cli::new(shared.name.clone(), shared.version.clone(), move || {
            Box::new(Headless::new(Arc::clone(&factory))) as Box<dyn Dispatch>
        });
        if let Some(d) = &shared.dirs {
            cli = cli.key_dir(d.config.clone());
        }
        if !shared.instructions.is_empty() {
            cli = cli.instructions(shared.instructions.clone());
        }
        cli
    }

    /// Open the window and run until it closes.
    pub fn run(self) -> Result<(), Error> {
        crate::runtime::run_native(self)
    }
}

/// `fn main() -> ExitCode { ashlar_app::cli_main(my_app::app()) }`: an app's `-cli` binary.
pub fn cli_main<D: Document>(app: App<D>) -> std::process::ExitCode {
    app.cli().run()
}

/// A session of an app without a window: what the CLI, MCP and tests drive. Implements
/// [`Dispatch`]; effects that need a window are dropped (toasts are logged).
pub struct Headless<D: Document> {
    shared: Arc<Shared<D>>,
    session: Session<D>,
}

impl<D: Document> Headless<D> {
    pub(crate) fn new(shared: Arc<Shared<D>>) -> Self {
        let session = Session::new(Arc::clone(&shared.format));
        Headless { shared, session }
    }

    pub fn session(&self) -> &Session<D> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session<D> {
        &mut self.session
    }

    /// The command table (built-ins and the app's).
    pub fn registry(&self) -> &Registry<Session<D>> {
        &self.shared.registry
    }

    /// Where the app keeps its files.
    pub fn dirs(&self) -> Option<&AppDirs> {
        self.shared.dirs.as_ref()
    }

    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// Where an untitled document of this app would be saved by default.
    pub fn default_dir(&self) -> Option<PathBuf> {
        self.shared.dirs.as_ref().map(|d| d.data.clone())
    }
}

impl<D: Document> Dispatch for Headless<D> {
    fn commands(&self) -> Vec<CommandInfo> {
        self.shared.registry.infos(&self.session)
    }

    fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        let r = self.shared.registry.execute(&mut self.session, id, params);
        self.session.drain_headless();
        r
    }
}
