//! The command line of the `hostkeeper` binary: `hostkeeper daemon` and its flags, and
//! `hostkeeper maintenance-hook`, the maintenance hook the daemon runs as a step
//! ([`crate::maintenance_hook`]), which exits with the hook contract's codes; and the verbs root
//! runs on the host itself, `hostkeeper activate`, `pin`, `unpin` (with the coordinator out of
//! reach) and `renew` ([`crate::local`], which says their exit statuses).
//! Configuration comes in as flags only, never from the environment (AGENTS.md S2); every
//! default is a constant here.
//!
//! `hostkeeper daemon` started from the packaged binary (`/usr/bin/hostkeeper`,
//! `/usr/local/bin/hostkeeper`) gives the binary directory its first binary when `current` starts
//! none ([`crate::seed::seed`]) and execs `current` with its own arguments ([`crate::packaged`],
//! [`run_from`]); a Mac's daemon writes its config from the managed preferences when the config
//! is not there ([`crate::mac_config::at_start`]), before it reads it ([`start_with`],
//! [`HostFiles::managed_config`]), then its stores and signers files from their managed
//! preferences ([`crate::mac_files`]).
//!
//! The two values read from the environment are platform-set markers of systemd, and only
//! `--notify-systemd` makes the daemon use them ([`SystemdVars`]): the address of the notify
//! socket, and `WATCHDOG_USEC`, which says systemd runs a watchdog for the unit.
//!
//! Exit status of `hostkeeper daemon`: [`EXIT_FAILED`] when the daemon could not start or serve,
//! `2` for a usage error (as clap reports it), [`EXIT_CONFIG`] when the daemon config did not
//! load, [`EXIT_LOCKED`] when another daemon holds the state directory. A verb or a flag this
//! binary does not have is a usage error, `2`, as clap reports it: `ctl` (the coordinator's
//! command line is `hk` now, docs/cli.md), `ci` (CI runs `hk releases set-desired` and
//! `hk releases wait-converged`, docs/deploy/ci-client.md), `admin` and `--admin-socket` (the
//! daemon serves no admin socket; a host's status, abort and clear-record go through the
//! coordinator, docs/admin.md), and the verbs of the update before the hand-over
//! (`self-update-protocol`, `self-update-fallback`, `self-update-seed`, `mac-config`,
//! `update-hook` and its step words).
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
use hostkeeper_enroll::{
    Enroller, GrpcEnroll, LinuxClaims, ManagedFile, OsEntropy, PkiDir, RenewalState,
    Settings as EnrollSettings, SystemWallClock,
};
use hostkeeper_install::version::Owner;
use hostkeeper_launchd::{Platform, PlatformKind};
use hostkeeper_link::{
    ClockSleeper, LoopFlags, MaintenanceSource, PkiCertSource, StopFlag, ThreadDispatch,
};
use hostkeeper_observe::ProcessRunner;
use hostkeeper_package::net::{NetFetcher, UreqTransport};
use hostkeeper_placed::roots::STATE_DIR;
use hostkeeper_placed::{RealFs, Roots, UnitControl, UnitControlError};
use hostkeeper_steps::{Clock, LaunchError, StepCall, StepExit, StepLauncher};

use crate::repository::RepositoryPorts;
use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;

use crate::adapters::{Adapters, RunnerLauncher, host_claims, host_clock, host_managed};
use crate::console::{AGETTY, ConsoleIssue, ENROLLMENT_ISSUE};
use crate::coordinator_store::CoordinatorFetcher;
use crate::executor::{Daemon, Placement, Ports};
use crate::handover::ProcessExec;
use crate::host_identity::{HostIdentity, IDENTITY_DIR, IdentityOwner};
use crate::identity_group::IdentityGroup;
use crate::ipmi::DevIpmi;
use crate::links::{DaemonLog, HostLink, Links};
use crate::mac::HostIoreg;
use crate::maintenance_collect::{Collector, LinuxScope};
use crate::maintenance_facts::MaintenanceFacts;
use crate::maintenance_hook::HookArgs;
use crate::maintenance_softwareupdate::MacosMajor;
use crate::maintenance_state::{RunReported, on_demand_of};
use crate::notify::{
    NoNotifier, NoWatchdog, ReadyFile, ReadyNotifier, SystemdNotifier, SystemdWatchdog, Watchdog,
};
use crate::observer::{OwnService, observer_with};
use crate::operator_keys::OperatorKeys;
use crate::prerequisites::Prerequisites;
use crate::prune::{DEFAULT_KEEP_VERSIONS, LayoutPrune, MAX_KEEP_VERSIONS, NoLockPoint};
use crate::real_packages::RealPackages;
use crate::repositories;
use crate::repository_push::Managed;
use crate::selfupdate::binary_dir;
use crate::start::{self, Settings, StartError};
use crate::unix_clock::SystemUnixClock;
use hostkeeper_install::announce::OsHost;
use hostkeeper_install::host::{Host, Ids};
use hostkeeper_install::macos_pkg::signers::Signers;
use hostkeeper_install::{Announcement, Family, Tools};

pub use crate::start::{EXIT_CONFIG, EXIT_FAILED, EXIT_LOCKED};

