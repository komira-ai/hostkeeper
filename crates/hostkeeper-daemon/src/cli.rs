//! The command line of the `hostkeeper` binary: `hostkeeper daemon` and its flags,
//! `hostkeeper ctl`, which hands every argument after it to `hostkeeper-ctl`, `hostkeeper ci`, the
//! CI client a service's release job runs once it has verified this binary (`hostkeeper_ci`,
//! docs/deploy/ci-client.md), and the two plumbing verbs of hostkeeper's own update (design
//! section 6.6), which no operator runs: `hostkeeper self-update-fallback --dir <dir>`, what the
//! generated fallback unit runs, and on a Mac the fallback timer job ([`crate::fallback`]), and
//! `hostkeeper self-update-protocol`,
//! which prints this binary's self-update protocol and does nothing else
//! ([`crate::selfupdate::protocol_line`]). Configuration comes in as flags only, never from the
//! environment (AGENTS.md S2); every default is a constant here.
//!
//! The one value read from the environment is the address of systemd's notify socket, a
//! platform-set marker, and only `--notify-systemd` makes the daemon use it.
//!
//! Exit status of `hostkeeper daemon`: [`EXIT_FAILED`] when the daemon could not start or serve,
//! `2` for a usage error (as clap reports it), [`EXIT_CONFIG`] when the daemon config did not
//! load, [`EXIT_LOCKED`] when another daemon holds the state directory. The exit status of
//! `hostkeeper ctl` is the code `hostkeeper_ctl::exit` assigns to the call, `2` for a usage error
//! among them.
//!
//! `--version` prints `hostkeeper X.Y.Z+<12 hex digits of the commit>` for a release build and
//! `hostkeeper X.Y.Z` for a developer build ([`crate::stamp`]).

use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use hostkeeper_config::daemon::DaemonConfig;
use hostkeeper_config::manifest::HostkeeperVersion;
use hostkeeper_ctl::transport::Network;
use hostkeeper_ctl::{Connect, Io};
use hostkeeper_enroll::{
    Enroller, GrpcEnroll, OsEntropy, PkiDir, Settings as EnrollSettings, SystemWallClock,
};
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_link::{ClockSleeper, LoopFlags, PkiCertSource, StopFlag, ThreadDispatch};
use hostkeeper_package::gh::{GhVerifier, OwnedBy, SystemRunner};
use hostkeeper_package::net::{NetFetcher, UreqTransport};
use hostkeeper_package::ports::{Attest, Attested, Verifier, VerifyError};
use hostkeeper_placed::roots::STATE_DIR;
use hostkeeper_placed::{RealFs, Roots, UnitControl, UnitControlError};
use hostkeeper_steps::{Clock, LaunchError, StepCall, StepExit, StepLauncher, SystemClock};
use hostkeeper_unpack::Owner;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

use crate::adapters::{Adapters, host_claims};
use crate::executor::{Daemon, Placement, Ports};
use crate::files;
use crate::links::{DaemonLog, Links, NoLink};
use crate::mac::HostIoreg;
use crate::notify::{NoNotifier, ReadyNotifier, SystemdNotifier};
use crate::observer::observer_with;
use crate::prune::{LayoutPrune, NoLockPoint};
use crate::real_packages::RealPackages;
use crate::start::{self, Settings, StartError};
use crate::unix_clock::SystemUnixClock;

pub use crate::start::{EXIT_CONFIG, EXIT_FAILED, EXIT_LOCKED};

