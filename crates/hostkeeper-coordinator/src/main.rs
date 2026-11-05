//! The `hostkeeper-coordinator` binary: see [`hostkeeper_coordinator::config`].

fn main() -> std::process::ExitCode {
    hostkeeper_coordinator::config::main(std::env::args_os().collect())
}
