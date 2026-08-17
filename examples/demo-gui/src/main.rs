//! A minimal windowed host that wires the Tracer SDK by hand, the way a real immediate-mode app would.
//!
//! It has been compiled and linted (`cargo xtask demo`) but never run: there is no display where it was
//! written. Settings come from the environment so nothing is sent unless asked to:
//!
//! - `TRACER_DEMO_DIR`: data folder (default: a `tracer-demo-gui` folder in the system temp dir)
//! - `TRACER_DEMO_ENDPOINT`, `TRACER_DEMO_KEY`: server and write key; without an endpoint nothing is sent
//!
//! Inspect mode is on, so everything that would be sent is also written to `outbox.jsonl`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use tracer_sdk::{ConsentCategory, Gpu, LoggerMode, Prop, Tracer};

tracer_sdk::events! {
    pub enum Category {
        Ui = "ui",
        Doc = "doc",
    }
    pub enum DemoEvent {
        ButtonClicked = "ui.button_clicked" in Ui,
        ThemeToggled = "ui.theme_toggled" in Ui,
        DocSaved = "doc.saved" in Doc,
    }
}

struct DemoApp {
    tracer: Tracer,
    first_frame_done: bool,
    status: String,
    dark: bool,
    data_dir: PathBuf,
}

/// Graphics adapter details for the envelope header, if the host can read them.
///
/// With the `wgpu` renderer they are in `cc.wgpu_render_state` (`adapter.get_info()`). This demo uses the
/// `glow` renderer, where the renderer string can only be read through an `unsafe` GL call, which this
/// workspace forbids, so it reports none.
fn gpu_details(_cc: &eframe::CreationContext<'_>) -> Option<Gpu> {
    None
}

impl DemoApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let data_dir = std::env::var_os("TRACER_DEMO_DIR").map_or_else(|| std::env::temp_dir().join("tracer-demo-gui"), PathBuf::from);
        let key = std::env::var("TRACER_DEMO_KEY").unwrap_or_else(|_| "tk_unset".to_owned());

        let mut builder = Tracer::builder(&key, "demo", env!("CARGO_PKG_VERSION"))
            .data_dir(&data_dir)
            .listen_all::<DemoEvent>()
            .logger(LoggerMode::Install)
            .inspect(true);
        if let Ok(endpoint) = std::env::var("TRACER_DEMO_ENDPOINT") {
            builder = builder.endpoint(endpoint);
        }
        if let Some(gpu) = gpu_details(cc) {
            builder = builder.gpu(gpu);
        }
        let tracer = builder.init();
        log::info!("demo window opened");
        Self { tracer, first_frame_done: false, status: String::new(), dark: true, data_dir }
    }

    fn consent_switch(&self, ui: &mut eframe::egui::Ui, category: ConsentCategory, label: &str) {
        let mut allowed = self.tracer.consent(category);
        if ui.checkbox(&mut allowed, label).changed() {
            self.tracer.set_consent(category, allowed);
        }
    }

    fn contents(&mut self, ui: &mut eframe::egui::Ui) {
        // First-run notice. Recording has already started, but nothing is sent until the user has seen
        // this and pressed OK; a category switched off below before that is never sent at all.
        if self.tracer.notice_pending() {
            ui.group(|ui| {
                ui.label("This app sends crash reports, performance numbers and usage counts to its developer. You can switch each off below.");
                if ui.button("OK").clicked() {
                    self.tracer.notice_shown();
                }
            });
        }

        ui.heading("Tracer demo");
        ui.label(format!("install {}", self.tracer.install_id()));
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("Track an event").clicked() {
                self.tracer.track_with(DemoEvent::ButtonClicked, &[("button", Prop::Str("track")), ("big", Prop::Bool(false))]);
                self.tracer.track_id(Category::Ui, "ui.palette_command");
                self.status = "tracked ui.button_clicked".to_owned();
            }
            if ui.button("Save").clicked() {
                self.tracer.track(DemoEvent::DocSaved);
                log::info!("saved the document");
            }
            if ui.checkbox(&mut self.dark, "Dark").changed() {
                self.tracer.track(DemoEvent::ThemeToggled);
            }
        });

        ui.horizontal(|ui| {
            if ui.button("Handled error").clicked() {
                let failure = std::io::Error::other("the demo export failed");
                self.tracer.report_handled(&failure, Some("demo.export"));
                self.status = "reported a handled error".to_owned();
            }
            if ui.button("Panic").clicked() {
                crash_on_purpose();
            }
        });

        ui.horizontal(|ui| {
            if ui.button("Block the UI for 3 s").clicked() {
                // This turn of the loop takes 3 s: the watchdog reports a hang once the threshold passes.
                log::warn!("blocking the UI thread");
                std::thread::sleep(Duration::from_secs(3));
            }
            if ui.button("Block for 3 s, detection paused").clicked() {
                // What a host does around a native modal dialog that stops its loop.
                self.tracer.pause_hang_detection();
                std::thread::sleep(Duration::from_secs(3));
                self.tracer.resume_hang_detection();
            }
        });

        ui.separator();
        ui.label("Data the SDK may send");
        self.consent_switch(ui, ConsentCategory::Crash, "Crash reports");
        self.consent_switch(ui, ConsentCategory::Performance, "Performance");
        self.consent_switch(ui, ConsentCategory::Usage, "Usage");

        ui.horizontal(|ui| {
            if ui.button("Write local report").clicked() {
                let path = self.data_dir.join("report.txt");
                self.status = match self.tracer.write_report(&path) {
                    Ok(()) => format!("wrote {}", path.display()),
                    Err(error) => format!("could not write the report: {error}"),
                };
            }
            if ui.button("Forget me").clicked() {
                self.tracer.forget();
                self.status = "forgotten; new install id issued".to_owned();
            }
        });
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
    }
}

/// The deliberate crash behind the "Panic" button.
#[allow(clippy::panic, reason = "the demo's panic button exists to crash the host on purpose")]
fn crash_on_purpose() -> ! {
    panic!("demo panic from the button")
}

impl eframe::App for DemoApp {
    /// Runs once per loop turn, whether or not a frame is drawn.
    ///
    /// The toolkit owns the event loop and only runs it on input, so this host never sees the wait
    /// between turns. It marks the work it does see, here and in `ui`, as turns: a turn that takes longer
    /// than the hang threshold is a hang, and the idle time in between never is. No timed repaint is
    /// needed to keep the watchdog quiet.
    fn logic(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        let _turn = self.tracer.turn();
        let (focused, active) = ctx.input(|input| (input.focused, !input.events.is_empty()));
        self.tracer.set_focused(focused);
        if active {
            self.tracer.note_input();
        }
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        let _turn = self.tracer.turn();
        let started = Instant::now();
        self.contents(ui);
        self.tracer.frame(started.elapsed());
        if !self.first_frame_done {
            self.first_frame_done = true;
            self.tracer.first_frame();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.tracer.flush_blocking(Duration::from_secs(2));
    }
}

fn main() -> eframe::Result {
    eframe::run_native("Tracer demo", eframe::NativeOptions::default(), Box::new(|cc| Ok(Box::new(DemoApp::new(cc)))))
}
