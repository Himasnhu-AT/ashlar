//! The runtime: one frame of an app (control requests, jobs, autosave, menus, keys, the view,
//! dialogs), the repaint policy, and the eframe glue.
//!
//! Repaints happen only for: input (egui/eframe), a store revision change made during a frame, a
//! job's wake, a control request, toasts and egui animations, injected agent input, and pending
//! work that asks for its own frame (settings and autosave wake hooks when a save is due, the
//! autosave writer when a write finished). Every request goes through [`crate::wake`] with a
//! reason (`app.stats` → `wakes`). An idle app, clean or dirty, requests nothing and draws no
//! frames.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use ashlar_command::shortcut::{Platform, format_list};
use ashlar_command::{CommandError, CommandInfo, Keymap, Registry};
use ashlar_control::{AppInfo, Reply, Request, Server};
use ashlar_fs::{ConfigStore, DirConfig, MemoryConfig, Settings};
use ashlar_jobs::Jobs;
use ashlar_store::{Autosave, AutosaveConfig, RecoveryCandidate, Revision};
use ashlar_ui::automation;
use ashlar_ui::commands::{CommandPalette, KeymapChange, KeymapEditor, MenuSpec, dispatch, menu_bar};
use ashlar_ui::devtools;
use ashlar_ui::dialog::{Dialog, DialogButton, DialogOutcome};
use ashlar_ui::toasts::Toasts;
use ashlar_ui::tokens::{self, ThemeMode, Tokens};
use ashlar_ui::wake::{self, Reason, Scheduler, WakeStats};
use serde_json::{Value, json};

use crate::app::{App, Error, SetupFn, Shared, ViewFn};
use crate::control::UiQueue;
use crate::crash::{self, LaunchGuard, PreviousLaunch};
use crate::session::{Document, Effect, Session, now};
use crate::stats::FrameStats;
use crate::telemetry::Telemetry;

/// Top-level menus in this order; menus the app adds follow in first-seen order.
pub const MENU_ORDER: &[&str] = &["File", "Edit", "View"];

/// What `run()` found out before the window opened.
#[derive(Default)]
pub struct Launch {
    /// Start without restoring view settings (after a crash during startup, or `--safe-mode`).
    pub safe_mode: bool,
    pub previous: Option<PreviousLaunch>,
    /// A file named on the command line.
    pub open: Option<PathBuf>,
    pub control_port: Option<u16>,
    pub(crate) guard: Option<LaunchGuard>,
    pub(crate) telemetry: Telemetry,
}

/// Which file dialog a command asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Open,
    SaveAs,
}

/// Picks a file for [`Pick`]; `None` = cancelled. The default shows the native dialog.
pub type FilePicker = Box<dyn FnMut(Pick) -> Option<PathBuf>>;

/// What the view gets each frame: the session, commands, jobs, toasts and settings.
pub struct Cx<'a, D: Document> {
    session: &'a mut Session<D>,
    registry: &'a Registry<Session<D>>,
    /// Background work; a job's apply runs on the session (each edit there is an undo step).
    pub jobs: &'a mut Jobs<Session<D>>,
    pub toasts: &'a mut Toasts,
    /// App settings (debounced, saved in the config folder).
    pub settings: &'a mut Settings,
    queued: &'a mut Vec<(String, Value)>,
    ctx: egui::Context,
}

impl<D: Document> Cx<'_, D> {
    /// The current document.
    pub fn doc(&self) -> &D {
        self.session.doc()
    }

    /// The current document as an `Arc` (to keep while calling `&mut self` methods).
    pub fn snapshot(&self) -> Arc<D> {
        self.session.snapshot()
    }

    /// The revision of the current document (key caches by it).
    pub fn revision(&self) -> Revision {
        self.session.revision()
    }

    pub fn session(&self) -> &Session<D> {
        self.session
    }

    pub fn session_mut(&mut self) -> &mut Session<D> {
        self.session
    }

    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// Run a command after the view returns (menus, buttons). Errors are shown as toasts.
    pub fn dispatch(&mut self, id: &str, params: Value) {
        self.queued.push((id.to_string(), params));
    }

    /// Run a command now and get its result.
    pub fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        self.registry.execute(self.session, id, params)
    }

    /// An undoable change made directly by the view (one step called `label`).
    pub fn edit<R>(&mut self, label: &str, f: impl FnOnce(&mut D) -> R) -> R {
        self.session.edit(label, f)
    }

    /// An undoable change folded into the previous one with the same label and key (a drag).
    pub fn edit_merged<R>(&mut self, label: &str, merge_key: &str, f: impl FnOnce(&mut D) -> R) -> R {
        self.session.edit_merged(label, merge_key, f)
    }

    /// View state (not in the document; `ui.set` changes it too).
    pub fn view<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.session.view(key)
    }

    pub fn view_or<T: serde::de::DeserializeOwned>(&self, key: &str, default: T) -> T {
        self.session.view_or(key, default)
    }

    pub fn set_view<T: serde::Serialize + ?Sized>(&mut self, key: &str, value: &T) {
        self.session.set_view(key, value);
    }
}

#[derive(Default)]
struct InfoCache {
    key: Option<(Revision, u64, u64, u64)>,
    seq: u64,
    infos: Vec<CommandInfo>,
    menu: MenuSpec,
}

enum Modal {
    Recovery,
    SafeMode,
    Consent([bool; 3]),
    Confirm { quit: bool, command: Option<(String, Value)> },
}

struct AutosaveState {
    autosave: Autosave,
    #[cfg(not(target_arch = "wasm32"))]
    writer: Option<ashlar_store::BackgroundWriter>,
    file_gen: u64,
}

