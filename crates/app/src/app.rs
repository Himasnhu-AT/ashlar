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
/// Changes the window's builder before the window opens (see [`App::viewport`]).
pub type ViewportFn = Box<dyn FnOnce(egui::ViewportBuilder) -> egui::ViewportBuilder>;

/// What only the window's creation needs (taken out of the [`App`] before it is split).
#[derive(Default)]
pub(crate) struct WindowSpec {
    pub viewport: Option<ViewportFn>,
    /// PNG bytes for the window and taskbar icon.
    pub icon: Option<Vec<u8>>,
}
/// A named starting state ([`App::scene`]).
pub type SceneFn<D> = Arc<dyn Fn(&mut Session<D>) + Send + Sync>;

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
    /// Frames the GPU may queue ahead of the screen (see [`App::frame_latency`]).
    pub frame_latency: u32,
    pub control_port: Option<u16>,
    pub follow_system_accent: bool,
    pub motion: Option<ashlar_motion::MotionPrefs>,
    pub single_instance: bool,
    pub accept_drops: bool,
    /// Run toast actions whose command is registered (see [`App::run_toast_actions`]).
    pub toast_commands: bool,
    /// Save the window's placement and restore it on launch (see [`App::remember_window`]).
    pub remember_window: bool,
    /// The tray icon to show (see [`App::tray`]).
    #[cfg(feature = "tray")]
    pub tray: Option<ashlar_native::tray::TraySpec>,
    /// System-wide shortcuts: (command id, chord text) (see [`App::global_shortcut`]).
    #[cfg(feature = "hotkey")]
    pub global_shortcuts: Vec<(String, String)>,
    /// Named starting states, in declaration order.
    pub scenes: Vec<(String, SceneFn<D>)>,
    /// The web page's `<canvas>` id (see [`App::canvas_id`]).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub canvas_id: Option<String>,
}

impl<D: Document> Shared<D> {
    /// The scene called `name`.
    pub(crate) fn scene(&self, name: &str) -> Option<&SceneFn<D>> {
        self.scenes.iter().find(|(n, _)| n == name).map(|(_, f)| f)
    }

    /// Every scene's name, in declaration order.
    pub(crate) fn scene_names(&self) -> Vec<String> {
        self.scenes.iter().map(|(n, _)| n.clone()).collect()
    }
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
    telemetry_honors_dnt: bool,
    instructions: String,
    dirs: Option<AppDirs>,
    window_size: [f32; 2],
    frame_latency: u32,
    control_port: Option<u16>,
    follow_system_accent: bool,
    motion: Option<ashlar_motion::MotionPrefs>,
    single_instance: bool,
    accept_drops: Option<bool>,
    toast_commands: bool,
    remember_window: bool,
    #[cfg(feature = "tray")]
    tray: Option<ashlar_native::tray::TraySpec>,
    #[cfg(feature = "hotkey")]
    global_shortcuts: Vec<(String, String)>,
    window: WindowSpec,
    scenes: Vec<(String, SceneFn<D>)>,
    canvas_id: Option<String>,
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
            telemetry_honors_dnt: true,
            instructions: String::new(),
            dirs: None,
            window_size: [960.0, 640.0],
            frame_latency: 1,
            control_port: None,
            follow_system_accent: true,
            motion: None,
            single_instance: false,
            accept_drops: None,
            toast_commands: false,
            remember_window: true,
            #[cfg(feature = "tray")]
            tray: None,
            #[cfg(feature = "hotkey")]
            global_shortcuts: Vec::new(),
            window: WindowSpec::default(),
            scenes: Vec::new(),
            canvas_id: None,
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

