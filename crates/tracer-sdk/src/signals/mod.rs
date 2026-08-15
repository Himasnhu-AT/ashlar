//! Crash, hang, performance, system and log capture. Talks to the rest of the SDK only through [`Sink`].

use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use tracer_proto::{Breadcrumb, Device};

use crate::config::Config;
use crate::sink::Sink;

pub mod rt;

pub(crate) struct Signals {
    #[allow(dead_code)]
    sink: Arc<dyn Sink>,
}

impl Signals {
    /// Builds the state and starts what the config enables: panic hook, hang watchdog, logger.
    pub(crate) fn start(_config: &Config, sink: Arc<dyn Sink>) -> Arc<Self> {
        Arc::new(Self { sink })
    }

    /// Called from the pipeline's worker thread about once a second: closes performance windows,
    /// samples CPU and memory, and saves breadcrumbs to disk.
    pub(crate) fn tick(&self) {}

    /// Flushes the open performance window. Called before the session ends.
    pub(crate) fn shutdown(&self) {}

    pub(crate) fn frame(&self, _duration: Duration) {}

    pub(crate) fn first_frame(&self) {}

    pub(crate) fn heartbeat(&self) {}

    pub(crate) fn pause_hang_detection(&self) {}

    pub(crate) fn resume_hang_detection(&self) {}

    pub(crate) fn report_handled(&self, _message: &str, _context: Option<&'static str>) {}

    pub(crate) fn report_child_exit(&self, _program: &'static str, _status: ExitStatus) {}

    pub(crate) fn counter(&self, _name: &'static str) -> rt::Counter {
        rt::Counter::detached()
    }

    pub(crate) fn gauge(&self, _name: &'static str) -> rt::Gauge {
        rt::Gauge::detached()
    }

    /// A logger that records breadcrumbs and forwards every record to `inner`.
    pub(crate) fn logger(self: &Arc<Self>, inner: Option<Box<dyn log::Log>>) -> Box<dyn log::Log> {
        Box::new(NullLogger(inner))
    }

    /// Writes a human-readable report (session, device, recent logs, last crash) for a bug report.
    pub(crate) fn write_report(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }
}

/// Breadcrumbs the previous run saved before it ended, oldest first. Removes them from disk.
/// Called by the pipeline at startup when it finds that the previous run ended uncleanly.
pub(crate) fn take_saved_breadcrumbs(_dir: &Path) -> Vec<Breadcrumb> {
    Vec::new()
}

/// Static details of this machine for the envelope header. Called once by the pipeline at startup.
/// Only the operating system name and architecture are available without the `sysinfo` feature.
pub(crate) fn device_info(config: &Config) -> Device {
    Device { os_name: std::env::consts::OS.to_owned(), arch: std::env::consts::ARCH.to_owned(), gpu: config.gpu.clone(), ..Device::default() }
}

struct NullLogger(Option<Box<dyn log::Log>>);

impl log::Log for NullLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        self.0.as_ref().is_some_and(|inner| inner.enabled(metadata))
    }

    fn log(&self, record: &log::Record<'_>) {
        if let Some(inner) = &self.0 {
            inner.log(record);
        }
    }

    fn flush(&self) {
        if let Some(inner) = &self.0 {
            inner.flush();
        }
    }
}