/// The program name `hostkeeper ctl` shows in its usage and help.
pub const CTL_PROGRAM: &str = "hostkeeper ctl";
/// The default of `--config`.
pub const DEFAULT_CONFIG: &str = "/etc/hostkeeper/hostkeeper.toml";
/// The default of `--state-dir`.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/updater/state";
/// The default of `--packages-dir`.
pub const DEFAULT_PACKAGES_DIR: &str = "/var/lib/hostkeeper/packages";
/// The default of `--admin-socket`: the host platform's (`hostkeeper_launchd::Platform`'s
/// `admin_socket`), `/var/run/hostkeeper/admin.sock` on macOS.
pub const DEFAULT_ADMIN_SOCKET: &str = if cfg!(target_os = "macos") {
    "/var/run/hostkeeper/admin.sock"
} else {
    "/run/hostkeeper/admin.sock"
};
/// The variable systemd sets for a `Type=notify` unit: the address of its notify socket.
pub const NOTIFY_SOCKET: &str = "NOTIFY_SOCKET";
/// The CA bundle (PEM) of a Debian-family Linux host: the roots the package fetch's HTTPS client
/// trusts. A Linux host whose bundle is neither this nor [`CA_BUNDLE_RHEL`] is given this path.
pub const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// The CA bundle of a RHEL-family Linux host.
pub const CA_BUNDLE_RHEL: &str = "/etc/pki/tls/certs/ca-bundle.crt";
/// The CA bundle of a Mac, the system roots macOS keeps as PEM.
pub const CA_BUNDLE_MACOS: &str = "/etc/ssl/cert.pem";
/// The pinned `gh` that checks a package's attestation, on a Linux host.
pub const GH_BINARY: &str = "/usr/lib/hostkeeper/gh";
/// The pinned `gh` on a Mac, where the installer pkg puts it (`/usr` itself is read-only there).
pub const GH_BINARY_MACOS: &str = "/usr/local/lib/hostkeeper/gh";
/// The Sigstore trusted root, as `gh attestation trusted-root` prints it, on a Linux host.
pub const SIGSTORE_TRUSTED_ROOT: &str = "/usr/lib/hostkeeper/trusted_root.json";
/// The Sigstore trusted root on a Mac, where the installer pkg puts it.
pub const SIGSTORE_TRUSTED_ROOT_MACOS: &str = "/usr/local/lib/hostkeeper/trusted_root.json";
/// The directory `gh` runs in, under the state directory.
pub const GH_HOME: &str = ".gh";
/// How often each service's dead-man looks at its running step on a host, in seconds.
pub const DEADMAN_POLL_SECS: u64 = 1;
/// The pki directory: the host's key and its certificate generations, `current` the one in use
/// (design section 10).
pub const PKI_DIR: &str = "/etc/updater/pki";
/// How long the enrollment loop's connection, handshake and each call to the coordinator may take.
pub const ENROLL_TIMEOUT: Duration = Duration::from_secs(30);

/// hostkeeper: keeps a host's services on the versions its coordinator orders.
#[derive(Debug, Parser)]
#[command(name = "hostkeeper", version = crate::stamp::version())]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the daemon: the one root process of the host.
    Daemon(DaemonFlags),
    /// Call the coordinator's API: one command per RPC, generated from the protobuf services.
    /// `hostkeeper ctl --help` lists them.
    #[command(disable_help_flag = true)]
    Ctl {
        /// Everything after `ctl`, unparsed: `hostkeeper-ctl` parses it, `--help` included.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, num_args = 0.., value_name = "ARGS")]
        args: Vec<OsString>,
    },
    /// The CI client: set a service's desired version from its release job and wait for the
    /// fleet (docs/deploy/ci-client.md).
    #[command(subcommand)]
    Ci(hostkeeper_ci::CiCommand),
    /// Put the previous hostkeeper binary back after the new one failed to start, and record the
    /// update failed and its version bad (run by the generated fallback unit).
    SelfUpdateFallback(FallbackFlags),
    /// Print this binary's self-update protocol and nothing else (run by the daemon before it
    /// stages a new binary).
    SelfUpdateProtocol,
}

