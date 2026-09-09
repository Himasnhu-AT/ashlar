//! The `tracer-server` binary: see `tracer_server::cli` for the subcommands.

fn main() -> std::process::ExitCode {
    tracer_server::cli::main(std::env::args().skip(1).collect())
}
