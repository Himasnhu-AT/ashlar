#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Its own test binary: the dialog hook is process-wide, and the hang test in `hooks.rs` installs
//! one of its own.

use std::time::Duration;

use ashlar_telemetry::TelemetryConfig;
use ashlar_telemetry::testing::{self, Recorder};

/// The dialog hook holds a clone of the client: after `quit` the app's own clones are the last
/// ones, so dropping them ends the session (an exited session, its last performance window sent)
/// instead of leaving it to be reported as an unclean exit on the next launch.
#[test]
fn quit_releases_the_dialog_hook_so_the_session_ends() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Recorder::default();
    let t = testing::client(&TelemetryConfig::new("demo", "1.0.0"), dir.path(), &fake, Duration::from_secs(2));
    t.pause_hangs_in_dialogs();
    t.command("doc.edit", true, Duration::from_millis(1));
    t.frame(Duration::from_millis(3));
    t.quit();
    drop(t);
    let ended = (0..200).any(|_| {
        std::thread::sleep(Duration::from_millis(10));
        fake.types().contains(&"session_end")
    });
    assert!(ended, "{:?}", fake.types());
}
