//! `ashlar-debug`: see the library documentation and `ashlar-debug --help`.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

fn main() -> std::process::ExitCode {
    ashlar_debug::main()
}