/// The default of `--config`.
pub const DEFAULT_CONFIG: &str = "/etc/hostkeeper/hostkeeper.toml";
/// The host's stores, beside the daemon config ([`DEFAULT_CONFIG`]) on both platforms: the file a
/// Mac's start renders from its managed preferences ([`crate::mac_files::stores`]).
pub const STORES_FILE: &str = "/etc/hostkeeper/stores.toml";
pub use hostkeeper_install::macos_pkg::signers::SIGNERS_FILE;
/// The default of `--state-dir`.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/updater/state";
/// The default of `--packages-dir`.
pub const DEFAULT_PACKAGES_DIR: &str = "/var/lib/hostkeeper/packages";
/// The default of `--facts-file`, on Linux and on a Mac.
pub const DEFAULT_FACTS_FILE: &str = "/var/lib/hostkeeper/facts.json";
/// The default of `--package-lock-wait-secs`: five minutes, the length of a usual unattended
/// upgrade.
pub const DEFAULT_PACKAGE_LOCK_WAIT_SECS: u64 = 300;
/// The largest `--package-lock-wait-secs`: an hour, the longest wait a step may ask for
/// (`hostkeeper_steps::runner::MAX_RETRY_SECS`).
pub const MAX_PACKAGE_LOCK_WAIT_SECS: u64 = 3600;
/// The variable systemd sets for a `Type=notify` unit: the address of its notify socket.
pub const NOTIFY_SOCKET: &str = "NOTIFY_SOCKET";
/// The variable systemd sets for a unit with `WatchdogSec=`: the watchdog's timeout in
/// microseconds. The daemon reads only whether it is set.
pub const WATCHDOG_USEC: &str = "WATCHDOG_USEC";
/// The CA bundle of a Mac, the system roots macOS keeps as PEM: the roots the fetch of a
/// `macos-pkg` entry's `.pkg` trusts.
pub const CA_BUNDLE_MACOS: &str = "/etc/ssl/cert.pem";
/// The pinned `gh` that checks an attestation, on a Linux host: a file hostkeeper's own package
/// installs for `hostkeeper-pxe`; the daemon runs none.
pub const GH_BINARY: &str = "/usr/lib/hostkeeper/gh";
/// The Sigstore trusted root, as `gh attestation trusted-root` prints it, on a Linux host: a file
/// hostkeeper's own package installs for `hostkeeper-pxe`; the daemon reads none.
pub const SIGSTORE_TRUSTED_ROOT: &str = "/usr/lib/hostkeeper/trusted_root.json";
/// How often each service's dead-man looks at its running step on a host, in seconds.
pub const DEADMAN_POLL_SECS: u64 = 1;
/// The pki directory: the host's key and its certificate generations, `current` the one in use
/// (design section 10).
pub const PKI_DIR: &str = "/etc/updater/pki";
/// The owner, and the group a copy for root alone has, of the host identity copy: uid and gid 0
/// on every platform the daemon runs on (a Mac's `wheel` is gid 0).
const ROOT: IdentityOwner = IdentityOwner { uid: 0, gid: 0 };
/// How long the enrollment loop's connection, handshake and each call to the coordinator may take.
pub const ENROLL_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `agetty --reload` may run: it only touches agetty's reload file and exits.
pub const AGETTY_DEADLINE: Duration = Duration::from_secs(10);
/// How much of `agetty --reload`'s standard output is read: its exit alone is judged.
pub const AGETTY_OUTPUT_CAP: usize = 4 * 1024;

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
    /// Run a step of the maintenance hook: the OS work of a maintenance run, which the daemon
    /// runs as it runs a hook's step.
    MaintenanceHook(HookArgs),
    /// `activate`, `pin`, `unpin` and `renew`: what root runs on the host itself
    /// ([`crate::local`]).
    #[command(flatten)]
    Local(crate::local::LocalVerb),
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
    /// The host's facts file, which the daemon writes from the facts the coordinator sends: the
    /// host id, host group, platform and labels, for the services that read them.
    #[arg(long, default_value = DEFAULT_FACTS_FILE)]
    pub facts_file: PathBuf,
    /// Tell systemd the daemon is ready (`READY=1` on the notify socket systemd sets for a unit
    /// of `Type=notify`) once its boot work is over.
    #[arg(long, conflicts_with = "ready_file")]
    pub notify_systemd: bool,
    /// Write this file, with the daemon's pid, once the boot work is over ([`ReadyFile`]): the
    /// readiness of a daemon under OpenRC, whose init script removes it before a start and waits
    /// for it after.
    #[arg(long)]
    pub ready_file: Option<PathBuf>,
    /// The seconds a step of the repository installer waits at most for the host's package
    /// manager's lock, from 1 to 3600: apt's own wait (`DPkg::Lock::Timeout`) for an install,
    /// and a try every 2 seconds for `dpkg --purge` and `apt-mark`. A lock still held then defers
    /// the order (`package_manager_busy`), which halts nothing. It is also the maintenance look's
    /// bound: each run of the look's package manager that finds the lock held is tried again
    /// every 2 seconds for at most this long, and a lock still held then fails the look.
    #[arg(
        long,
        default_value_t = DEFAULT_PACKAGE_LOCK_WAIT_SECS,
        value_parser = clap::value_parser!(u64).range(1..=MAX_PACKAGE_LOCK_WAIT_SECS),
    )]
    pub package_lock_wait_secs: u64,
    /// How many versions of each service the host keeps besides `current`, `previous`, the one
    /// its pin names (`pins/<service>` beside the state directory) and the ones an unfinished
    /// update or force still uses, from 0 to 32: the newest, by the time each was staged. A prune
    /// after a confirmed switch deletes the rest, and none while an update is on trial.
    #[arg(
        long,
        default_value_t = DEFAULT_KEEP_VERSIONS,
        value_parser = clap::value_parser!(u64).range(0..=MAX_KEEP_VERSIONS),
    )]
    pub keep_versions: u64,
    /// Which Linux updates the host reports due (`maintenance-v1`): `security`, the default,
    /// security updates and newer kernels; `all`, every update. It applies to a Linux host alone.
    #[arg(long, value_enum, default_value_t = LinuxScope::Security)]
    pub maintenance_linux_scope: LinuxScope,
    /// Whether a Mac reports the newest major upgrade of macOS due (`maintenance-v1`); without
    /// the flag, a major upgrade is never due, though `softwareupdate` lists it. A Linux host
    /// ignores it.
    #[arg(long)]
    pub maintenance_macos_major: bool,
    /// The heartbeat loop's flag: `--heartbeat-secs`.
    #[command(flatten)]
    pub link: LoopFlags,
}