/// One app instance's frame loop. Built by [`App::runtime`] (tests, benches) or `App::run`.
pub struct Runtime<D: Document> {
    shared: Arc<Shared<D>>,
    session: Session<D>,
    jobs: Jobs<Session<D>>,
    toasts: Toasts,
    settings: Settings,
    view: Option<ViewFn<D>>,
    setup: Option<SetupFn>,
    keymap: Keymap,
    keymap_gen: u64,
    palette: CommandPalette,
    keymap_editor: KeymapEditor,
    keymap_open: bool,
    cache: InfoCache,
    control: Option<(Server, Receiver<Request>)>,
    injected_requests: Vec<Request>,
    autosave: Option<AutosaveState>,
    recovery: Vec<RecoveryCandidate>,
    modals: Vec<Modal>,
    stats: FrameStats,
    wakes: Scheduler,
    launch: Launch,
    queued: Vec<(String, Value)>,
    created: Option<std::time::Instant>,
    started: bool,
    first_frame_done: bool,
    view_error: Option<String>,
    allow_close: bool,
    quit_after_save: bool,
    shown_title: String,
    picker: Option<FilePicker>,
    passes: u64,
    /// Running in a real window (not the headless driver): OS menus may be installed.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub(crate) native: bool,
    #[cfg(all(feature = "native-menu", any(target_os = "macos", target_os = "windows")))]
    native_menu: Option<crate::native_menu::NativeMenu>,
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown error".into())
}

impl<D: Document> Runtime<D> {
    pub(crate) fn new(shared: Arc<Shared<D>>, view: Option<ViewFn<D>>, setup: Option<SetupFn>, launch: Launch) -> Self {
        let mut session = Session::new(Arc::clone(&shared.format));
        session.set_windowed(true);
        session.telemetry = launch.telemetry.clone();
        let config: Arc<dyn ConfigStore> = match &shared.dirs {
            Some(d) => Arc::new(DirConfig::new(d.config.clone())),
            None => Arc::new(MemoryConfig::new()),
        };
        let settings = Settings::load(config, "settings.json", &[]);
        let mut keymap = Keymap::from_registry(&shared.registry, Platform::current());
        if let Some(o) = settings.get_value("keymap") {
            keymap.apply_overrides_json(o);
        }
        if !launch.safe_mode
            && let Some(Value::Object(view)) = settings.get_value("view")
        {
            for (k, v) in view {
                session.set_view_value(k, v.clone());
            }
        }
        let mut rt = Runtime {
            session,
            jobs: Jobs::new(),
            toasts: Toasts::new(),
            settings,
            view,
            setup,
            keymap,
            keymap_gen: 0,
            palette: CommandPalette::new(),
            keymap_editor: KeymapEditor::new(),
            keymap_open: false,
            cache: InfoCache::default(),
            control: None,
            injected_requests: Vec::new(),
            autosave: None,
            recovery: Vec::new(),
            modals: Vec::new(),
            stats: FrameStats::default(),
            wakes: Scheduler::default(),
            queued: Vec::new(),
            created: now(),
            started: false,
            first_frame_done: false,
            view_error: None,
            allow_close: false,
            quit_after_save: false,
            shown_title: String::new(),
            picker: None,
            passes: 0,
            native: false,
            #[cfg(all(feature = "native-menu", any(target_os = "macos", target_os = "windows")))]
            native_menu: None,
            launch,
            shared,
        };
        rt.start_autosave();
        if let Some(path) = rt.launch.open.take() {
            let r = rt.execute("file.open", json!({"path": path.to_string_lossy(), "discard": true}));
            if let Err(e) = r {
                rt.toasts.error(e.to_string());
            }
        }
        if rt.launch.safe_mode {
            rt.modals.push(Modal::SafeMode);
        }
        if rt.launch.telemetry.notice_pending() {
            rt.modals.push(Modal::Consent([true, true, true]));
        }
        rt
    }

    fn start_autosave(&mut self) {
        let (Some(every), Some(dirs)) = (self.shared.autosave, &self.shared.dirs) else { return };
        let mut cfg = AutosaveConfig::new(dirs.data.join("Recovery"));
        cfg.interval = every;
        match Autosave::start(cfg) {
            Ok((autosave, candidates)) => {
                self.recovery = candidates;
                if !self.recovery.is_empty() {
                    self.modals.insert(0, Modal::Recovery);
                }
                self.autosave = Some(AutosaveState {
                    autosave,
                    #[cfg(not(target_arch = "wasm32"))]
                    writer: ashlar_store::BackgroundWriter::start().ok(),
                    file_gen: u64::MAX,
                });
            }
            Err(e) => log::warn!("autosave is off: {e}"),
        }
    }

    // ------------------------------------------------------------------ public API