    /// Telemetry (with the `telemetry` feature): write key and server. On when `endpoint` is set
    /// (no notice, no switches). The user's `DO_NOT_TRACK` turns it off (see
    /// [`App::telemetry_honors_do_not_track`]); `TRACER_DISABLED` in the environment always does.
    pub fn telemetry(mut self, key: impl Into<String>, endpoint: Option<&str>) -> Self {
        self.telemetry = Some(TelemetryConfig { key: key.into(), endpoint: endpoint.map(str::to_string), honor_do_not_track: true });
        self
    }

    /// Whether the user's `DO_NOT_TRACK` turns telemetry off. On by default. An app that passes
    /// `false` must say so plainly in its privacy text: users who set `DO_NOT_TRACK` expect it to
    /// work. `TRACER_DISABLED` is honoured either way.
    pub fn telemetry_honors_do_not_track(mut self, honor: bool) -> Self {
        self.telemetry_honors_dnt = honor;
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

    /// On the web, the id of the `<canvas>` the app runs in (default: the app's name, then
    /// `ashlar`). When the app cannot start (no such canvas, no WebGL), a visible error appears
    /// next to the canvas (or at the top of the page) as well as in the console. Ignored on the
    /// desktop.
    pub fn canvas_id(mut self, id: impl Into<String>) -> Self {
        self.canvas_id = Some(id.into());
        self
    }

    pub fn window_size(mut self, width: f32, height: f32) -> Self {
        self.window_size = [width, height];
        self
    }

    /// Frames the GPU may queue ahead of the screen, 1 to 3 (default 1: the lowest input latency,
    /// and one swapchain image fewer, about 10 MB for a 960×640 window on a Retina screen). Apps
    /// that draw heavy GPU scenes every frame may prefer 2 for smoother throughput.
    pub fn frame_latency(mut self, frames: u32) -> Self {
        self.frame_latency = frames.clamp(1, 3);
        self
    }

    /// Serve the control port on this port (also `--control <port>` and `<APP>_CONTROL_PORT`).
    pub fn control_port(mut self, port: u16) -> Self {
        self.control_port = Some(port);
        self
    }

    /// Recolour the accent (hover, focus, selection) from the OS accent colour (default on).
    /// Off pins the built-in accent. Tokens the app installs itself always win over the OS.
    pub fn follow_system_accent(mut self, follow: bool) -> Self {
        self.follow_system_accent = follow;
        self
    }

    /// Motion settings that win over the OS "reduce motion" setting (an app's own motion
    /// preference). Without this the app follows the OS, at normal speed.
    pub fn motion(mut self, prefs: ashlar_motion::MotionPrefs) -> Self {
        self.motion = Some(prefs);
        self
    }

    /// One window per app (default off). A second launch hands its arguments (the files to open)
    /// to the running one and exits; the running one opens them with `file.open` and comes to the
    /// front. The endpoint is named after the app (see [`App::organization`]); desktop only.
    pub fn single_instance(mut self, on: bool) -> Self {
        self.single_instance = on;
        self
    }

    /// Show a tray / status-bar icon (feature `tray`; desktop only). Choosing one of its menu items
    /// runs the command with that id, under the rules the OS menu bar follows; clicking the icon
    /// shows and focuses the window. It appears after the first frame and goes when the app ends;
    /// where the desktop has no tray it is skipped with a log line.
    #[cfg(feature = "tray")]
    pub fn tray(mut self, spec: ashlar_native::tray::TraySpec) -> Self {
        self.tray = Some(spec);
        self
    }

    /// Run `command` when `chord` (`"Mod+Shift+Space"`, the keymap's syntax) is pressed anywhere,
    /// even while another app has the focus (feature `hotkey`; macOS, Windows, Linux on X11).
    /// The window comes to the front first. A chord without Ctrl, Alt or ⌘, or one another app
    /// holds, is skipped with a log line.
    #[cfg(feature = "hotkey")]
    pub fn global_shortcut(mut self, command: impl Into<String>, chord: impl Into<String>) -> Self {
        self.global_shortcuts.push((command.into(), chord.into()));
        self
    }

    /// Open files dropped on the window (default on when the app sets a [`App::format`] or a
    /// [`App::file_extension`]). The first file with a known extension opens with `file.open`
    /// (a dirty document asks to save first); the others reach the view as
    /// [`Cx::dropped_files`](crate::Cx::dropped_files). Off leaves drops to the view's own input.
    pub fn accept_drops(mut self, on: bool) -> Self {
        self.accept_drops = Some(on);
        self
    }

    /// Run a toast's action when it is clicked (default off). With it on, a clicked
    /// [`ToastAction`](ashlar_ui::toasts::ToastAction) whose `command` is a registered command id
    /// runs like a menu pick, with empty params, after the frame's toasts are drawn; errors become
    /// toasts. Actions naming anything else stay queued for the view's
    /// [`Toasts::take_actions`](ashlar_ui::toasts::Toasts::take_actions).
    pub fn run_toast_actions(mut self, on: bool) -> Self {
        self.toast_commands = on;
        self
    }

    /// Change the window before it opens: a minimum size, decorations, transparency… `f` gets
    /// the builder ashlar made (title, size, app id, the restored placement) and returns it.
    /// Desktop only; the headless driver has no window.
    ///
    /// ```no_run
    /// # use ashlar_app::App;
    /// App::<i64>::new("notes", "0.1.0").viewport(|b| b.with_min_inner_size([480.0, 320.0]));
    /// ```
    pub fn viewport(mut self, f: impl FnOnce(egui::ViewportBuilder) -> egui::ViewportBuilder + 'static) -> Self {
        let previous = self.window.viewport.take();
        self.window.viewport = Some(Box::new(move |b| {
            let b = match previous {
                Some(p) => p(b),
                None => b,
            };
            f(b)
        }));
        self
    }

    /// The window and taskbar icon, as PNG bytes (`include_bytes!("icon.png")`). An image that
    /// does not decode is logged and the default icon stays. Desktop only.
    pub fn icon(mut self, png: impl Into<Vec<u8>>) -> Self {
        self.window.icon = Some(png.into());
        self
    }

    /// Remember where the window was (default on): its position, size, maximized and full
    /// screen state go to the settings under `"window"` when they change and on close, and the
    /// next launch opens the window there, kept on a visible monitor. Ignored without a window
    /// (`--headless`, [`crate::Driver`]) and without an app folder ([`App::dirs`]).
    pub fn remember_window(mut self, on: bool) -> Self {
        self.remember_window = on;
        self
    }

    /// The window's own options, taken out before the app is split.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) fn take_window(&mut self) -> WindowSpec {
        std::mem::take(&mut self.window)
    }