/// The flags of `hostkeeper self-update-fallback`.
#[derive(Clone, Debug, PartialEq, Eq, Args)]
pub struct FallbackFlags {
    /// The binary directory: `versions/`, `current` and `previous`, in the state directory of
    /// the service `hostkeeper`.
    #[arg(long)]
    pub dir: PathBuf,
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
    /// The heartbeat loop's flag: `--heartbeat-secs`.
    #[command(flatten)]
    pub link: LoopFlags,
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
    let flags = match cli.command {
        Command::Daemon(flags) => flags,
        Command::Ctl { args } => return ExitCode::from(ctl_on_terminal(args)),
        Command::SelfUpdateFallback(flags) => return crate::fallback::main(&flags.dir),
        Command::SelfUpdateProtocol => {
            print!("{}", crate::selfupdate::protocol_line());
            return ExitCode::SUCCESS;
        }
        Command::Ci(command) => return hostkeeper_ci::run(command),
    };
    let period = flags.link.heartbeat_secs;
    let started = notifier(&flags, notify_socket)
        .and_then(|notifier| daemon(&flags.settings(), period, notifier.as_ref()));
    started.err().map_or(ExitCode::SUCCESS, report)
}

/// Runs `hostkeeper ctl` with `args`, the arguments after `ctl`, and returns its exit code
/// (`hostkeeper_ctl::exit`). `connect` makes the transport of a call.
pub fn ctl(args: Vec<OsString>, connect: &dyn Connect, io: Io<'_>) -> u8 {
    hostkeeper_ctl::run(
        std::iter::once(OsString::from(CTL_PROGRAM)).chain(args),
        connect,
        io,
    )
}

