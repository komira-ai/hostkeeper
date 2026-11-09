//! `hostkeeper-failing-daemon`: the `hostkeeper` binary of this build made to fail its start, in
//! the cross-release rig (`tests/self_update_across_releases.rs`), a program of the tests, not
//! shipped.
//!
//! It parses the `hostkeeper` binary's command line (`hostkeeper_daemon::cli::Cli`) and takes
//! only its `daemon` verb; any other is a usage error. It starts nothing of the shipped daemon:
//! it writes one line to standard error ([`REFUSED`]) and exits 1, a new binary that cannot
//! start.

use std::process::ExitCode;

use clap::Parser;
use hostkeeper_daemon::cli::{Cli, Command};

/// What `daemon` writes to standard error before it exits 1.
const REFUSED: &str = "hostkeeper-failing-daemon: this build refuses to start";
/// The exit status of `daemon` (the daemon's `EXIT_FAILED`).
const EXIT_REFUSED: u8 = 1;
/// The exit status of any other verb (`2`, a usage error, as clap's).
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let Cli { command } = Cli::parse();
    if !matches!(command, Command::Daemon(_)) {
        eprintln!("hostkeeper-failing-daemon: `daemon` is the only verb of this program");
        return ExitCode::from(EXIT_USAGE);
    }
    eprintln!("{REFUSED}");
    ExitCode::from(EXIT_REFUSED)
}
