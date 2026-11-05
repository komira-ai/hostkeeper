//! `hostkeeper-device-daemon`: the `hostkeeper` binary for the macOS device tests
//! (`tests/device_mac.rs`), which start the daemon as a launchd job and need its path
//! (`CARGO_BIN_EXE_hostkeeper-device-daemon`). It only forwards to the daemon's command line, as
//! the `hostkeeper` binary does; a program of the tests, not shipped.

fn main() -> std::process::ExitCode {
    hostkeeper_daemon::cli::main()
}