/// [`ctl`] on the process's own streams and the real network.
fn ctl_on_terminal(args: Vec<OsString>) -> u8 {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    ctl(
        args,
        &Network,
        Io {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
    )
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

/// Starts the daemon and serves it, beating every `period` seconds, returning only on an error
/// or for its own update.
fn daemon(
    settings: &Settings,
    period: NonZeroU64,
    notifier: &dyn ReadyNotifier,
) -> Result<(), StartError> {
    let own_version = HostkeeperVersion::parse(env!("CARGO_PKG_VERSION"))?;
    note_start(settings, Platform::host().kind, own_version, &SystemClock);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();
    let runtime = runtime.map_err(StartError::Runtime)?;
    runtime.block_on(start_and_serve(settings, period, own_version, notifier))
}

/// Adds this start to the start log the fallback on macOS judges the burst by
/// ([`crate::fallback::record_start`]), before anything that can fail; a log that cannot be
/// written is said on standard error and does not stop the start.
pub fn note_start(
    settings: &Settings,
    kind: PlatformKind,
    own_version: HostkeeperVersion,
    clock: &dyn Clock,
) {
    let own = own_version.to_string();
    if let Err(error) = crate::fallback::record_start(&settings.state_dir, kind, &own, clock) {
        eprintln!("hostkeeper daemon: this start is not in the start log: {error}");
    }
}

async fn start_and_serve(
    settings: &Settings,
    period: NonZeroU64,
    own_version: HostkeeperVersion,
    notifier: &dyn ReadyNotifier,
) -> Result<(), StartError> {
    let files = HostFiles::host();
    let platform = Platform::host();
    let started = start_with(settings, &files, own_version, notifier, &platform)?;
    let links = host_links(started.daemon(), &files, period, platform.kind);
    start::serve(started, links).await
}

/// The enrollment and heartbeat loops of a host ([`Links`]), over the pki directory of `files`:
///
/// - the enrollment loop's enroller is made on its own worker ([`host_enroller`]): gRPC with TLS
///   to the daemon config's `coordinator`, pinned to its `ca_sha256`, the key and the
///   certificate generations in the pki directory, the claims of a host of `kind`
///   ([`host_claims`]);
/// - the heartbeat loop reads the certificate and key of the pki directory's `current` link at
///   every heartbeat ([`PkiCertSource`]), and beats every `period` seconds on the daemon's clock;
///   this build has no coordinator link yet ([`NoLink`]), so every heartbeat ends unsent;
/// - each loop and each order runs on a thread of its own ([`ThreadDispatch`]);
/// - both loops log through the daemon's alert sink ([`DaemonLog`]).
pub fn host_links(
    daemon: &Arc<Daemon>,
    files: &HostFiles,
    period: NonZeroU64,
    kind: PlatformKind,
) -> Links {
    let ctx = daemon.ctx();
    let log = Arc::new(DaemonLog::new(Arc::clone(daemon)));
    let stop = StopFlag::default();
    let endpoint = ctx.config.coordinator.as_str().to_owned();
    let settings = EnrollSettings {
        ca_sha256: ctx.config.ca_sha256.clone(),
        image: image_of(ctx.own_version),
    };
    let pki = files.pki.clone();
    Links {
        enroller: Box::new(move || host_enroller(&endpoint, &pki, settings, kind)),
        link: Arc::new(NoLink),
        certs: Arc::new(PkiCertSource::new(&files.pki, log.clone())),
        clock: Arc::new(SystemUnixClock),
        log,
        sleeper: Box::new(ClockSleeper::new(Arc::clone(&ctx.clock), stop.clone())),
        period,
        dispatch: Arc::new(ThreadDispatch),
        stop,
    }
}

/// What the enrollment request says the daemon came from: this binary, `hostkeeper-<version>`.
pub fn image_of(own_version: HostkeeperVersion) -> String {
    format!("hostkeeper-{own_version}")
}

/// The enroller of a host of `kind`: [`GrpcEnroll`] to `endpoint`, the pki directory at `pki`
/// (its `current` generation is what a renewal presents), the claims of [`host_claims`] (on a
/// Mac through the host's `ioreg`, [`HostIoreg::system`]), the operating
/// system's entropy, the wall clock for the certificates' validity alone, and the monotonic
/// clock for the retries. Opening it takes the pki directory's lock and finishes what a crash
/// left there. `Err` names what refused: the endpoint, the runtime or the pki directory.
pub fn host_enroller(
    endpoint: &str,
    pki: &Path,
    settings: EnrollSettings,
    kind: PlatformKind,
) -> Result<Enroller, String> {
    let store = PkiDir::new(pki);
    let coordinator = GrpcEnroll::new(endpoint, store.current_paths(), ENROLL_TIMEOUT)
        .map_err(|error| error.to_string())?;
    let ports = hostkeeper_enroll::Ports {
        coordinator: Box::new(coordinator),
        store: Box::new(store),
        claims: host_claims(kind, Box::new(HostIoreg::system())),
        entropy: Box::new(OsEntropy),
        wall: Box::new(SystemWallClock),
        clock: Box::new(SystemClock),
    };
    Enroller::open(settings, ports).map_err(|error| error.to_string())
}

/// Starts the daemon on the ports of the host's build for `platform`, reading the host's own
/// files from `files`, and returns it without serving: the config of `settings` is loaded, the
/// platform's adapters are chosen ([`Adapters::for_platform`]), the ports are built
/// ([`host_ports`]) and the start runs ([`start::startup_notifying`]).
pub fn start_with(
    settings: &Settings,
    files: &HostFiles,
    own_version: HostkeeperVersion,
    notifier: &dyn ReadyNotifier,
    platform: &Platform,
) -> Result<start::Started, StartError> {
    let config = start::load_config(settings)?;
    Adapters::for_platform(platform)
        .and_then(|adapters| host_ports(&config, settings, files, own_version, adapters))
        .and_then(|ports| start::startup_notifying(settings, ports, own_version, notifier))
}

/// The files outside the daemon's own directories that the host's ports and loops read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFiles {
    /// The CA bundle, PEM: the roots the package fetch trusts.
    pub ca_bundle: PathBuf,
    /// The pinned `gh`.
    pub gh: PathBuf,
    /// The Sigstore trusted root.
    pub trusted_root: PathBuf,
    /// The pki directory, which the enrollment loop writes and the heartbeat loop reads.
    pub pki: PathBuf,
}

impl HostFiles {
    /// Where this host keeps them: [`HostFiles::for_platform`] for the platform this binary was
    /// built for, asking the filesystem which CA bundle exists.
    pub fn host() -> HostFiles {
        HostFiles::for_platform(Platform::host().kind, Path::exists)
    }

    /// Where a host of `kind` keeps them, `exists` saying whether a path is there; the pki
    /// directory is [`PKI_DIR`] on both.
    ///
    /// - A Mac: [`CA_BUNDLE_MACOS`], [`GH_BINARY_MACOS`] and [`SIGSTORE_TRUSTED_ROOT_MACOS`],
    ///   whatever `exists` says.
    /// - Linux: [`GH_BINARY`] and [`SIGSTORE_TRUSTED_ROOT`], and the first of [`CA_BUNDLE`]
    ///   (Debian family) and [`CA_BUNDLE_RHEL`] (RHEL family) that exists; [`CA_BUNDLE`] when
    ///   neither does, so the fetch reads a bundle that is not there and has no root to trust.
    pub fn for_platform(kind: PlatformKind, exists: impl Fn(&Path) -> bool) -> HostFiles {
        let (ca_bundle, gh, trusted_root) = match kind {
            PlatformKind::Macos => (
                CA_BUNDLE_MACOS,
                GH_BINARY_MACOS,
                SIGSTORE_TRUSTED_ROOT_MACOS,
            ),
            PlatformKind::Linux => (
                [CA_BUNDLE, CA_BUNDLE_RHEL]
                    .into_iter()
                    .find(|bundle| exists(Path::new(bundle)))
                    .unwrap_or(CA_BUNDLE),
                GH_BINARY,
                SIGSTORE_TRUSTED_ROOT,
            ),
        };
        HostFiles {
            ca_bundle: PathBuf::from(ca_bundle),
            gh: PathBuf::from(gh),
            trusted_root: PathBuf::from(trusted_root),
            pki: PathBuf::from(PKI_DIR),
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

/// The ports of this build over the platform's `adapters` ([`Adapters`]: the step launcher, the
/// service manager, the unit directory, the observer's service manager and process probe, and
/// the package check). The package store is the real one ([`RealPackages`]: HTTPS over the CA
/// bundle of `files`, `gh` for the attestation, unpacked into `--packages-dir` as root, each
/// staged tree judged by the adapters' package check); the observer looks through the adapters
/// and TCP ([`observer_with`]). On Linux the service manager is not part of this build yet
/// ([`Unwired`]): an order that reached it is refused there. Placed paths are linked under the
/// host's own directories and the adapters' unit directory, `--packages-dir` being the packages
/// directory; the prune of step 18 of a force deletes nothing yet. The directory `gh` runs in
/// (`<state-dir>/.gh`) is made mode 0700 here. `Err` when that directory cannot be made or the
/// packages directory cannot be opened.
pub fn host_ports(
    config: &DaemonConfig,
    settings: &Settings,
    files: &HostFiles,
    own_version: HostkeeperVersion,
    adapters: Adapters,
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
    })?
    .with_package_check(adapters.package_check);
    Ok(Ports {
        launcher: adapters.launcher,
        packages: Arc::new(packages),
        clock,
        deadman_poll_secs: Some(DEADMAN_POLL_SECS),
        units: adapters.units,
        placement: Placement {
            roots: Roots::new(
                Path::new(STATE_DIR),
                &adapters.unit_dir,
                &settings.packages_dir,
            ),
            fs: Arc::new(RealFs::new(Path::new("/"))),
        },
        prune: Arc::new(LayoutPrune),
        force_lock_point: Arc::new(NoLockPoint),
        observer: Arc::new(observer_with(
            adapters.manager,
            adapters.process,
            &settings.packages_dir,
            own_version,
        )),
    })
}

/// The service manager of a build that has none, and a step launcher that starts nothing: each
/// refuses. A Linux host's adapters use it as their service manager ([`Adapters::linux`]); the
/// tests use it as a launcher that refuses every step.
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
