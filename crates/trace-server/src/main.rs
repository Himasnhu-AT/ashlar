//! The `ashlar-trace-server` binary: see `ashlar_trace_server::cli` for the subcommands.

fn main() -> std::process::ExitCode {
    ashlar_trace_server::cli::main(std::env::args().skip(1).collect())
}