impl DaemonFlags {
    /// The settings of these flags; the daemon config is trusted when root owns it, and the run
    /// directory is the host platform's.
    pub fn settings(&self) -> Settings {
        Settings {
            config: self.config.clone(),
            state_dir: self.state_dir.clone(),
            packages_dir: self.packages_dir.clone(),
            facts_file: self.facts_file.clone(),
            run_dir: Platform::host().run_dir,
            trusted_uid: 0,
            package_lock_wait_secs: self.package_lock_wait_secs,
            keep_versions: self.keep_versions,
        }
    }
}

/// What systemd set in the environment of the process for its unit: platform-set markers, not
/// configuration (AGENTS.md S2). The daemon uses them with `--notify-systemd` only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemdVars {
    /// `NOTIFY_SOCKET`: the address of the notify socket of a `Type=notify` unit.
    pub notify_socket: Option<OsString>,
    /// `WATCHDOG_USEC`: set when the unit has `WatchdogSec=`.
    pub watchdog_usec: Option<OsString>,
}

/// The entry point of the `hostkeeper` binary: [`run_from`] with the process's own arguments
/// and image, the paths a package installs the binary at, what systemd set and the process's
/// own exec.
pub fn main() -> ExitCode {
    let exe = std::env::current_exe().unwrap_or_default();
    run_from(
        std::env::args_os().collect(),
        &exe,
        &crate::packaged::host_binaries(),
        systemd_vars(),
        &crate::packaged::exec,
    )
}

/// Runs the command line `args` (the program's name first) of the binary whose image is `exe`:
/// `hostkeeper daemon` from one of the `packaged` paths seeds and execs `current` with the
/// arguments after the program's name ([`crate::packaged::start`], through `exec`), its error
/// said on standard error as the daemon's and its status the exit status; anything else is
/// [`run_with`] with `systemd`.
pub fn run_from(
    args: Vec<OsString>,
    exe: &Path,
    packaged: &[&Path],
    systemd: SystemdVars,
    exec: crate::packaged::Exec<'_>,
) -> ExitCode {
    let cli = Cli::parse_from(&args);
    if let Command::Daemon(flags) = &cli.command
        && let Some(error) = crate::packaged::start(
            exe,
            packaged,
            &flags.state_dir,
            env!("CARGO_PKG_VERSION"),
            args.get(1..).unwrap_or_default(),
            exec,
        )
    {
        eprintln!("hostkeeper daemon: {error}");
        return ExitCode::from(error.exit_code());
    }
    run_with(cli, systemd)
}

/// What systemd set for the process, if anything: the notify socket's address and the
/// watchdog's timeout. It is used only with `--notify-systemd`.
#[expect(
    clippy::disallowed_methods,
    reason = "NOTIFY_SOCKET and WATCHDOG_USEC are the platform-set markers of a Type=notify unit and of its watchdog, which AGENTS.md S2 allows; the flag --notify-systemd decides whether they are used"
)]
fn systemd_vars() -> SystemdVars {
    SystemdVars {
        notify_socket: std::env::var_os(NOTIFY_SOCKET),
        watchdog_usec: std::env::var_os(WATCHDOG_USEC),
    }
}

/// Runs a parsed command line to its exit status, with nothing set by systemd.
pub fn run(cli: Cli) -> ExitCode {
    run_with(cli, SystemdVars::default())
}

