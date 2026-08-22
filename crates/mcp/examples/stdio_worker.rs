//! A worker for `Backend::Subprocess`: serves the control protocol on stdin/stdout for a tiny
//! counter app. An app's `-cli serve` verb does the same for its own session.

#[path = "../tests/support/mod.rs"]
mod support;

use ashlar_control::{AppInfo, DEFAULT_MAX_LINE, NoUi, answer, serve_stream};

fn main() {
    // Stray stdout output is tolerated by the parent; show that it is.
    println!("worker starting");
    let mut session = support::Counter::default();
    let app = AppInfo::new("counter", "0.1.0");
    serve_stream(std::io::stdin().lock(), std::io::stdout().lock(), DEFAULT_MAX_LINE, |method, params| {
        answer(method, params, &mut session, &mut NoUi, &app)
    });
}