    /// A named starting state: `setup` runs on the session (load a fixture, fill a list, set the
    /// theme…) when the scene is applied, with `--scene NAME` (in a window or `--headless`), the
    /// `ui.scene {name}` control method, the MCP tool `ui_scene` or [`crate::Driver::scene`].
    /// `--list-scenes` prints the names. A name declared twice keeps the first.
    ///
    /// ```no_run
    /// # use ashlar_app::{App, Session};
    /// # #[derive(Clone, Default, serde::Serialize, serde::Deserialize)] struct Doc { notes: Vec<String> }
    /// App::<Doc>::new("notes", "0.1.0").scene("two-notes", |s: &mut Session<Doc>| {
    ///     s.edit("Two notes", |d| d.notes = vec!["a".into(), "b".into()]);
    /// });
    /// ```
    pub fn scene(mut self, name: impl Into<String>, setup: impl Fn(&mut Session<D>) + Send + Sync + 'static) -> Self {
        let name = name.into();
        if self.scenes.iter().any(|(n, _)| *n == name) {
            log::warn!("scene `{name}` is declared twice; the first one wins");
        } else {
            self.scenes.push((name, Arc::new(setup)));
        }
        self
    }

    /// The names of the app's scenes, in declaration order.
    pub fn scene_names(&self) -> Vec<String> {
        self.scenes.iter().map(|(n, _)| n.clone()).collect()
    }

