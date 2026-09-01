//! Telemetry hook points. With the `telemetry` feature this wraps a `tracer_sdk::Tracer`; without
//! it every call is an empty inline function.
//!
//! Hook points: the command chokepoint (`command.run` with the command id, ok and duration), every
//! frame (`frame`), the first frame, each main-loop turn, the panic hook (the tracer's hook runs
//! after the runtime's own), and consent: nothing is sent until the app has shown the notice
//! ([`Telemetry::notice_pending`], [`Telemetry::notice_shown`]); the runtime shows one.

use std::time::Duration;

/// Where and how to send telemetry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelemetryConfig {
    /// The app's write key (not a secret; it only allows ingest).
    pub key: String,
    /// Server base URL. `None` records locally and sends nothing.
    pub endpoint: Option<String>,
}

#[cfg(feature = "telemetry")]
mod imp {
    use super::*;
    use tracer_sdk::{ConsentCategory, ProcessKind, Prop, Tracer};

    tracer_sdk::events! {
        pub enum Category {
            Command = "command",
            App = "app",
        }
        pub enum AppEvent {
            CommandRun = "command.run" in Command,
            AppStarted = "app.started" in App,
            DocumentSaved = "app.document_saved" in App,
        }
    }

    /// The runtime's tracer (cheap to clone).
    #[derive(Clone)]
    pub struct Telemetry {
        pub(crate) tracer: Tracer,
    }

    impl Default for Telemetry {
        fn default() -> Self {
            Telemetry { tracer: Tracer::disabled() }
        }
    }

    impl Telemetry {
        pub fn start(cfg: &TelemetryConfig, app: &str, version: &str, data_dir: &std::path::Path, gui: bool) -> Telemetry {
            let mut b = Tracer::builder(&cfg.key, app, version).data_dir(data_dir).listen_all::<AppEvent>().process(if gui {
                ProcessKind::Gui
            } else {
                ProcessKind::Cli
            });
            if let Some(e) = &cfg.endpoint {
                b = b.endpoint(e);
            }
            let tracer = b.init();
            tracer.track(AppEvent::AppStarted);
            Telemetry { tracer }
        }

        pub fn is_enabled(&self) -> bool {
            self.tracer.is_enabled()
        }

        /// Wrap the runtime's logger so log lines become crash breadcrumbs.
        pub fn wrap_logger(&self, inner: Box<dyn log::Log>) -> Box<dyn log::Log> {
            self.tracer.logger(Some(inner))
        }

        pub fn command(&self, id: &'static str, ok: bool, took: Option<Duration>) {
            let ms = took.map_or(-1.0, |d| d.as_secs_f64() * 1000.0);
            self.tracer.track_with(AppEvent::CommandRun, &[("id", Prop::Str(id)), ("ok", Prop::Bool(ok)), ("ms", Prop::Float(ms))]);
            self.tracer.track_id(Category::Command, id);
        }

        pub fn saved(&self) {
            self.tracer.track(AppEvent::DocumentSaved);
        }

        pub fn frame(&self, d: Duration) {
            self.tracer.frame(d);
        }

        pub fn first_frame(&self) {
            self.tracer.first_frame();
        }

        pub fn begin_turn(&self) {
            self.tracer.begin_turn();
        }

        pub fn end_turn(&self) {
            self.tracer.end_turn();
        }

        pub fn input(&self, focused: bool, active: bool) {
            self.tracer.set_focused(focused);
            if active {
                self.tracer.note_input();
            }
        }

        pub fn pause_hangs(&self) {
            self.tracer.pause_hang_detection();
        }

        pub fn resume_hangs(&self) {
            self.tracer.resume_hang_detection();
        }

        pub fn notice_pending(&self) -> bool {
            self.tracer.is_enabled() && self.tracer.notice_pending()
        }

        /// The user closed the notice with these choices (crash, performance, usage).
        pub fn notice_shown(&self, crash: bool, performance: bool, usage: bool) {
            self.tracer.set_consent(ConsentCategory::Crash, crash);
            self.tracer.set_consent(ConsentCategory::Performance, performance);
            self.tracer.set_consent(ConsentCategory::Usage, usage);
            self.tracer.notice_shown();
        }

        pub fn flush(&self, timeout: Duration) {
            let _ = self.tracer.flush_blocking(timeout);
        }
    }
}

#[cfg(not(feature = "telemetry"))]
mod imp {
    use super::*;

    /// Telemetry is compiled out (`telemetry` feature off): every call does nothing.
    #[derive(Clone, Default)]
    pub struct Telemetry(());

    #[allow(clippy::unused_self)]
    impl Telemetry {
        pub fn start(_cfg: &TelemetryConfig, _app: &str, _version: &str, _data_dir: &std::path::Path, _gui: bool) -> Telemetry {
            Telemetry(())
        }
        pub fn is_enabled(&self) -> bool {
            false
        }
        pub fn wrap_logger(&self, inner: Box<dyn log::Log>) -> Box<dyn log::Log> {
            inner
        }
        #[inline]
        pub fn command(&self, _id: &'static str, _ok: bool, _took: Option<Duration>) {}
        #[inline]
        pub fn saved(&self) {}
        #[inline]
        pub fn frame(&self, _d: Duration) {}
        #[inline]
        pub fn first_frame(&self) {}
        #[inline]
        pub fn begin_turn(&self) {}
        #[inline]
        pub fn end_turn(&self) {}
        #[inline]
        pub fn input(&self, _focused: bool, _active: bool) {}
        #[inline]
        pub fn pause_hangs(&self) {}
        #[inline]
        pub fn resume_hangs(&self) {}
        pub fn notice_pending(&self) -> bool {
            false
        }
        pub fn notice_shown(&self, _crash: bool, _performance: bool, _usage: bool) {}
        pub fn flush(&self, _timeout: Duration) {}
    }
}

pub use imp::Telemetry;
#[cfg(feature = "telemetry")]
pub use imp::{AppEvent, Category};
