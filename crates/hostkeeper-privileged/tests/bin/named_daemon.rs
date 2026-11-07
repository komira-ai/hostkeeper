//! `hostkeeper-named-daemon`: the `hostkeeper` binary of this build in the cross-release rig
//! (`tests/self_update_across_releases.rs`), a program of the tests, not shipped.
//!
//! Every verb but `daemon` is forwarded to the daemon's command line as the `hostkeeper` binary
//! forwards it (`hostkeeper_daemon::cli::run_with`): `self-update-protocol`, which an older
//! daemon's probe asks of it, `update-hook` and its step words, which an older daemon runs as the
//! hook of hostkeeper's package, and `self-update-fallback` are the shipped code.
//!
//! `daemon` is the shipped daemon as `hostkeeper daemon` runs it (`hostkeeper_daemon::cli`): the
//! start log, the host's files, ports and adapters (the release channel, the attestation check
//! with the host's `gh` and trusted root, the coordinator over mutual TLS, systemd or launchd),
//! the start and the heartbeat and enrollment loops, served until it asks to exit for its own
//! update. One thing differs: its version is the name of the file it runs from, its links
//! resolved (`hostkeeper_privileged::across::program_version`), where the shipped binary's is the
//! one it was built with. The binary directory keeps version `<v>` at `versions/<v>`, so this
//! build staged under two names runs as two versions, both newer than the pinned release.
//!
//! It reads no environment variable but the two systemd sets for a `Type=notify` unit with a
//! watchdog, as the shipped binary does.

use std::process::ExitCode;

use clap::Parser;
use hostkeeper_daemon::adapters::host_clock;
use hostkeeper_daemon::cli::{self, Cli, Command, DaemonFlags, HostFiles, SystemdVars};
use hostkeeper_daemon::start::{self, StartError};
use hostkeeper_launchd::Platform;
use hostkeeper_privileged::across::program_version;

/// The program's name, at the start of what it says on standard error.
const PROGRAM: &str = "hostkeeper-named-daemon";
/// The exit status of a start this program refuses (`2`, a usage error, as clap's).
const EXIT_USAGE: u8 = 2;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Daemon(flags) => daemon(&flags),
        command => cli::run_with(Cli { command }, systemd_vars()),
    }
}

/// What systemd set for the process, the notify socket's address and the watchdog's timeout:
/// what `hostkeeper_daemon::cli` reads for `--notify-systemd`.
#[expect(
    clippy::disallowed_methods,
    reason = "NOTIFY_SOCKET and WATCHDOG_USEC are the platform-set markers of a Type=notify unit and of its watchdog, which AGENTS.md S2 allows; the shipped binary reads them the same way"
)]
fn systemd_vars() -> SystemdVars {
    SystemdVars {
        notify_socket: std::env::var_os(cli::NOTIFY_SOCKET),
        watchdog_usec: std::env::var_os(cli::WATCHDOG_USEC),
    }
}

/// `daemon`, as the module says.
fn daemon(flags: &DaemonFlags) -> ExitCode {
    let own = match std::env::current_exe()
        .map_err(|e| format!("its own executable cannot be read: {e}"))
        .and_then(|exe| program_version(&exe))
    {
        Ok(own) => own,
        Err(why) => {
            eprintln!("{PROGRAM}: {why}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let systemd = systemd_vars();
    let period = flags.link.heartbeat_secs;
    let watchdog = cli::watchdog(flags, &systemd);
    let settings = flags.settings();
    let served = cli::notifier(flags, systemd.notify_socket).and_then(|notifier| {
        let platform = Platform::host();
        cli::note_start(&settings, platform.kind, own, &host_clock());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(StartError::Runtime)?;
        runtime.block_on(async {
            let files = HostFiles::host();
            let started = cli::start_with(&settings, &files, own, notifier.as_ref(), &platform)?;
            let links = cli::host_links(started.daemon(), &files, period, platform.kind, watchdog);
            start::serve(started, links).await
        })
    });
    ExitCode::from(cli::exit_status(&served))
}