/// Runs a parsed command line to its exit status; `systemd` is what systemd set for the process,
/// used with `--notify-systemd` only. `maintenance-hook` runs its step on this host
/// ([`crate::maintenance_hook::host_main`]) and exits with the step's outcome.
pub fn run_with(cli: Cli, systemd: SystemdVars) -> ExitCode {
    let flags = match cli.command {
        Command::Daemon(flags) => flags,
        Command::MaintenanceHook(args) => {
            return ExitCode::from(crate::maintenance_hook::host_main(&args));
        }
        Command::Local(verb) => return ExitCode::from(crate::local::main(verb)),
    };
    let period = flags.link.heartbeat_secs;
    let watchdog = watchdog(&flags, &systemd);
    let served = notifier(&flags, systemd.notify_socket).and_then(|notifier| {
        daemon(
            &flags.settings(),
            period,
            flags.maintenance_linux_scope,
            MacosMajor::from_flag(flags.maintenance_macos_major),
            notifier.as_ref(),
            watchdog,
        )
    });
    ExitCode::from(exit_status(&served))
}

/// The exit status of `hostkeeper daemon` once it has served: 0 when it returned to be started
/// again, as after its own update asked it to exit ([`start::serve`]), which the unit's
/// `Restart=always` restarts; the error's status ([`StartError::exit_code`]), said on standard
/// error, otherwise.
pub fn exit_status(served: &Result<(), StartError>) -> u8 {
    match served {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("hostkeeper daemon: {error}");
            error.exit_code()
        }
    }
}

/// The readiness notifier the flags ask for: systemd's with `--notify-systemd`, refused when no
/// notify socket was set; the file of `--ready-file`; none without either.
pub fn notifier(
    flags: &DaemonFlags,
    notify_socket: Option<OsString>,
) -> Result<Box<dyn ReadyNotifier>, StartError> {
    if let Some(path) = &flags.ready_file {
        return Ok(Box::new(ReadyFile::new(path)));
    }
    match (flags.notify_systemd, notify_socket) {
        (false, _) => Ok(Box::new(NoNotifier)),
        (true, Some(socket)) => Ok(Box::new(SystemdNotifier::new(socket))),
        (true, None) => Err(StartError::NoNotifySocket),
    }
}

/// The watchdog the flags and systemd ask for: systemd's with `--notify-systemd` and a notify
/// socket, which sends its notice only when `WATCHDOG_USEC` is set; none otherwise (without a
/// notify socket, [`notifier`] refuses the start).
pub fn watchdog(flags: &DaemonFlags, systemd: &SystemdVars) -> Box<dyn Watchdog> {
    match (flags.notify_systemd, &systemd.notify_socket) {
        (true, Some(socket)) => Box::new(SystemdWatchdog::new(
            socket.clone(),
            systemd.watchdog_usec.as_deref(),
        )),
        _ => Box::new(NoWatchdog),
    }
}

/// Starts the daemon and serves it, beating every `period` seconds and telling `watchdog` after
/// each heartbeat, returning only on an error or for its own update. Before anything else, a
/// hand-over of its own update into another binary is acted on ([`crate::handover`]): that
/// binary is exec'd with this process's arguments, and this one never returns here.
fn daemon(
    settings: &Settings,
    period: NonZeroU64,
    scope: LinuxScope,
    major: MacosMajor,
    notifier: &dyn ReadyNotifier,
    watchdog: Box<dyn Watchdog>,
) -> Result<(), StartError> {
    let own_version = HostkeeperVersion::parse(env!("CARGO_PKG_VERSION"))?;
    let exec = ProcessExec::new(std::env::args_os().skip(1).collect());
    crate::handover::at_daemon_start(&settings.state_dir, &own_version.to_string(), &exec);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build();
    let runtime = runtime.map_err(StartError::Runtime)?;
    runtime.block_on(start_and_serve(
        settings,
        period,
        scope,
        major,
        own_version,
        notifier,
        watchdog,
    ))
}

async fn start_and_serve(
    settings: &Settings,
    period: NonZeroU64,
    scope: LinuxScope,
    major: MacosMajor,
    own_version: HostkeeperVersion,
    notifier: &dyn ReadyNotifier,
    watchdog: Box<dyn Watchdog>,
) -> Result<(), StartError> {
    let files = HostFiles::host();
    let platform = Platform::host();
    let started = start_with(settings, &files, own_version, notifier, &platform)?;
    let links = host_links(
        started.daemon(),
        &files,
        period,
        platform.kind,
        scope,
        major,
        watchdog,
    );
    let terminated = crate::terminate::sigterm(links.log.as_ref());
    start::serve_until(started, links, terminated).await;
    Ok(())
}

