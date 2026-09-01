//! `notes-cli commands | exec <id> [json] | run <file> | mcp | serve | version`, and
//! `--connect PORT` to drive the running app.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

fn main() -> std::process::ExitCode {
    ashlar::app::cli_main(notes::app())
}
