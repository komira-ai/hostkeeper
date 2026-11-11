//! The `pxe` binary: see [`hostkeeper_pxe::cli`]. This file only forwards to it.

fn main() -> std::process::ExitCode {
    hostkeeper_pxe::cli::main(std::env::args_os().collect())
}
