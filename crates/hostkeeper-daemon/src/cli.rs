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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_package::gh::{GhVerifier, OwnedBy, SystemRunner};
use hostkeeper_package::net::{NetFetcher, UreqTransport};
use hostkeeper_package::ports::{Attest, Attested, Verifier, VerifyError};
use hostkeeper_placed::roots::{STATE_DIR, SYSTEMD_UNIT_DIR};
use hostkeeper_placed::{RealFs, Roots, UnitControl, UnitControlError};
use hostkeeper_steps::{Clock, LaunchError, StepCall, StepExit, StepLauncher, SystemClock};
use hostkeeper_unpack::Owner;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

use crate::executor::{Placement, Ports};
use crate::files;
use crate::notify::{NoNotifier, ReadyNotifier, SystemdNotifier};
use crate::observer::host_observer;
use crate::prune::NoPrune;
use crate::real_packages::RealPackages;
use crate::start::{self, Settings, StartError};

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
/// The host's CA bundle (PEM): the roots the package fetch's HTTPS client trusts.
pub const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// The pinned `gh` that checks a package's attestation.
pub const GH_BINARY: &str = "/usr/lib/hostkeeper/gh";
/// The Sigstore trusted root, as `gh attestation trusted-root` prints it.
pub const SIGSTORE_TRUSTED_ROOT: &str = "/usr/lib/hostkeeper/trusted_root.json";
/// The directory `gh` runs in, under the state directory.
pub const GH_HOME: &str = ".gh";
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
    let started = start_with(settings, &HostFiles::host(), own_version, notifier)?;
    start::serve(started).await
}

/// Starts the daemon on the ports of the host's build, reading the host's own files from
/// `files`, and returns it without serving: the config of `settings` is loaded, the ports are
/// built ([`host_ports`]) and the start runs ([`start::startup_notifying`]).
pub fn start_with(
    settings: &Settings,
    files: &HostFiles,
    own_version: HostkeeperVersion,
    notifier: &dyn ReadyNotifier,
) -> Result<start::Started, StartError> {
    let config = start::load_config(settings)?;
    host_ports(&config, settings, files, own_version)
        .and_then(|ports| start::startup_notifying(settings, ports, own_version, notifier))
}

/// The files outside the daemon's own directories that the host's ports read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFiles {
    /// The CA bundle, PEM: the roots the package fetch trusts.
    pub ca_bundle: PathBuf,
    /// The pinned `gh`.
    pub gh: PathBuf,
    /// The Sigstore trusted root.
    pub trusted_root: PathBuf,
}

impl HostFiles {
    /// Where a host keeps them: [`CA_BUNDLE`], [`GH_BINARY`] and [`SIGSTORE_TRUSTED_ROOT`].
    pub fn host() -> HostFiles {
        HostFiles {
            ca_bundle: PathBuf::from(CA_BUNDLE),
            gh: PathBuf::from(GH_BINARY),
            trusted_root: PathBuf::from(SIGSTORE_TRUSTED_ROOT),
        }
    }
}

/// The DER certificates of the PEM bundle at `path`. A bundle that cannot be opened has none,
/// and a section that cannot be read is skipped: a fetch with no root to trust ends
/// `fetch_failed`, and the daemon still starts.
pub fn tls_roots(path: &Path) -> Vec<Vec<u8>> {
    CertificateDer::pem_file_iter(path).map_or_else(
        |_| Vec::new(),
        |sections| {
            sections
                .filter_map(Result::ok)
                .map(|der| der.as_ref().to_vec())
                .collect()
        },
    )
}

/// The [`Verifier`] of a host: `gh` checking the attestation offline, made for each package
/// checked (the verifier of the package library is not shareable between threads).
pub struct HostVerifier {
    gh: PathBuf,
    trusted_root: PathBuf,
    home: PathBuf,
    clock: Arc<dyn Clock>,
}

impl HostVerifier {
    /// A verifier that runs `gh` with `trusted_root`, in `home`, deadlines read on `clock`. `gh`
    /// and the root must be owned by root and not writable by group or others.
    pub fn new(
        gh: PathBuf,
        trusted_root: PathBuf,
        home: PathBuf,
        clock: Arc<dyn Clock>,
    ) -> HostVerifier {
        HostVerifier {
            gh,
            trusted_root,
            home,
            clock,
        }
    }
}

