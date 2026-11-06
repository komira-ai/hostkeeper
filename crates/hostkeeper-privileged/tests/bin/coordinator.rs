//! `hostkeeper-privileged-coordinator`: the `hostkeeper-coordinator` binary for the first-boot
//! tests (`tests/first_boot.rs`, `tests/install_uninstall.rs`), which start it as a child process
//! and need its path (`CARGO_BIN_EXE_hostkeeper-privileged-coordinator`). It only forwards to the
//! coordinator's command line, as the `hostkeeper-coordinator` binary does; a program of the
//! tests, not shipped.

fn main() -> std::process::ExitCode {
    hostkeeper_coordinator::config::main(std::env::args_os().collect())
}