/// The enrollment and heartbeat loops of a host ([`Links`]), over the pki directory of `files`:
///
/// - the enrollment loop's enroller is made on its own worker ([`host_enroller`]): gRPC with TLS
///   to the daemon config's `coordinator`, pinned to its `ca_sha256`, the key and the
///   certificate generations in the pki directory, the claims of a host of `kind`
///   ([`host_claims`]); one that cannot be made is made again after a wait on the daemon's
///   clock, and every heartbeat reports what the loop last said of the certificate;
/// - the heartbeat loop reads the certificate and key of the pki directory's `current` link at
///   every heartbeat ([`PkiCertSource`]), and beats every `period` seconds on the daemon's clock, and once
///   more as soon as a running order ends;
///   its link ([`HostLink`]) is the JSON route and gRPC over mutual TLS to the same
///   `coordinator`, pinned to the same `ca_sha256`, trusting the CA bundle of the pki directory's
///   `current` generation, its connections bounded on the daemon's clock; the link is made at the
///   first heartbeat, on the loop's worker, so a host not enrolled yet makes none;
/// - each loop and each order runs on a thread of its own ([`ThreadDispatch`]);
/// - both loops log through the daemon's alert sink ([`DaemonLog`]);
/// - the heartbeat loop tells `watchdog` after each heartbeat that it is alive;
/// - on a Mac (`kind`), the Remote Login probe starts on a thread of its own, and every heartbeat
///   after it is done reports it ([`crate::remote_login_probe::at_host_start`]);
/// - every heartbeat reports the Full Disk Access grant ([`crate::full_disk_access`]);
/// - every host keeps the certificate and key of the pki directory's `current` generation at
///   [`IDENTITY_DIR`], after each step of the enrollment loop, for the group `hostkeeper-identity`
///   ([`HostIdentity`], [`IdentityGroup::for_host`]);
/// - every host keeps the operator keys the coordinator sends in the managed block of its login
///   account's `authorized_keys` ([`OperatorKeys::at_host`]);
/// - every host keeps the host prerequisites the coordinator sends, which a Mac installs and a
///   Linux host only reports ([`Prerequisites::at_host`]);
/// - every host reports what it last found of its maintenance ([`maintenance_source`] over
///   [`crate::maintenance_collectors::at_host`]): a Linux host under `scope`, from the packages
///   `placed.json` records and its package manager, which waits for a held lock for the daemon's
///   `--package-lock-wait-secs`; a Mac from `sw_vers`, `softwareupdate`,
///   `fdesetup`, its data volume and its Command Line Tools, a major upgrade due under `major`,
///   and its Xcodes, its update account and its device-management conflicts;
/// - every heartbeat of a Linux host reports its BMC's address and MAC, read through
///   `/dev/ipmi0` ([`crate::bmc::at_host`]);
/// - when `files` names a console, the enrollment loop writes its issue file, naming the daemon
///   config's `coordinator`, and has the gettys redraw by running its agetty with `--reload` as
///   a child process, its output read to [`AGETTY_OUTPUT_CAP`] and bounded by
///   [`AGETTY_DEADLINE`] ([`crate::console`]).
pub fn host_links(
    daemon: &Arc<Daemon>,
    files: &HostFiles,
    period: NonZeroU64,
    kind: PlatformKind,
    scope: LinuxScope,
    major: MacosMajor,
    watchdog: Box<dyn Watchdog>,
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
    let console = files.console.as_ref().map(|console| {
        ConsoleIssue::new(
            console.issue.clone(),
            endpoint.clone(),
            &console.agetty,
            Box::new(RunnerLauncher::new(
                ProcessRunner::new(AGETTY_OUTPUT_CAP),
                AGETTY_DEADLINE,
            )),
        )
    });
    let link = HostLink::new(
        ctx.config.clone(),
        files.pki.clone(),
        ctx.dirs.state.clone(),
        crate::coordinator_address::host_lookup(),
        Arc::clone(&ctx.clock),
        log.clone(),
    );
    let certs = Arc::new(PkiCertSource::new(&files.pki, log.clone()));
    let identity = HostIdentity::new(
        certs.clone(),
        PathBuf::from(IDENTITY_DIR),
        IdentityGroup::for_host(kind),
        ROOT,
    );
    let operator_keys = OperatorKeys::at_host(kind, log.clone());
    let prerequisites = Prerequisites::at_host(kind, log.clone());
    let maintenance = maintenance_source(
        daemon,
        kind,
        scope,
        crate::maintenance_collectors::at_host(kind, major, ctx.repository_ports.lock_wait_secs),
        Arc::new(SystemUnixClock),
        Arc::new(ThreadDispatch),
        log.clone(),
    );
    let health =
        crate::host_health::at_host(kind, &ctx.dirs.state, ctx.clock.as_ref(), log.clone());
    let clock: Arc<dyn hostkeeper_link::UnixClock> = Arc::new(SystemUnixClock);
    Links {
        enroller: Box::new(move || host_enroller(&endpoint, &pki, settings.clone(), kind)),
        link: Arc::new(link),
        certs,
        bmc: crate::bmc::at_host(kind, Arc::clone(&clock)),
        serial_console: crate::serial_baseline::at_host(
            kind,
            ctx.config.serial_console.as_ref(),
            Arc::clone(&clock),
            log.clone(),
        ),
        full_disk_access: Some(crate::full_disk_access::at_host(
            kind,
            Arc::clone(&clock),
            log.clone(),
        )),
        clock,
        log,
        sleeper: Box::new(ClockSleeper::new(Arc::clone(&ctx.clock), stop.clone())),
        enroll_sleeper: Box::new(ClockSleeper::new(Arc::clone(&ctx.clock), stop.clone())),
        renewal: RenewalState::default(),
        period,
        dispatch: Arc::new(ThreadDispatch),
        stop,
        watchdog,
        console,
        identity: Some(identity),
        remote_login_probe: crate::remote_login_probe::at_host_start(kind),
        operator_keys: Some(Arc::new(operator_keys)),
        prerequisites: Some(Arc::new(prerequisites)),
        maintenance,
        health: Some(health),
    }
}