impl Verifier for HostVerifier {
    fn verify(&self, request: &Attest<'_>) -> Result<Attested, VerifyError> {
        GhVerifier::new(
            self.gh.clone(),
            self.trusted_root.clone(),
            self.home.clone(),
            Box::new(SystemRunner::new(Arc::clone(&self.clock))),
            Box::new(OwnedBy { uid: 0 }),
        )
        .verify(request)
    }
}

/// The ports of this build. The package store is the real one ([`RealPackages`]: HTTPS over the
/// CA bundle of `files`, `gh` for the attestation, unpacked into `--packages-dir` as root); the
/// observer looks through `systemctl`, `/proc` and TCP ([`host_observer`]). The host's step
/// launcher and service manager are not part of this build yet, and no order reaches the daemon
/// yet (the heartbeat loop that brings them comes later): until they are wired in, an order that
/// did come would be refused when it reached them, never run. Placed paths are linked under the
/// host's own directories, `--packages-dir` being the packages directory; the prune of step 18
/// of a force deletes nothing yet. The directory `gh` runs in (`<state-dir>/.gh`) is made mode 0700
/// here. `Err` when that directory cannot be made or the packages directory cannot be opened.
pub fn host_ports(
    config: &DaemonConfig,
    settings: &Settings,
    files: &HostFiles,
    own_version: HostkeeperVersion,
) -> Result<Ports, StartError> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let fetcher = NetFetcher::new(
        UreqTransport::new(&tls_roots(&files.ca_bundle)),
        Arc::clone(&clock),
    );
    let home = settings.state_dir.join(GH_HOME);
    files::make_private(&home).map_err(|source| StartError::Io {
        path: home.clone(),
        source,
    })?;
    let verifier = HostVerifier::new(
        files.gh.clone(),
        files.trusted_root.clone(),
        home,
        Arc::clone(&clock),
    );
    let packages = RealPackages::open(
        Box::new(fetcher),
        Box::new(verifier),
        Arc::clone(&clock),
        config,
        &settings.packages_dir,
        Owner::ROOT,
    )
    .map_err(|source| StartError::Io {
        path: settings.packages_dir.clone(),
        source,
    })?;
    Ok(Ports {
        // The production step launcher, SystemdLauncher<SystemdRunProcess>, replaces Unwired
        // here once komira-ai/hostkeeper#9 lands.
        launcher: Arc::new(Unwired),
        packages: Arc::new(packages),
        clock,
        deadman_poll_secs: Some(DEADMAN_POLL_SECS),
        units: Arc::new(Unwired),
        placement: Placement {
            roots: Roots::new(
                Path::new(STATE_DIR),
                Path::new(SYSTEMD_UNIT_DIR),
                &settings.packages_dir,
            ),
            fs: Arc::new(RealFs::new(Path::new("/"))),
        },
        prune: Arc::new(NoPrune),
        observer: Arc::new(host_observer(&settings.packages_dir, own_version)),
    })
}

/// The step launcher and service manager of a build that has none: each refuses.
#[derive(Debug)]
pub struct Unwired;

/// What [`Unwired`] answers.
pub const UNWIRED: &str = "this build of hostkeeper has no step launcher or service manager";

impl StepLauncher for Unwired {
    fn run(&self, _call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        Err(LaunchError::NoStart(UNWIRED.to_owned()))
    }

    fn stop(&self, _unit: &str) -> Result<(), LaunchError> {
        Err(LaunchError::Launcher(UNWIRED.to_owned()))
    }
}

impl Unwired {
    fn refuse<T>(action: &str) -> Result<T, UnitControlError> {
        Err(UnitControlError {
            action: action.to_owned(),
            why: UNWIRED.to_owned(),
        })
    }
}

impl UnitControl for Unwired {
    fn daemon_reload(&self) -> Result<(), UnitControlError> {
        Unwired::refuse("daemon-reload")
    }

    fn stop_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        Unwired::refuse(&format!("stop {unit}"))
    }

    fn start_unit(&self, unit: &str) -> Result<(), UnitControlError> {
        Unwired::refuse(&format!("start {unit}"))
    }

    fn is_active(&self, unit: &str) -> Result<bool, UnitControlError> {
        Unwired::refuse(&format!("is-active {unit}"))
    }
}