    pub fn session(&self) -> &Session<D> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session<D> {
        &mut self.session
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn toasts(&self) -> &Toasts {
        &self.toasts
    }

    /// Frame-time statistics (`app.stats`).
    pub fn stats(&self) -> &FrameStats {
        &self.stats
    }

    pub fn stats_mut(&mut self) -> &mut FrameStats {
        &mut self.stats
    }

    /// Frames and wake requests per [`Reason`] (`app.stats` → `wakes`).
    pub fn wakes(&self) -> WakeStats {
        self.wakes.stats()
    }

    /// Passes run so far (frames, plus egui's occasional extra layout pass).
    pub fn passes(&self) -> u64 {
        self.passes
    }

    /// Replace the native file dialogs (tests, scripted runs).
    pub fn set_file_picker(&mut self, picker: impl FnMut(Pick) -> Option<PathBuf> + 'static) {
        self.picker = Some(Box::new(picker));
    }

    /// Run a command through the chokepoint now.
    pub fn execute(&mut self, id: &str, params: Value) -> Result<Value, CommandError> {
        self.shared.registry.execute(&mut self.session, id, params)
    }

    /// Hand a control request to the runtime as if it came from the control port (tests).
    pub fn inject_request(&mut self, req: Request) {
        self.injected_requests.push(req);
    }

    /// The control port, when one is serving.
    pub fn control_port(&self) -> Option<u16> {
        self.control.as_ref().map(|(s, _)| s.port())
    }

    /// Whether the app has decided to close (the window should go away).
    pub fn is_closing(&self) -> bool {
        self.allow_close
    }

    /// The error that suspended the view, if one did.
    pub fn view_error(&self) -> Option<&str> {
        self.view_error.as_deref()
    }

    fn info(&self) -> AppInfo {
        AppInfo::new(self.shared.name.clone(), self.shared.version.clone())
    }

    // ------------------------------------------------------------------ start

    fn ensure_started(&mut self, ctx: &egui::Context) {
        if self.started {
            return;
        }
        self.started = true;
        devtools::install(ctx);
        let mode = if self.launch.safe_mode { ThemeMode::System } else { self.settings.get::<ThemeMode>("theme").unwrap_or_default() };
        tokens::install(ctx, &Tokens::light(), &Tokens::dark(), mode);
        if let Some(setup) = self.setup.take()
            && let Err(p) = catch_unwind(AssertUnwindSafe(|| setup(ctx)))
        {
            log::error!("app setup panicked: {}", panic_text(p.as_ref()));
        }
        let w = wake::waker(ctx, Reason::JOB);
        self.jobs.set_wake(move || w.wake());
        let w = wake::waker(ctx, Reason::SETTINGS);
        self.settings.set_wake(move |d| w.reschedule(d));
        if let Some(a) = &mut self.autosave {
            let w = wake::waker(ctx, Reason::AUTOSAVE);
            a.autosave.set_wake(move |d| w.reschedule(d));
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(writer) = &a.writer {
                let w = wake::waker(ctx, Reason::AUTOSAVE);
                writer.set_wake(move || w.wake());
            }
        }
        let port = self.launch.control_port.or(self.shared.control_port);
        // A launcher may say where the key goes (`ashlar-debug open`); otherwise the settings folder.
        let key_dir = ashlar_control::key::key_dir_from_env().or_else(|| self.shared.dirs.as_ref().map(|d| d.config.clone()));
        if let (Some(port), Some(key_dir)) = (port, key_dir) {
            let w = wake::waker(ctx, Reason::CONTROL);
            match Server::start(ashlar_control::Config::new(port, key_dir), move || w.wake()) {
                Ok((server, rx)) => {
                    self.control = Some((server, rx));
                }
                Err(e) => {
                    log::error!("control port {port}: {e}");
                    self.toasts.error(format!("Agent control is off: {e}"));
                }
            }
        }
    }

    // ------------------------------------------------------------------ the frame

    /// Work that needs no drawing: control requests, finished jobs, effects, autosave, settings.
    /// eframe calls it before every frame (and while the window is hidden).
    pub fn logic(&mut self, ctx: &egui::Context) {
        self.launch.telemetry.begin_turn();
        self.ensure_started(ctx);
        self.serve_control(ctx);
        for rec in self.jobs.drain_ready(&mut self.session) {
            if rec.status == ashlar_jobs::Status::Failed {
                self.toasts.error(format!("{}: {}", rec.label, rec.message));
            }
        }
        self.apply_effects(ctx);
        #[cfg(all(feature = "native-menu", any(target_os = "macos", target_os = "windows")))]
        for id in self.native_menu.as_ref().map(|m| m.drain()).unwrap_or_default() {
            self.run_ui_command(ctx, &id, json!({}));
        }
        self.launch.telemetry.end_turn();
    }

    /// The start of a frame (eframe's `raw_input_hook`, before [`Runtime::logic`]): credit the
    /// frame to the wake reasons that were due. (Injected agent input is handed to egui by the
    /// debugger's plugin.)
    pub fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // installed every frame: an app may reset egui's memory, which holds the scheduler
        self.wakes.install(ctx);
        self.wakes.begin_frame(raw.time.unwrap_or_else(|| ctx.input(|i| i.time)));
    }

    /// Draw the frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let started = now();
        self.launch.telemetry.begin_turn();
        self.ensure_started(&ctx);
        self.passes = self.passes.saturating_add(1);
        // timed wakes not yet due when this frame ran (input came first) stay scheduled
        self.wakes.rearm(&ctx);
        let rev_before = self.session.revision();
        let (focused, active) = ctx.input(|i| (i.focused, !i.events.is_empty()));
        self.launch.telemetry.input(focused, active);

        self.refresh_infos();
        if !self.modal_open()
            && let Some(id) = dispatch(&ctx, &self.keymap)
        {
            self.run_ui_command(&ctx, &id, json!({}));
        }
        self.update_title(&ctx);
        if !self.native_menu_active() {
            let spec = &self.cache.menu;
            let chosen = egui::Panel::top("ashlar.menu_bar").show(ui, |ui| menu_bar(ui, spec, Platform::current())).inner;
            if let Some(id) = chosen {
                self.run_ui_command(&ctx, &id, json!({}));
            }
        }
        self.draw_view(ui);
        for (id, params) in std::mem::take(&mut self.queued) {
            self.run_ui_command(&ctx, &id, params);
        }
        self.apply_effects(&ctx);
        self.draw_modals(&ctx);
        self.draw_tools(&ctx);
        self.toasts.show(&ctx);
        self.handle_close(&ctx);
        self.apply_effects(&ctx);
        self.persist_view();
        self.tick_storage(&ctx);
        self.feed_devtools(&ctx);