/// The maintenance facts `daemon`'s heartbeats report (`maintenance-v1`): looks by `collectors`
/// under `scope` (`--maintenance-linux-scope`), leaving out the packages of the daemon's own
/// `placed.json` ([`crate::Placement`] of its context), timed by `clock` and run on `dispatch`;
/// none when there is no collector, which then announces and sends nothing. The report carries
/// the run state of the host's maintenance turn, the host's last probe and, on a Mac (`kind`),
/// `probe` as a kind run only when named ([`RunReported`], [`on_demand_of`]).
pub fn maintenance_source(
    daemon: &Arc<Daemon>,
    kind: PlatformKind,
    scope: LinuxScope,
    collectors: Vec<Box<dyn Collector>>,
    clock: Arc<dyn hostkeeper_link::UnixClock>,
    dispatch: Arc<dyn hostkeeper_link::Dispatch>,
    log: Arc<dyn hostkeeper_link::Log>,
) -> Option<Arc<dyn MaintenanceSource>> {
    MaintenanceFacts::of_host(
        collectors,
        &daemon.ctx().placement,
        scope,
        clock,
        dispatch,
        log,
    )
    .map(|facts| {
        Arc::new(RunReported::new(
            facts,
            &daemon.ctx().dirs.state,
            on_demand_of(kind),
        )) as Arc<dyn MaintenanceSource>
    })
}

/// What the enrollment request says the daemon came from: this binary, `hostkeeper-<version>`.
pub fn image_of(own_version: HostkeeperVersion) -> String {
    format!("hostkeeper-{own_version}")
}

/// The enroller of a host of `kind`: [`GrpcEnroll`] to `endpoint`, the pki directory at `pki`
/// (its `current` generation is what a renewal presents), the claims of [`host_claims`] (on
/// Linux the host's files and its BMC behind `/dev/ipmi0`, [`DevIpmi::host`]; on a Mac through
/// the host's `ioreg`, [`HostIoreg::system`]), on a Mac the managed preferences its device management writes
/// the enrollment nonce to ([`host_managed`]), the operating
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
        claims: host_claims(
            kind,
            LinuxClaims::host(),
            Box::new(HostIoreg::system()),
            Arc::new(DevIpmi::host()),
        ),
        entropy: Box::new(OsEntropy),
        wall: Box::new(SystemWallClock),
        clock: Box::new(host_clock()),
        managed: host_managed(kind, ManagedFile::host()),
    };
    Enroller::open(settings, ports).map_err(|error| error.to_string())
}

/// Starts the daemon on the ports of the host's build for `platform`, reading the host's own
/// files from `files`, and returns it without serving: with the managed preferences of `files`
/// (a Mac's), the config of `settings` is written from them when it is not there
/// ([`crate::mac_config::at_start`]), and the stores file and the signers file are rendered from
/// their domains when those are there ([`crate::mac_files`]); the config is loaded, the platform's adapters are chosen
/// ([`Adapters::for_platform`]), the ports are built ([`host_ports`]) and the start runs
/// ([`start::startup_notifying`]). A Mac's start also keeps the Mac awake once the config is loaded
/// ([`crate::keep_awake::at_host_start`]) and turns off macOS's own automatic install of macOS
/// updates ([`crate::auto_install::at_host_start`]), then makes the update account when it is missing, hides
/// it and gives it a Secure Token (`update_account_make`).
pub fn start_with(
    settings: &Settings,
    files: &HostFiles,
    own_version: HostkeeperVersion,
    notifier: &dyn ReadyNotifier,
    platform: &Platform,
) -> Result<start::Started, StartError> {
    if let Some(managed) = &files.managed_config {
        crate::mac_config::at_start(managed, &settings.config, settings.trusted_uid)?;
    }
    if let Some(managed) = &files.managed_stores {
        crate::mac_files::stores(managed, &files.stores, settings.trusted_uid)?;
    }
    if let Some((managed, target)) = files
        .managed_macos_pkg
        .as_deref()
        .zip(files.macos_pkg_signers.as_deref())
    {
        crate::mac_files::macos_pkg(managed, target, settings.trusted_uid)?;
    }
    let config = start::load_config(settings)?;
    crate::keep_awake::at_host_start(&config, platform.kind);
    crate::auto_install::at_host_start(platform.kind);
    crate::update_account_make::at_host_start(platform.kind);
    crate::layout::ensure_state_dirs(settings)?;
    Adapters::for_platform(platform)
        .and_then(|adapters| host_ports(&config, settings, files, own_version, adapters))
        .and_then(|ports| start::startup_notifying(settings, ports, own_version, notifier))
}

