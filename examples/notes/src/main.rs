//! Notes in a window. `--control <port>` (or `NOTES_CONTROL_PORT`) lets an agent drive it.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

fn main() -> std::process::ExitCode {
    match notes::app().run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("notes: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
