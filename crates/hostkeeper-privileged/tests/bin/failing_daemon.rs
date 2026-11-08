//! `hostkeeper-failing-daemon`: the `hostkeeper` binary of this build made to fail its start, in
//! the cross-release rig (`tests/self_update_across_releases.rs`), a program of the tests, not
//! shipped.
//!
//! Every verb but `daemon` is forwarded to the daemon's command line as the `hostkeeper` binary
//! forwards it (`hostkeeper_daemon::cli::run_with`), so an older daemon's probe and the hook of
//! hostkeeper's package find the shipped answers, and the update reaches its restart.
//!
//! `daemon` does what the shipped daemon does first, and nothing after: it adds this start to the
//! start log (`hostkeeper_daemon::cli::note_start`, which the fallback on macOS judges the burst
//! by), as the version named by the file it runs from
//! (`hostkeeper_privileged::across::program_version`), writes one line to standard error
//! ([`REFUSED`]) and exits 1: a new binary that cannot start.

use std::process::ExitCode;

use clap::Parser;
use hostkeeper_daemon::adapters::host_clock;
use hostkeeper_daemon::cli::{self, Cli, Command, SystemdVars};
use hostkeeper_launchd::Platform;
use hostkeeper_privileged::across::program_version;

/// What `daemon` writes to standard error before it exits 1.
const REFUSED: &str = "hostkeeper-failing-daemon: this build refuses to start";
/// The exit status of `daemon` (the daemon's `EXIT_FAILED`).
const EXIT_REFUSED: u8 = 1;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon(flags) => {
            match std::env::current_exe()
                .map_err(|e| format!("its own executable cannot be read: {e}"))
                .and_then(|exe| program_version(&exe))
            {
                Ok(own) => {
                    cli::note_start(&flags.settings(), Platform::host().kind, own, &host_clock())
                }
                Err(why) => eprintln!("hostkeeper-failing-daemon: no start logged: {why}"),
            }
            eprintln!("{REFUSED}");
            ExitCode::from(EXIT_REFUSED)
        }
        // Run with nothing systemd set: no verb but `daemon` reads it.
        command => cli::run_with(Cli { command }, SystemdVars::default()),
    }
}