/// The files outside the daemon's own directories that the host's ports and loops read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostFiles {
    /// The stores file ([`hostkeeper_package::stores`]), which a Mac's start renders from
    /// `managed_stores` ([`crate::mac_files::stores`]); a start refuses a file that is not sound,
    /// and has no store when it is not there.
    pub stores: PathBuf,
    /// The signers file of the macos-pkg installer
    /// ([`hostkeeper_install::macos_pkg::signers`]); a start refuses a file that is not sound and
    /// starts without one. None on Linux, which never reads it.
    pub macos_pkg_signers: Option<PathBuf>,
    /// The pki directory, which the enrollment loop writes and the heartbeat loop reads.
    pub pki: PathBuf,
    /// The console's issue file and the agetty that redraws it; none on a Mac.
    pub console: Option<ConsoleFiles>,
    /// The managed preferences a Mac's device management installs, which the start writes the daemon config
    /// from when that is not there ([`crate::mac_config::at_start`]); none on Linux.
    pub managed_config: Option<PathBuf>,
    /// The managed preferences of the domain `dev.hostkeeper.stores`, which the start renders as
    /// `stores`, replacing a file that differs ([`crate::mac_files::stores`]); none on Linux.
    pub managed_stores: Option<PathBuf>,
    /// The managed preferences of the domain `dev.hostkeeper.macos-pkg`, which the start renders
    /// as `macos_pkg_signers`, replacing a file that differs ([`crate::mac_files::macos_pkg`]);
    /// none on Linux.
    pub managed_macos_pkg: Option<PathBuf>,
    /// The directory of repository entries ([`hostkeeper_install::repository::config`]), which a
    /// start refuses when it is not sound, and which the repository sets the coordinator pushes
    /// replace ([`crate::repository_push`]); none on a Mac, which has no repository entries,
    /// never reads the directory and takes no set.
    pub repositories: Option<PathBuf>,
    /// The directory the host's `os-release` is read under, which the installers the daemon
    /// announces are decided by ([`hostkeeper_install::announce`]): `/`.
    pub os_root: PathBuf,
}

/// What the enrollment loop shows on the machine's console through ([`crate::console`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleFiles {
    /// The issue file the enrollment loop writes.
    pub issue: PathBuf,
    /// The agetty run with `--reload` after each write.
    pub agetty: PathBuf,
}

impl HostFiles {
    /// Where this host keeps them: [`HostFiles::for_platform`] for the platform this binary was
    /// built for.
    pub fn host() -> HostFiles {
        HostFiles::for_platform(Platform::host().kind)
    }

