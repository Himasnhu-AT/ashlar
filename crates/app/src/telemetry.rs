//! Telemetry hook points. With the `telemetry` feature this wraps an `ashlar_telemetry::Telemetry`
//! (always on when an endpoint is configured: no notice, no switches; `TRACER_DISABLED` turns it
//! off); without it every call is an empty inline function.
//!
//! Hook points: the command chokepoint (`command.run` with the command id, ok and duration), every
//! frame (`frame`; the first one also records the startup time), each main-loop turn, the panic
//! hook (the tracer's hook runs after the runtime's own), saves, and a short flush on quit.

use std::time::Duration;

/// Where and how to send telemetry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelemetryConfig {
    /// The app's write key (not a secret; it only allows ingest).
    pub key: String,
    /// Server base URL. `None`: no telemetry at all.
    pub endpoint: Option<String>,
}

#[cfg(feature = "telemetry")]
mod imp {
    use super::*;

    /// The runtime's telemetry client (cheap to clone).
    #[derive(Clone, Default)]
    pub struct Telemetry {
        pub(crate) inner: ashlar_telemetry::Telemetry,
    }

    impl Telemetry {
        pub fn start(cfg: &TelemetryConfig, app: &str, version: &str, data_dir: &std::path::Path, gui: bool) -> Telemetry {
            let process = if gui { ashlar_telemetry::ProcessKind::Gui } else { ashlar_telemetry::ProcessKind::Cli };
            let c = ashlar_telemetry::TelemetryConfig::new(app, version).endpoint(cfg.endpoint.as_deref(), &cfg.key).process(process);
            Telemetry { inner: ashlar_telemetry::Telemetry::start(&c, Some(data_dir)) }
        }

        pub fn is_enabled(&self) -> bool {
            self.inner.is_enabled()
        }

        /// Wrap the runtime's logger so log lines become crash breadcrumbs (redacted).
        pub fn wrap_logger(&self, inner: Box<dyn log::Log>) -> Box<dyn log::Log> {
            self.inner.wrap_logger(inner)
        }

        pub fn command(&self, id: &'static str, ok: bool, took: Option<Duration>) {
            self.inner.command(id, ok, took.unwrap_or_default());
        }

        pub fn saved(&self) {
            self.inner.saved();
        }

        pub fn frame(&self, d: Duration) {
            self.inner.frame(d);
        }

        /// Recorded by the first [`Telemetry::frame`]; kept for callers.
        pub fn first_frame(&self) {}

        pub fn begin_turn(&self) {
            self.inner.tracer().begin_turn();
        }

        pub fn end_turn(&self) {
            self.inner.tracer().end_turn();
        }

        pub fn input(&self, focused: bool, active: bool) {
            self.inner.input(focused, active);
        }

        pub fn pause_hangs(&self) {
            self.inner.tracer().pause_hang_detection();
        }

        pub fn resume_hangs(&self) {
            self.inner.tracer().resume_hang_detection();
        }

        pub fn flush(&self, timeout: Duration) {
            self.inner.flush(timeout);
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
        pub fn flush(&self, _timeout: Duration) {}
    }
}

pub use imp::Telemetry;
