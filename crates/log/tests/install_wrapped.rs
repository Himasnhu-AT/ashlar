#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `install_wrapped` installs the wrapper around the logger, and the wrapper sees every record
//! before passing it on (its own test binary: the process logger can be set once).

use std::sync::{Arc, Mutex};

use log::{LevelFilter, Log, Metadata, Record};

struct Collect(Arc<Mutex<Vec<String>>>, &'static str);

impl Log for Collect {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn log(&self, r: &Record<'_>) {
        self.0.lock().unwrap().push(format!("{} {}", self.1, r.args()));
    }
    fn flush(&self) {}
}

struct Wrap {
    inner: Box<dyn Log>,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Log for Wrap {
    fn enabled(&self, m: &Metadata<'_>) -> bool {
        self.inner.enabled(m)
    }
    fn log(&self, r: &Record<'_>) {
        self.seen.lock().unwrap().push(format!("wrapper {}", r.args()));
        self.inner.log(r);
    }
    fn flush(&self) {}
}

#[test]
fn the_wrapper_sees_every_record_and_forwards_it() {
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let inner = Box::new(Collect(lines.clone(), "inner"));
    let seen = lines.clone();
    assert!(ashlar_log::logging::install_wrapped(inner, LevelFilter::Info, |l| Box::new(Wrap { inner: l, seen })));
    log::info!("hello");
    assert_eq!(*lines.lock().unwrap(), ["wrapper hello", "inner hello"]);
}
