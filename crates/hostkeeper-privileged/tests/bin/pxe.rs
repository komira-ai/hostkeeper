//! `hostkeeper-privileged-pxe`: the `hostkeeper-pxe` binary for the network-install tests
//! (`tests/net_install.rs`, `tests/net_install_arm64.rs`), which run its `fetch` and start its
//! `serve` as a child process and need its path (`CARGO_BIN_EXE_hostkeeper-privileged-pxe`). It
//! only forwards to the command line of `hostkeeper_pxe`, as the `hostkeeper-pxe` binary does; a
//! program of the tests, not shipped.

fn main() -> std::process::ExitCode {
    hostkeeper_pxe::cli::main(std::env::args_os().collect())
}
