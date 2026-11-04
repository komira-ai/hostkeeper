//! The command line of the `hostkeeper` binary: `hostkeeper daemon` and its flags. Configuration
//! comes in as flags only, never from the environment (AGENTS.md S2); every default is a
//! constant here.
//!
//! Exit status: [`EXIT_FAILED`] when the daemon could not start or serve, `2` for a usage error
//! (as clap reports it), [`EXIT_CONFIG`] when the daemon config did not load, [`EXIT_LOCKED`]
//! when another daemon holds the state directory.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_config::names::{ServiceName, Version};
use hostkeeper_steps::{LaunchError, StepCall, StepExit, StepLauncher, SystemClock};

use crate::executor::Ports;
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
    run(Cli::parse())
}

/// Runs a parsed command line to its exit status.
pub fn run(cli: Cli) -> ExitCode {
    let Command::Daemon(flags) = cli.command;
    daemon(&flags.settings())
        .err()
        .map_or(ExitCode::SUCCESS, report)
}

fn report(error: StartError) -> ExitCode {
    eprintln!("hostkeeper daemon: {error}");
    ExitCode::from(error.exit_code())
}

/// Starts the daemon and serves it, returning only on an error.
fn daemon(settings: &Settings) -> Result<(), StartError> {
    let own_version = HostkeeperVersion::parse(env!("CARGO_PKG_VERSION"))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();
    let runtime = runtime.map_err(StartError::Runtime)?;
    runtime.block_on(start_and_serve(settings, own_version))
}

async fn start_and_serve(
    settings: &Settings,
    own_version: HostkeeperVersion,
) -> Result<(), StartError> {
    start::serve(start::startup(settings, host_ports(), own_version)?).await
}

/// The ports of this build. The host's step launcher and package store are not part of it yet,
/// and no order reaches the daemon yet (the heartbeat loop that brings them comes later): until
/// both are wired in, an order that did come would be refused, never run.
fn host_ports() -> Ports {
    Ports {
        launcher: Arc::new(Unwired),
        packages: Arc::new(Unwired),
        clock: Arc::new(SystemClock),
        deadman_poll_secs: Some(DEADMAN_POLL_SECS),
    }
}

/// The launcher and package store of a build that has neither: each refuses.
#[derive(Debug)]
pub struct Unwired;

/// What [`Unwired`] answers.
pub const UNWIRED: &str = "this build of hostkeeper has no step launcher or package store";

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
