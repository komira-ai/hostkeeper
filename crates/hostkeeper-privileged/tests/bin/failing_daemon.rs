//! `hostkeeper-failing-daemon`: the `hostkeeper` binary of this build made to fail its start, in
//! the cross-release rig (`tests/self_update_across_releases.rs`), a program of the tests, not
//! shipped.
//!
//! `daemon` is the only verb, as the `hostkeeper` binary's (`hostkeeper_daemon::cli::Cli`). It
//! starts nothing of the shipped daemon: it writes one line to standard error ([`REFUSED`]) and
//! exits 1, a new binary that cannot start.

use std::process::ExitCode;

use clap::Parser;
use hostkeeper_daemon::cli::{Cli, Command};

/// What `daemon` writes to standard error before it exits 1.
const REFUSED: &str = "hostkeeper-failing-daemon: this build refuses to start";
/// The exit status of `daemon` (the daemon's `EXIT_FAILED`).
const EXIT_REFUSED: u8 = 1;

fn main() -> ExitCode {
    let Cli {
        command: Command::Daemon(_),
    } = Cli::parse();
    eprintln!("{REFUSED}");
    ExitCode::from(EXIT_REFUSED)
}
