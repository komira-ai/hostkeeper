//! The command line of the `hostkeeper` binary: `hostkeeper daemon` and its flags. Configuration
//! comes in as flags only, never from the environment (AGENTS.md S2); every default is a
//! constant here.
//!
//! The one value read from the environment is the address of systemd's notify socket, a
//! platform-set marker, and only `--notify-systemd` makes the daemon use it.
//!
//! Exit status: [`EXIT_FAILED`] when the daemon could not start or serve, `2` for a usage error
//! (as clap reports it), [`EXIT_CONFIG`] when the daemon config did not load, [`EXIT_LOCKED`]
//! when another daemon holds the state directory.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_placed::roots::SYSTEMD_UNIT_DIR;
use hostkeeper_placed::{RealFs, UnitControl, UnitControlError};
use hostkeeper_steps::{LaunchError, StepCall, StepExit, StepLauncher, SystemClock};

use crate::executor::{Placement, Ports};
use crate::notify::{NoNotifier, ReadyNotifier, SystemdNotifier};
use crate::packages::{PackageError, Packages, VersionDir};
use crate::start::{self, Settings, StartError};
use crate::verb::PackageDigest;

pub use crate::start::{EXIT_CONFIG, EXIT_FAILED, EXIT_LOCKED};

/// The default of `--config`.
pub const DEFAULT_CONFIG: &str = "/etc/hostkeeper/hostkeeper.toml";
/// The default of `--state-dir`.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/updater/state";
/// The default of `--packages-dir`.
pub const DEFAULT_PACKAGES_DIR: &str = "/var/lib/hostkeeper/packages";
/// The default of `--admin-socket`.
pub const DEFAULT_ADMIN_SOCKET: &str = "/run/hostkeeper/admin.sock";
/// The variable systemd sets for a `Type=notify` unit: the address of its notify socket.
pub const NOTIFY_SOCKET: &str = "NOTIFY_SOCKET";
/// How often each service's dead-man looks at its running step on a host, in seconds.
pub const DEADMAN_POLL_SECS: u64 = 1;

/// hostkeeper: keeps a host's services on the versions its coordinator orders.
#[derive(Debug, Parser)]
#[command(name = "hostkeeper", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the daemon: the one root process of the host.
    Daemon(DaemonFlags),
}

/// The flags of `hostkeeper daemon`.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct DaemonFlags {
    /// The daemon config: root-owned, not writable by group or others.
    #[arg(long, default_value = DEFAULT_CONFIG)]
    pub config: PathBuf,
    /// The step records, params files and held halts, a directory per service, and the lock.
    #[arg(long, default_value = DEFAULT_STATE_DIR)]
    pub state_dir: PathBuf,
    /// The packages, a directory per service with its versions and `current` and `previous`.
    #[arg(long, default_value = DEFAULT_PACKAGES_DIR)]
    pub packages_dir: PathBuf,
    /// The root-only admin socket, made mode 0600 in a directory of mode 0700.
    #[arg(long, default_value = DEFAULT_ADMIN_SOCKET)]
    pub admin_socket: PathBuf,
    /// Tell systemd the daemon is ready (`READY=1` on the notify socket systemd sets for a unit
    /// of `Type=notify`) once its boot work is over.
    #[arg(long)]
    pub notify_systemd: bool,
}

impl DaemonFlags {
    /// The settings of these flags; the daemon config is trusted when root owns it.
    pub fn settings(&self) -> Settings {
        Settings {
            config: self.config.clone(),
            state_dir: self.state_dir.clone(),
            packages_dir: self.packages_dir.clone(),
            admin_socket: self.admin_socket.clone(),
            trusted_uid: 0,
        }
    }
}

/// The entry point of the `hostkeeper` binary.
pub fn main() -> ExitCode {
    run_with(Cli::parse(), notify_socket())
}

/// The address of the notify socket systemd set for the process, if any: a platform-set marker,
/// not configuration (AGENTS.md S2). It is used only with `--notify-systemd`.
#[expect(
    clippy::disallowed_methods,
    reason = "NOTIFY_SOCKET is the platform-set marker of a Type=notify unit, which AGENTS.md S2 allows; the flag --notify-systemd decides whether it is used"
)]
fn notify_socket() -> Option<OsString> {
    std::env::var_os(NOTIFY_SOCKET)
}