        // the frame changed the document: draw once more so everything shows the new state
        if self.session.revision() != rev_before {
            wake::wake(&ctx, Reason::DOCUMENT);
        }
        if let Some(s) = started {
            let took = s.elapsed();
            self.stats.record(took);
            self.launch.telemetry.frame(took);
        }
        if !self.first_frame_done {
            self.first_frame_done = true;
            if let Some(g) = &mut self.launch.guard {
                g.started();
            }
            if let Some(c) = self.created {
                self.stats.set_first_frame(c.elapsed());
            }
            self.launch.telemetry.first_frame();
            self.install_native_menu(&ctx);
        }
        self.launch.telemetry.end_turn();
    }

    fn draw_view(&mut self, ui: &mut egui::Ui) {
        let Runtime { session, shared, jobs, toasts, settings, queued, view, view_error, .. } = self;
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(err) = view_error.as_deref() {
                ui.centered_and_justified(|ui| ui.label(format!("This view stopped after an internal error:\n{err}")));
                return;
            }
            let Some(view) = view.as_mut() else { return };
            let mut cx = Cx { session, registry: &shared.registry, jobs, toasts, settings, queued, ctx: ui.ctx().clone() };
            let r = catch_unwind(AssertUnwindSafe(|| view(ui, &mut cx)));
            if let Err(p) = r {
                let msg = crash::last_panic().map_or_else(|| panic_text(p.as_ref()), |r| format!("{} ({})", r.message, r.location));
                log::error!("the view panicked: {msg}");
                *view_error = Some(msg);
            }
        });
    }

    fn native_menu_active(&self) -> bool {
        #[cfg(all(feature = "native-menu", any(target_os = "macos", target_os = "windows")))]
        {
            self.native_menu.is_some()
        }
        #[cfg(not(all(feature = "native-menu", any(target_os = "macos", target_os = "windows"))))]
        {
            false
        }
    }

    fn install_native_menu(&mut self, ctx: &egui::Context) {
        #[cfg(all(feature = "native-menu", any(target_os = "macos", target_os = "windows")))]
        if self.native {
            self.native_menu = crate::native_menu::NativeMenu::install(&self.cache.infos, ctx);
        }
        let _ = ctx;
    }

    fn modal_open(&self) -> bool {
        !self.modals.is_empty() || self.palette.is_open() || self.view_error.is_some()
    }

    // ------------------------------------------------------------------ commands

    fn refresh_infos(&mut self) {
        let key = (self.session.revision(), self.session.view_generation(), self.session.file_generation(), self.keymap_gen);
        if self.cache.key == Some(key) {
            return;
        }
        let mut infos = self.shared.registry.infos(&self.session);
        for info in &mut infos {
            let keys = self.keymap.keys_for(&info.id);
            info.shortcut = if keys.is_empty() { None } else { Some(format_list(keys)) };
        }
        self.cache.menu = MenuSpec::from_commands(&infos, MENU_ORDER);
        self.cache.infos = infos;
        self.cache.key = Some(key);
        self.cache.seq = self.cache.seq.wrapping_add(1);
        #[cfg(all(feature = "native-menu", any(target_os = "macos", target_os = "windows")))]
        if let Some(m) = &mut self.native_menu {
            m.update(&self.cache.infos);
        }
    }

    /// Run a command the user chose (menu, key, palette, button): errors become toasts.
    fn run_ui_command(&mut self, ctx: &egui::Context, id: &str, params: Value) {
        if let Err(e) = self.execute(id, params) {
            match e {
                CommandError::Disabled { .. } => log::debug!("{e}"),
                _ => {
                    self.toasts.error(e.to_string());
                }
            }
        }
        self.apply_effects(ctx);
    }

    fn apply_effects(&mut self, ctx: &egui::Context) {
        // effects can run commands that request more effects: bounded rounds
        for _ in 0..8 {
            let effects = self.session.take_effects();
            if effects.is_empty() {
                return;
            }
            for e in effects {
                self.apply_effect(ctx, e);
            }
        }
    }

    fn apply_effect(&mut self, ctx: &egui::Context, e: Effect) {
        match e {
            Effect::Toast { level, text } => {
                self.toasts.push(ashlar_ui::toasts::Toast::new(level, text));
            }
            Effect::OpenPalette => self.palette.open(),
            Effect::OpenKeymap => self.keymap_open = true,
            Effect::Theme(mode) => {
                tokens::set_mode(ctx, mode);
                let _ = self.settings.set("theme", &mode);
            }
            Effect::PickOpen => {
                if let Some(path) = self.pick(Pick::Open) {
                    let r = self.execute("file.open", json!({"path": path.to_string_lossy(), "discard": true}));
                    self.report(r);
                }
            }
            Effect::PickSaveAs => match self.pick(Pick::SaveAs) {
                Some(path) => {
                    let r = self.execute("file.save_as", json!({"path": path.to_string_lossy()}));
                    self.report(r);
                    self.continue_quit(ctx);
                }
                None => self.quit_after_save = false,
            },
            Effect::ConfirmDiscard { command, params } => self.modals.push(Modal::Confirm { quit: false, command: Some((command, params)) }),
            Effect::Quit { discard } => {
                if self.session.is_dirty() && !discard {
                    self.modals.push(Modal::Confirm { quit: true, command: None });
                } else {
                    self.close(ctx, discard);
                }
            }
        }
    }

    fn report(&mut self, r: Result<Value, CommandError>) {
        if let Err(e) = r {
            self.toasts.error(e.to_string());
        }
    }

    fn pick(&mut self, kind: Pick) -> Option<PathBuf> {
        if let Some(p) = &mut self.picker {
            return p(kind);
        }
        self.launch.telemetry.pause_hangs();
        let r = native_pick(&self.shared, &self.session, kind);
        self.launch.telemetry.resume_hangs();
        r
    }

    // ------------------------------------------------------------------ closing

    fn handle_close(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|i| i.viewport().close_requested());
        if !requested || self.allow_close {
            return;
        }
        if self.session.is_dirty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.modals.iter().any(|m| matches!(m, Modal::Confirm { quit: true, .. })) {
                self.modals.push(Modal::Confirm { quit: true, command: None });
            }
        } else {
            self.shutdown(false);
        }
    }

    fn close(&mut self, ctx: &egui::Context, discarded: bool) {
        self.shutdown(discarded);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Settle everything before the process ends: stop jobs, flush settings, end autosave.
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    pub fn shutdown(&mut self, _discarded: bool) {
        if self.allow_close {
            return;
        }
        self.allow_close = true;
        self.jobs.cancel_all();
        self.persist_view();
        if let Err(e) = self.settings.flush() {
            log::warn!("settings not saved: {e}");
        }
        if let Some(mut a) = self.autosave.take() {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(w) = a.writer.take() {
                for e in w.shutdown(&mut a.autosave) {
                    log::warn!("autosave: {e}");
                }
            }
            // a clean exit (saved, or the user chose not to keep the changes)
            if let Err(e) = a.autosave.close(false) {
                log::warn!("autosave cleanup: {e}");
            }
        }
        self.launch.telemetry.flush(Duration::from_millis(500));
        log::info!("{} closed", self.shared.name);
    }

    /// End without any cleanup, as if the process had died: autosave files stay for the next
    /// launch to recover, settings are not flushed. For crash-recovery tests.
    #[cfg_attr(target_arch = "wasm32", allow(unused_mut))]
    pub fn abandon(mut self) {
        self.allow_close = true;
        if let Some(mut a) = self.autosave.take() {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(w) = a.writer.take() {
                let _ = w.shutdown(&mut a.autosave);
            }
            // dropping (not closing) releases the lock and keeps the session's files
            drop(a);
        }
    }

    fn continue_quit(&mut self, ctx: &egui::Context) {
        if self.quit_after_save {
            self.quit_after_save = false;
            if !self.session.is_dirty() {
                self.close(ctx, false);
            }
        }
    }

    // ------------------------------------------------------------------ storage

    fn persist_view(&mut self) {
        let view = Value::Object(self.session.view_state().clone());
        if self.settings.get_value("view") != Some(&view) {
            self.settings.set_value("view", view);
        }
    }

    fn tick_storage(&mut self, ctx: &egui::Context) {
        let t = ctx.input(|i| i.time).max(0.0);
        let now = Duration::from_secs_f64(t);
        // a pending save asks for its own frame through the settings' wake hook
        if let Err(e) = self.settings.tick(now) {
            log::warn!("settings not saved: {e}");
        }
        let Some(a) = &mut self.autosave else { return };
        if a.file_gen != self.session.file_generation() {
            a.file_gen = self.session.file_generation();
            let _ = a.autosave.set_document(&self.session.display_name(), self.session.path());
        }
        let dirty = self.session.is_dirty();
        let rev = self.session.revision();
        if !dirty {
            if a.autosave.meta().dirty {
                let _ = a.autosave.mark_clean();
            }
            return;
        }
        // an autosave not yet due asks for its frame through the autosave's wake hook, a finished
        // background write through the writer's
        let snap = self.session.store().snapshot();
        let format = Arc::clone(&self.shared.format);
        #[cfg(not(target_arch = "wasm32"))]
        let wrote = match &mut a.writer {
            Some(w) => {
                for e in w.pump(&mut a.autosave) {
                    log::warn!("autosave: {e}");
                }
                w.tick(&mut a.autosave, now, rev, dirty, move || format.encode(&snap).unwrap_or_default())
            }
            None => a.autosave.tick(now, rev, dirty, || format.encode(&snap).unwrap_or_default()).ok().flatten().is_some(),
        };
        #[cfg(target_arch = "wasm32")]
        let wrote = a.autosave.tick(now, rev, dirty, || format.encode(&snap).unwrap_or_default()).ok().flatten().is_some();
        if wrote {
            log::debug!("autosaved revision {}", rev.0);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let dirty = if self.session.is_dirty() { " •" } else { "" };
        let title = format!("{}{dirty} — {}", self.session.display_name(), self.shared.title);
        if title != self.shown_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.shown_title = title;
        }
    }

    /// The window title the runtime last set (`name • — App` when there are unsaved changes).
    pub fn title(&self) -> &str {
        &self.shown_title
    }

    // ------------------------------------------------------------------ dialogs and tools

    fn draw_modals(&mut self, ctx: &egui::Context) {
        if let Some(err) = self.view_error.clone() {
            let buttons = [DialogButton::new("Save As…"), DialogButton::new("Try Again").default()];
            let file = crash::last_panic().and_then(|p| p.log_file);
            match Dialog::new("ashlar.view_error", "Something went wrong", &buttons).width(460.0).show(ctx, |ui| {
                ui.label("The view hit an internal error and was stopped. Your document is safe: save it, then try again.");
                ui.add_space(6.0);
                ui.monospace(&err);
                if let Some(f) = &file {
                    ui.small(format!("Crash log: {}", f.display()));
                }
            }) {
                Some(DialogOutcome::Button(0)) => {
                    let r = self.execute("file.save_as", json!({}));
                    self.report(r);
                    self.apply_effects(ctx);
                }
                Some(_) => {
                    self.view_error = None;
                    crash::take_last_panic();
                }
                None => {}
            }
            return;
        }
        let Some(modal) = self.modals.first_mut() else { return };
        let done = match modal {
            Modal::Recovery => self.recovery_dialog(ctx),
            Modal::SafeMode => {
                let buttons = [DialogButton::new("Keep Settings"), DialogButton::new("Reset View Settings").default()];
                let title = format!("{} started in safe mode", self.shared.title);
                match Dialog::new("ashlar.safe_mode", &title, &buttons).show(ctx, |ui| {
                    ui.label("The last launch did not finish starting. Saved view settings (layout, theme) were not loaded this time.");
                }) {
                    Some(DialogOutcome::Button(1)) => {
                        self.settings.remove("view");
                        self.settings.remove("theme");
                        ctx.memory_mut(|m| *m = egui::Memory::default());
                        self.wakes.install(ctx);
                        true
                    }
                    Some(_) => true,
                    None => false,
                }
            }
            Modal::Consent(choice) => {
                let buttons = [DialogButton::new("Continue").default()];
                let title = format!("Help improve {}", self.shared.title);
                let mut c = *choice;
                let out = Dialog::new("ashlar.consent", &title, &buttons).width(440.0).show(ctx, |ui| {
                    ui.label("Anonymous reports help find crashes and slow spots. No document content, file names or typed text are ever sent. Change this any time in settings.");
                    ui.checkbox(&mut c[0], "Crash reports");
                    ui.checkbox(&mut c[1], "Performance (frame times, hangs)");
                    ui.checkbox(&mut c[2], "Usage (which commands are used)");
                });
                *choice = c;
                if out.is_some() {
                    self.launch.telemetry.notice_shown(c[0], c[1], c[2]);
                    true
                } else {
                    false
                }
            }
            Modal::Confirm { quit, command } => {
                let (quit, command) = (*quit, command.clone());
                self.confirm_dialog(ctx, quit, command)
            }
        };
        if done && !self.modals.is_empty() {
            self.modals.remove(0);
        }
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context, quit: bool, command: Option<(String, Value)>) -> bool {
        let buttons = [DialogButton::cancel("Cancel"), DialogButton::destructive("Don't Save"), DialogButton::new("Save").default()];
        let title = format!("Save changes to “{}”?", self.session.display_name());
        let out = Dialog::new("ashlar.confirm_discard", &title, &buttons).show(ctx, |ui| {
            ui.label("Your changes will be lost if you don't save them.");
        });
        match out {
            Some(DialogOutcome::Button(1)) => {
                if quit {
                    self.close(ctx, true);
                } else if let Some((id, params)) = command {
                    self.run_ui_command(ctx, &id, params);
                }
                true
            }
            Some(DialogOutcome::Button(2)) => {
                self.quit_after_save = quit;
                let r = self.execute("file.save", json!({}));
                self.report(r);
                self.apply_effects(ctx);
                if !self.session.is_dirty() {
                    if quit {
                        self.continue_quit(ctx);
                    } else if let Some((id, params)) = command {
                        self.run_ui_command(ctx, &id, params);
                    }
                }
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    fn recovery_dialog(&mut self, ctx: &egui::Context) -> bool {
        let Some(c) = self.recovery.first() else { return true };
        let buttons = [DialogButton::destructive("Discard"), DialogButton::new("Later"), DialogButton::new("Recover").default()];
        let label = if c.meta.label.is_empty() { "Untitled".to_string() } else { c.meta.label.clone() };
        let ago = (ashlar_store::autosave::unix_now() - c.meta.saved_unix).max(0);
        let out = Dialog::new("ashlar.recovery", "Recover unsaved work?", &buttons).width(440.0).show(ctx, |ui| {
            ui.label(format!("“{label}” had unsaved changes when {} last closed (autosaved {} ago).", self.shared.title, human_age(ago)));
        });
        let Some(out) = out else { return false };
        let c = self.recovery.remove(0);
        match out {
            DialogOutcome::Button(2) => match c.latest().map(|p| self.shared.format.load(p)) {
                Some(Ok(loaded)) => {
                    let path = c.meta.doc_path.as_ref().map(PathBuf::from);
                    self.session.replace(D::default(), path, None);
                    self.session.replace_as_edit("Recover Unsaved Work", loaded.doc);
                    if let Err(e) = c.discard() {
                        log::warn!("recovery files not removed: {e}");
                    }
                    self.toasts.success(format!("Recovered “{label}”. Save it to keep it."));
                }
                Some(Err(e)) => {
                    self.toasts.error(format!("Could not recover “{label}”: {e}"));
                }
                None => {
                    let _ = c.discard();
                }
            },
            DialogOutcome::Button(0) => {
                if let Err(e) = c.discard() {
                    log::warn!("recovery files not removed: {e}");
                }
            }
            _ => {}
        }
        self.recovery.is_empty()
    }

    fn draw_tools(&mut self, ctx: &egui::Context) {
        if self.palette.is_open()
            && let Some(id) = self.palette.show(ctx, &self.cache.infos, self.cache.seq, Platform::current())
        {
            self.run_ui_command(ctx, &id, json!({}));
        }
        if self.keymap_open {
            let mut open = true;
            let mut change = None;
            let (keymap, editor, infos, seq) = (&mut self.keymap, &mut self.keymap_editor, &self.cache.infos, self.cache.seq);
            egui::Window::new("Keyboard Shortcuts").id(egui::Id::new("ashlar.keymap")).open(&mut open).default_size([520.0, 420.0]).show(ctx, |ui| {
                change = editor.show(ui, keymap, infos, seq);
            });
            self.keymap_open = open;
            if let Some(c) = change {
                log::info!("keymap changed: {c:?}");
                let _ = self.settings.set("keymap", &self.keymap.overrides());
                self.keymap_gen = self.keymap_gen.wrapping_add(1);
                if matches!(c, KeymapChange::Bound { .. } | KeymapChange::Reset { .. }) {
                    self.cache.key = None;
                }
            }
        }
    }

    // ------------------------------------------------------------------ agent control

    fn serve_control(&mut self, ctx: &egui::Context) {
        let mut reqs: Vec<Request> = std::mem::take(&mut self.injected_requests);
        if let Some((_, rx)) = &self.control {
            reqs.extend(rx.try_iter().take(256));
        }
        if reqs.is_empty() {
            return;
        }
        let info = self.info();
        let mut queue = UiQueue::default();
        for req in reqs {
            let mut bound = self.shared.registry.bind(&mut self.session);
            ashlar_control::handle(req, &mut bound, &mut queue, &info);
            self.apply_effects(ctx);
        }
        for req in std::mem::take(&mut queue.pending) {
            self.answer_ui(ctx, req);
        }
        wake::wake(ctx, Reason::CONTROL);
    }

    fn answer_ui(&mut self, ctx: &egui::Context, req: Request) {
        let p = req.params().clone();
        let visible = ctx.input(|i| i.viewport().visible()).unwrap_or(true);
        if devtools::handles(req.method()) {
            let needs_frames = !matches!(req.method(), "devtools.toggle" | "devtools.state");
            if needs_frames && !visible {
                // eframe draws no frames for a minimized or fully covered window
                return req.fail("the window is minimized or covered, so it draws no frames: bring it to the front for ui input, snapshots and screenshots (engine.execute works regardless)");
            }
            if let devtools::Outcome::NotMine(req) = devtools::handle(ctx, req) {
                req.unknown();
            }
            return;
        }
        match req.method() {
            "ui.inspect" => {
                let ins = automation::inspect(ctx);
                req.ok(json!({
                    "pass": ins.pass,
                    "elements": ins.elements,
                    "title": self.shown_title,
                    "document": {"name": self.session.display_name(), "dirty": self.session.is_dirty(), "revision": self.session.revision().0, "path": self.session.path().map(|p| p.to_string_lossy().into_owned())},
                    "view": self.session.view_state(),
                    "palette_open": self.palette.is_open(),
                    "keymap_open": self.keymap_open,
                    "dialogs": self.modals.len(),
                    "view_error": self.view_error,
                    "toasts": self.toasts.texts().collect::<Vec<_>>(),
                    "visible": visible,
                }));
            }
            "ui.menu.list" => {
                self.refresh_infos();
                req.ok(serde_json::to_value(&self.cache.menu).unwrap_or(Value::Null));
            }
            "ui.menu.invoke" => {
                let Some(id) = p.get("id").and_then(Value::as_str) else { return req.fail("ui.menu.invoke needs `id`") };
                self.refresh_infos();
                // menu item ids are command ids
                let command = self.cache.menu.find(id).map(|i| i.id.clone());
                match command {
                    Some(c) => {
                        let r = self.execute(&c, json!({}));
                        self.apply_effects(ctx);
                        req.reply(Reply::from_command(r));
                    }
                    None => req.fail(format!("no menu item `{id}` (see ui.menu.list)")),
                }
            }
            "ui.set" => {
                let Some(obj) = p.as_object() else { return req.fail("ui.set needs an object") };
                for (k, v) in obj {
                    if k == "theme" {
                        let mode: Option<ThemeMode> = serde_json::from_value(v.clone()).ok();
                        match mode {
                            Some(m) => {
                                self.session.set_view("theme", &m);
                                self.apply_effect(ctx, Effect::Theme(m));
                            }
                            None => return req.fail("theme is light, dark or system"),
                        }
                    } else {
                        self.session.set_view_value(k, v.clone());
                    }
                }
                req.ok(json!({}));
            }
            "app.quit" => {
                let discard = p.get("discard").and_then(Value::as_bool).unwrap_or(false);
                let dirty = self.session.is_dirty();
                self.apply_effect(ctx, Effect::Quit { discard });
                req.ok(json!({"closing": !dirty || discard}));
            }
            "app.stats" => {
                let mut v = serde_json::to_value(self.stats.summary()).unwrap_or(Value::Null);
                if let Some(o) = v.as_object_mut() {
                    o.insert("passes".into(), json!(self.passes));
                    o.insert("jobs".into(), self.jobs.to_json());
                    o.insert("log_lines_dropped".into(), json!(crate::logging::dropped()));
                    o.insert("wakes".into(), serde_json::to_value(self.wakes.stats()).unwrap_or(Value::Null));
                }
                if p.get("reset").and_then(Value::as_bool) == Some(true) {
                    self.stats.reset_window();
                }
                req.ok(v);
            }
            "app.logs" => {
                let limit = p.get("limit").and_then(Value::as_u64).unwrap_or(200).min(crate::logging::RING_LINES as u64) as usize;
                let level = p.get("level").and_then(Value::as_str).and_then(crate::logging::level_of).unwrap_or(log::Level::Trace);
                req.ok(json!({"lines": crate::logging::recent(limit, level)}));
            }
            _ => req.unknown(),
        }
    }

    /// The inspector's panels and the app's pending work for `ui.wait {for: "idle"}`. The panels
    /// are built only while the inspector is open.
    fn feed_devtools(&mut self, ctx: &egui::Context) {
        devtools::set_busy(ctx, self.jobs.is_busy());
        devtools::section(ctx, "Logs", || {
            crate::logging::recent(200, log::Level::Trace).into_iter().map(|l| format!("{:<5} {}: {}", l.level, l.target, l.message)).collect()
        });
        let history = self.session.store().history();
        devtools::section(ctx, "Undo", || {
            let mut lines: Vec<String> = history.undo_labels().map(|l| format!("  {l}")).collect();
            lines.push("── now ──".into());
            lines.extend(history.redo_labels().map(|l| format!("  {l} (redo)")));
            lines
        });
    }
}

impl<D: Document> Drop for Runtime<D> {
    fn drop(&mut self) {
        self.shutdown(false);
    }
}

fn human_age(secs: i64) -> String {
    match secs {
        s if s < 90 => format!("{s} s"),
        s if s < 90 * 60 => format!("{} min", s / 60),
        s if s < 48 * 3600 => format!("{} h", s / 3600),
        s => format!("{} days", s / 86_400),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn native_pick<D: Document>(shared: &Shared<D>, session: &Session<D>, kind: Pick) -> Option<PathBuf> {
    use ashlar_native::dialogs::FileDialog;
    let exts: Vec<&str> = shared.extensions.iter().map(String::as_str).collect();
    let mut d = FileDialog::new();
    if !exts.is_empty() {
        d = d.filter(shared.title.clone(), &exts);
    }
    if let Some(dir) = session.path().and_then(std::path::Path::parent) {
        d = d.directory(dir);
    }
    let r = match kind {
        Pick::Open => d.title("Open").open_file(),
        Pick::SaveAs => {
            let name = match exts.first() {
                Some(e) if session.path().is_none() => format!("Untitled.{e}"),
                _ => session.display_name(),
            };
            d.title("Save As").file_name(name).save_file()
        }
    };
    match r {
        Ok(p) => Some(p),
        Err(ashlar_native::NativeError::Cancelled) => None,
        Err(e) => {
            log::warn!("file dialog: {e}");
            None
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn native_pick<D: Document>(_shared: &Shared<D>, _session: &Session<D>, _kind: Pick) -> Option<PathBuf> {
    None
}

// ---------------------------------------------------------------------------------------------
// eframe
// ---------------------------------------------------------------------------------------------

struct EframeApp<D: Document> {
    rt: Runtime<D>,
    /// The debugger renders screenshots on eframe's wgpu device (set on the first frame).
    offscreen: bool,
}

impl<D: Document> eframe::App for EframeApp<D> {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.rt.logic(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if !std::mem::replace(&mut self.offscreen, true) {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(rs) = frame.wgpu_render_state() {
                devtools::set_offscreen(ui.ctx(), Some(crate::offscreen::renderer(rs.clone())));
            }
            #[cfg(target_arch = "wasm32")]
            let _ = frame;
        }
        self.rt.ui(ui);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.rt.raw_input_hook(ctx, raw_input);
    }

    fn on_exit(&mut self) {
        self.rt.shutdown(false);
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct Args {
    control: Option<u16>,
    safe_mode: bool,
    open: Option<PathBuf>,
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_args(env_prefix: &str) -> Args {
    let mut a = Args { control: None, safe_mode: false, open: None };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--control" => a.control = it.next().and_then(|p| p.parse().ok()),
            s if s.starts_with("--control=") => a.control = s.trim_start_matches("--control=").parse().ok(),
            "--safe-mode" => a.safe_mode = true,
            s if !s.starts_with('-') && a.open.is_none() => a.open = Some(PathBuf::from(s)),
            _ => {}
        }
    }
    if a.control.is_none() {
        a.control = std::env::var(format!("{env_prefix}_CONTROL_PORT")).ok().and_then(|p| p.trim().parse().ok());
    }
    a
}

/// On the web: run in the `<canvas>` whose id is the app's name (or `ashlar`). Returns at once;
/// the app runs in the browser's event loop.
#[cfg(target_arch = "wasm32")]
pub(crate) fn run_native<D: Document>(app: App<D>) -> Result<(), Error> {
    use wasm_bindgen::JsCast;
    let (shared, view, setup) = app.split();
    let level = log::LevelFilter::Info;
    let console: crate::logging::Output = Box::new(|line| web_sys::console::log_1(&line.into()));
    crate::logging::install(Box::new(crate::logging::Logger::new(&shared.name, None, level, false).with_output(console)), level);
    // the browser console sees every panic: ashlar's hook runs first, then chains to this one
    static CONSOLE_HOOK: std::sync::Once = std::sync::Once::new();
    CONSOLE_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            web_sys::console::error_1(&format!("panic: {info}").into());
            previous(info);
        }));
    });
    crash::install(None, &shared.name, &shared.version);
    let document = web_sys::window().and_then(|w| w.document()).ok_or_else(|| Error::Window("no document".into()))?;
    let canvas = document
        .get_element_by_id(&shared.name)
        .or_else(|| document.get_element_by_id("ashlar"))
        .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        .ok_or_else(|| Error::Window(format!("no <canvas id=\"{}\"> in the page", shared.name)))?;
    wasm_bindgen_futures::spawn_local(async move {
        let creator: eframe::AppCreator<'static> =
            Box::new(move |_cc| Ok(Box::new(EframeApp { rt: Runtime::new(shared, view, setup, Launch::default()), offscreen: false })));
        if let Err(e) = eframe::WebRunner::new().start(canvas, eframe::WebOptions::default(), creator).await {
            log::error!("the app could not start: {e:?}");
        }
    });
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run_native<D: Document>(app: App<D>) -> Result<(), Error> {
    let (shared, view, setup) = app.split();
    let args = parse_args(&shared.env_prefix);
    let logs = shared.dirs.as_ref().map(|d| d.logs.clone());
    if let Some(d) = &shared.dirs
        && let Err(e) = d.ensure()
    {
        eprintln!("{}: could not create its folders: {e}", shared.name);
    }
    let env_var = format!("{}_LOG", shared.env_prefix);
    let filter = crate::logging::filter_from_env(&[env_var.as_str(), "RUST_LOG"], "info");
    let level = filter.max_level();
    let telemetry = match (&shared.telemetry, &shared.dirs) {
        (Some(cfg), Some(d)) => Telemetry::start(cfg, &shared.name, &shared.version, &d.data, true),
        _ => Telemetry::default(),
    };
    let logger = crate::logging::Logger::new(&shared.name, logs.as_deref(), level, true).with_filter(filter);
    crate::logging::install(telemetry.wrap_logger(Box::new(logger)), level);
    // after the tracer's hook: ours runs first, then chains to it
    crash::install(logs.as_deref(), &shared.name, &shared.version);
    log::info!("{} {} starting", shared.name, shared.version);

    let (guard, previous) = match &shared.dirs {
        Some(d) => {
            let (g, p) = LaunchGuard::begin(&d.data);
            (Some(g), p)
        }
        None => (None, PreviousLaunch::Clean),
    };
    let safe_mode = args.safe_mode || matches!(previous, PreviousLaunch::CrashedAtStartup { .. });
    if safe_mode {
        log::warn!("starting in safe mode ({previous:?})");
    }
    let launch = Launch { safe_mode, previous: Some(previous), open: args.open, control_port: args.control, guard, telemetry };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(shared.title.clone())
            .with_inner_size(shared.window_size)
            .with_app_id(shared.name.clone()),
        ..Default::default()
    };
    let title = shared.title.clone();
    eframe::run_native(
        &title,
        options,
        Box::new(move |_cc| {
            let mut rt = Runtime::new(shared, view, setup, launch);
            rt.native = true;
            Ok(Box::new(EframeApp { rt, offscreen: false }))
        }),
    )
    .map_err(|e| Error::Window(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_well() {
        assert_eq!(human_age(5), "5 s");
        assert_eq!(human_age(600), "10 min");
        assert_eq!(human_age(7200), "2 h");
        assert_eq!(human_age(300_000), "3 days");
    }
}