    pub(crate) fn split(self) -> (Arc<Shared<D>>, Option<ViewFn<D>>, Option<SetupFn>) {
        let env_prefix: String = self.name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' }).collect();
        let accept_drops = self.accept_drops.unwrap_or(self.format.is_some() || !self.extensions.is_empty());
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
            telemetry: self.telemetry.map(|t| TelemetryConfig { honor_do_not_track: self.telemetry_honors_dnt, ..t }),
            instructions: self.instructions,
            window_size: self.window_size,
            frame_latency: self.frame_latency,
            control_port: self.control_port,
            follow_system_accent: self.follow_system_accent,
            motion: self.motion,
            single_instance: self.single_instance,
            accept_drops,
            toast_commands: self.toast_commands,
            remember_window: self.remember_window,
            #[cfg(feature = "tray")]
            tray: self.tray,
            #[cfg(feature = "hotkey")]
            global_shortcuts: self.global_shortcuts,
            scenes: self.scenes,
            canvas_id: self.canvas_id,
        };
        (Arc::new(shared), self.view, self.setup)
    }

    /// A session without a window, driving the same commands (tests, scripts).
    pub fn headless(self) -> Headless<D> {
        let (shared, _, _) = self.split();
        Headless::new(shared)
    }

    /// The runtime without a window, for tests and benches: drive it with [`crate::Driver`]. It
    /// ignores the OS preferences (see [`Runtime::set_preferences`]) so runs match on every machine.
    pub fn runtime(self) -> Runtime<D> {
        let (shared, view, setup) = self.split();
        let mut launch = crate::runtime::Launch::default();
        if let (Some(cfg), Some(d)) = (&shared.telemetry, &shared.dirs) {
            launch.telemetry = crate::telemetry::Telemetry::start(cfg, &shared.name, &shared.version, &d.data, true);
        }
        let mut rt = Runtime::new(shared, view, setup, launch);
        // tests and benches see the same tokens and motion on every machine
        rt.set_preferences(crate::runtime::Preferences::default);
        rt
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
    ///
    /// With `--headless` on the command line, run with no window instead: a virtual-clock
    /// [`crate::Driver`] in a temporary folder that writes `--snap out.png` and `--tree tree.json`
    /// (after `--script file.ui` and `--at SECONDS`, at `--size WxH`, `--scale S`, `--theme
    /// light|dark`, `--scene NAME`), then exits with 0 (done), 1 (a step failed) or 2 (bad
    /// arguments). (`--headless` needs the `headless` feature, on by default.) `--scene NAME` also
    /// works with a window; `--list-scenes` prints the scenes.
    pub fn run(self) -> Result<(), Error> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let args: Vec<String> = std::env::args().skip(1).collect();
            if args.iter().any(|a| a == "--list-scenes") {
                for name in self.scene_names() {
                    println!("{name}");
                }
                std::process::exit(0);
            }
            if let Some(name) = scene_arg(&args)
                && !self.scenes.iter().any(|(n, _)| *n == name)
            {
                eprintln!("error: no scene `{name}` (scenes: {})", self.scene_names().join(", "));
                std::process::exit(i32::from(ashlar_debug::Exit::Usage.code()));
            }
        }
        #[cfg(all(feature = "headless", not(target_arch = "wasm32")))]
        {
            let args: Vec<String> = std::env::args().skip(1).collect();
            if crate::headless_run::requested(&args) {
                let code = crate::headless_run::run(self, &args);
                std::process::exit(i32::from(code));
            }
        }
        crate::runtime::run_native(self)
    }
}

/// The scene named by `--scene NAME` (or `--scene=NAME`), if any.
#[cfg(not(target_arch = "wasm32"))]
fn scene_arg(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--scene" {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix("--scene=") {
            return Some(v.to_string());
        }
    }
    None
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
