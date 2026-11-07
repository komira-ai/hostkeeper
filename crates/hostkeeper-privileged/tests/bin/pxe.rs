//! `hostkeeper-privileged-pxe`: the `hostkeeper-pxe` binary for the network-install tests
//! (`tests/net_install.rs`, `tests/net_install_arm64.rs`), which run its `fetch` and start its
//! `serve` as a child process and need its path (`CARGO_BIN_EXE_hostkeeper-privileged-pxe`). On
//! Linux it only forwards to the command line of `hostkeeper_pxe`, as the `hostkeeper-pxe` binary
//! does; `hostkeeper-pxe` is Linux's only, so elsewhere (the macOS job builds every program of
//! the crate) it says so and fails. A program of the tests, not shipped.

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    hostkeeper_pxe::cli::main(std::env::args_os().collect())
}

#[cfg(not(target_os = "linux"))]
fn main() -> std::process::ExitCode {
    eprintln!("hostkeeper-privileged-pxe: hostkeeper-pxe runs on Linux only");
    std::process::ExitCode::FAILURE
}