/// Runs a parsed command line to its exit status, with no notify socket.
pub fn run(cli: Cli) -> ExitCode {
    run_with(cli, None)
}

/// Runs a parsed command line to its exit status; `notify_socket` is the address of the notify
/// socket systemd set for the process, used with `--notify-systemd` only.
pub fn run_with(cli: Cli, notify_socket: Option<OsString>) -> ExitCode {
    let Command::Daemon(flags) = cli.command;
    let started = notifier(&flags, notify_socket)
        .and_then(|notifier| daemon(&flags.settings(), notifier.as_ref()));
    started.err().map_or(ExitCode::SUCCESS, report)
}

/// The readiness notifier the flags ask for: systemd's with `--notify-systemd`, refused when no
/// notify socket was set; none without it.
pub fn notifier(
    flags: &DaemonFlags,
    notify_socket: Option<OsString>,
) -> Result<Box<dyn ReadyNotifier>, StartError> {
    match (flags.notify_systemd, notify_socket) {
        (false, _) => Ok(Box::new(NoNotifier)),
        (true, Some(socket)) => Ok(Box::new(SystemdNotifier::new(socket))),
        (true, None) => Err(StartError::NoNotifySocket),
    }
}

fn report(error: StartError) -> ExitCode {
    eprintln!("hostkeeper daemon: {error}");
    ExitCode::from(error.exit_code())
}

/// Starts the daemon and serves it, returning only on an error.
fn daemon(settings: &Settings, notifier: &dyn ReadyNotifier) -> Result<(), StartError> {
    let own_version = HostkeeperVersion::parse(env!("CARGO_PKG_VERSION"))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();
    let runtime = runtime.map_err(StartError::Runtime)?;
    runtime.block_on(start_and_serve(settings, own_version, notifier))
}

async fn start_and_serve(
    settings: &Settings,
    own_version: HostkeeperVersion,
    notifier: &dyn ReadyNotifier,
) -> Result<(), StartError> {
    let started = start::startup_notifying(settings, host_ports(), own_version, notifier)?;
    start::serve(started).await
}

/// The ports of this build. The host's step launcher, package store and service manager are not
/// part of it yet, and no order reaches the daemon yet (the heartbeat loop that brings them comes
/// later): until they are wired in, an order that did come would be refused or fail, never run.
/// Placement reads and writes the host's own paths (`RealFs` at `/`), units in
/// `/etc/systemd/system`.
fn host_ports() -> Ports {
    Ports {
        launcher: Arc::new(Unwired),
        packages: Arc::new(Unwired),
        placement: Placement {
            unit_dir: PathBuf::from(SYSTEMD_UNIT_DIR),
            fs: Arc::new(RealFs::new(std::path::Path::new("/"))),
            units: Arc::new(Unwired),
        },
        clock: Arc::new(SystemClock),
        deadman_poll_secs: Some(DEADMAN_POLL_SECS),
    }
}

/// The launcher, package store and service manager of a build that has none: each refuses.
#[derive(Debug)]
pub struct Unwired;

/// What [`Unwired`] answers.
pub const UNWIRED: &str =
    "this build of hostkeeper has no step launcher, package store or service manager";

impl StepLauncher for Unwired {
    fn run(&self, _call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        Err(LaunchError::NoStart(UNWIRED.to_owned()))
    }

    fn stop(&self, _unit: &str) -> Result<(), LaunchError> {
        Err(LaunchError::Launcher(UNWIRED.to_owned()))
    }
}

impl Packages for Unwired {
    fn version_dir(
        &self,
        _service: &ServiceName,
        _version: &Version,
        _digest: &PackageDigest,
    ) -> Result<VersionDir, PackageError> {
        Err(PackageError::Other(UNWIRED.to_owned()))
    }
}

impl UnitControl for Unwired {
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        Err(unwired_units("daemon-reload"))
    }

    fn stop_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        Err(unwired_units(&format!("stop {unit}")))
    }
}

fn unwired_units(action: &str) -> UnitControlError {
    UnitControlError {
        action: action.to_owned(),
        why: UNWIRED.to_owned(),
    }
}