    /// Where a host of `kind` keeps them; the pki directory is [`PKI_DIR`] on both.
    ///
    /// - A Mac: no console, the managed preferences of
    ///   `hostkeeper_launchd::postinstall::MANAGED_CONFIG`, `MANAGED_STORES` and
    ///   `MANAGED_MACOS_PKG`, and the signers file [`SIGNERS_FILE`] beside `stores.toml`.
    /// - Linux: the repository entries' directory. The console's issue file is
    ///   [`ENROLLMENT_ISSUE`], redrawn by [`AGETTY`], under systemd; an OpenRC host (Alpine) has
    ///   the same files and no console: its getty is busybox's, which reads no issue directory
    ///   and has no `--reload`.
    pub fn for_platform(kind: PlatformKind) -> HostFiles {
        let console = (kind == PlatformKind::Linux).then(|| ConsoleFiles {
            issue: PathBuf::from(ENROLLMENT_ISSUE),
            agetty: PathBuf::from(AGETTY),
        });
        HostFiles {
            stores: PathBuf::from(STORES_FILE),
            macos_pkg_signers: (kind == PlatformKind::Macos).then(|| PathBuf::from(SIGNERS_FILE)),
            pki: PathBuf::from(PKI_DIR),
            console,
            managed_config: (kind == PlatformKind::Macos)
                .then(|| PathBuf::from(hostkeeper_launchd::postinstall::MANAGED_CONFIG)),
            managed_stores: (kind == PlatformKind::Macos)
                .then(|| PathBuf::from(hostkeeper_launchd::postinstall::MANAGED_STORES)),
            managed_macos_pkg: (kind == PlatformKind::Macos)
                .then(|| PathBuf::from(hostkeeper_launchd::postinstall::MANAGED_MACOS_PKG)),
            repositories: (kind != PlatformKind::Macos)
                .then(|| PathBuf::from(hostkeeper_install::repository::config::REPOSITORIES_DIR)),
            os_root: PathBuf::from("/"),
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

/// The signers file of `files`, which `owner_uid` must own: `Ok(None)` on a host with no such
/// file configured (Linux) or none there; `Err` ([`StartError::MacosPkg`]) on every refusal of
/// [`hostkeeper_install::macos_pkg::signers::load`].
pub fn load_signers(files: &HostFiles, owner_uid: u32) -> Result<Option<Signers>, StartError> {
    let Some(path) = &files.macos_pkg_signers else {
        return Ok(None);
    };
    hostkeeper_install::macos_pkg::signers::load(path, owner_uid).map_err(|source| {
        StartError::MacosPkg {
            path: path.clone(),
            source,
        }
    })
}

/// The ports of this build over the platform's `adapters` ([`Adapters`]: the step launcher, the
/// service manager, the unit directory and the observer's service manager and process probe).
/// The package store is the real one ([`RealPackages`]: HTTPS over the CA bundle of `files`, the
/// stores file of `files`, the bounds of `config`; it fetches the `.pkg` of a `macos-pkg` entry
/// into `--packages-dir`); the observer looks through the adapters and TCP ([`observer_with`]);
/// the unit control is the adapters' (`systemctl` on Linux, `launchctl` on macOS). The daemon's
/// links and `placed.json` are under the host's own directories and the adapters' unit
/// directory, `--packages-dir` being the packages directory; the prune of step 18 of a force
/// deletes nothing yet. The installers the daemon announces are decided here, once, from the
/// host's `os-release` under the `os_root` of `files` ([`hostkeeper_install::announce`]), its
/// repository entries ([`crate::repositories::announced`]) and, on a Mac, its signers file and
/// tools ([`crate::macos_pkg::ports`]: `macos-pkg`, whose installer's ports the daemon then
/// holds). `Err` when the stores file of `files` is not sound ([`StartError::Stores`]; one that
/// is not there gives no store) or the signers file is not ([`StartError::MacosPkg`]).
pub fn host_ports(
    config: &DaemonConfig,
    settings: &Settings,
    files: &HostFiles,
    own_version: HostkeeperVersion,
    adapters: Adapters,
) -> Result<Ports, StartError> {
    let announcement = announced(&files.os_root);
    let stores = hostkeeper_package::stores::load(&files.stores, 0).map_err(|source| {
        StartError::Stores {
            path: files.stores.clone(),
            source,
        }
    })?;
    let signers = load_signers(files, 0)?;
    let macos_pkg = crate::macos_pkg::ports(
        adapters.kind == PlatformKind::Macos,
        Host::new(&files.os_root, Ids::MAC),
        signers,
        host_tools(),
    );
    let repositories = repositories::load(files, repositories::ROOT_UID)?;
    let announcement = repositories::announced(announcement, &files.os_root, &repositories);
    let announcement = crate::macos_pkg::announced(announcement, macos_pkg.as_ref());
    say_announced(&announcement);
    let mut repository = RepositoryPorts::new(
        &settings.packages_dir,
        Path::new("/"),
        repositories,
        Owner::ROOT,
    )
    .with_macos_pkg(macos_pkg)
    .with_lock_wait_secs(settings.package_lock_wait_secs);
    if let Some(dir) = &files.repositories {
        repository = repository.with_managed(Managed {
            dir: dir.clone(),
            owner_uid: repositories::ROOT_UID,
            native: repositories::capable(&announcement, &files.os_root),
        });
    }
    let clock: Arc<dyn Clock> = Arc::new(host_clock());
    let coordinator =
        crate::mirror::over_coordinator(config, &files.pki, &settings.state_dir, clock.clone());
    if files.repositories.is_some()
        && let Some(fetch) = &coordinator
    {
        repository = repository.with_mirror(Arc::clone(fetch));
    }
    // Only a Mac fetches a `.pkg`: no other host reads a CA bundle.
    let roots = match adapters.kind {
        PlatformKind::Macos => tls_roots(Path::new(CA_BUNDLE_MACOS)),
        PlatformKind::Linux | PlatformKind::Openrc => Vec::new(),
    };
    let fetcher = NetFetcher::new(UreqTransport::new(&roots), Arc::clone(&clock));
    let packages = RealPackages::new(
        Box::new(fetcher),
        Arc::clone(&clock),
        config.bounds.clone(),
        stores,
        &settings.packages_dir,
    );
    let packages = match coordinator {
        Some(fetch) => packages.with_coordinator(Box::new(CoordinatorFetcher::new(
            fetch,
            Arc::clone(&clock),
            config.bounds.max_package_bytes,
        ))),
        None => packages,
    };
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
            &OwnService {
                unit: adapters.own_unit,
                binary_dir: binary_dir(&settings.state_dir),
            },
        )),
        announcement,
        repository: Arc::new(repository),
        own_job: adapters.own_job.map(Arc::new),
    })
}

/// The installers every host announces, and its family, decided from the host's `os-release`
/// under `os_root` ([`hostkeeper_install::announce`]).
fn announced(os_root: &Path) -> Announcement {
    hostkeeper_install::announce(OsHost {
        macos: cfg!(target_os = "macos"),
        root: os_root,
    })
}

/// Says once on standard error the installers the daemon announces, the native ones added
/// ([`crate::repositories::announced`], [`crate::macos_pkg::announced`]), and the family.
fn say_announced(announcement: &Announcement) {
    eprintln!(
        "hostkeeper daemon: announces the installers {}; the os-release family is {}",
        announcement.names().join(", "),
        announcement.family.map_or("none", Family::as_str)
    );
}

/// The `macos-pkg` installer's tools on a Mac: the host's own
/// ([`hostkeeper_install::HostTools`]).
#[cfg(target_os = "macos")]
fn host_tools() -> Option<Arc<dyn Tools>> {
    Some(Arc::new(hostkeeper_install::HostTools))
}

/// No `macos-pkg` installer's tools: a build for another platform runs none.
#[cfg(not(target_os = "macos"))]
fn host_tools() -> Option<Arc<dyn Tools>> {
    None
}

/// A service manager and a step launcher that run nothing: each refuses. No platform's adapters
/// use it; the tests use it as a launcher that refuses every step.
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
