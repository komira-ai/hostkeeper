//! Start-up and serving (design sections 4.1 and 6.5).
//!
//! [`startup`] does, in this order: load the daemon config (a config that fails to load stops the
//! daemon with an error naming the key, and nothing on the host changes: no directory, no lock
//! file, no socket); take the exclusive lock on `<state-dir>/lock`, held for the daemon's life (a
//! second daemon on the same state directory gets [`StartError::Locked`]); [`before_records`] and
//! [`boot_pass`]; bind the admin socket, removing a stale socket only now that the lock is held;
//! [`after_boot`]. [`serve`] then serves the admin socket.
//!
//! [`before_records`], [`boot_pass`] and [`after_boot`] are the boot work of design section 6.5.
//! [`before_records`] is the place of the placed links (finished or undone, and compared with
//! `current`), which a later change fills. [`boot_pass`] reads the step records against the boot
//! id, runs each unfinished update to its verdict and restarts the watchers
//! ([`crate::boot`]). [`after_boot`] runs the manifest's `boot` steps, then sends the readiness
//! notice. The admin socket is bound before [`after_boot`] but served only by [`serve`], after
//! [`startup`] has returned: nothing is answered on it while the boot work runs.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::daemon::{ConfigError, DaemonConfig};
use hostkeeper_config::manifest::{BadHostkeeperVersion, HostkeeperVersion};

use crate::admin;
use crate::boot::{BootError, BootReport, Stage};
use crate::executor::{Daemon, Dirs, Ports};
use crate::files;
use crate::notify::{NoNotifier, ReadyNotifier};
use crate::socket;

/// The lock file in the state directory.
pub const LOCK_FILE: &str = "lock";

/// The exit status of a daemon that could not start or serve for any reason but these below.
pub const EXIT_FAILED: u8 = 1;
/// The exit status of a daemon whose config failed to load (`2` is a usage error).
pub const EXIT_CONFIG: u8 = 3;
/// The exit status of a daemon that found another daemon holding the state directory.
pub const EXIT_LOCKED: u8 = 4;

/// Where the daemon reads and keeps things, from its flags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The daemon config.
    pub config: PathBuf,
    pub state_dir: PathBuf,
    pub packages_dir: PathBuf,
    pub admin_socket: PathBuf,
    /// The account the daemon config must be owned by: root (0) on a host.
    pub trusted_uid: u32,
}

/// Why the daemon could not start or serve.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The daemon config did not load; the error names the key or the file.
    #[error("{}: {source}", path.display())]
    Config {
        path: PathBuf,
        #[source]
        source: ConfigError,
    },
    /// Another daemon holds the state directory, or its lock cannot be taken.
    #[error("another daemon holds {}, or its lock cannot be taken: {source}", path.display())]
    Locked {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("the admin socket could not be served: {0}")]
    Serve(#[from] tonic::transport::Error),
    #[error("the async runtime could not be made: {0}")]
    Runtime(io::Error),
    #[error("this daemon's own version is not major.minor.patch: {0}")]
    OwnVersion(#[from] BadHostkeeperVersion),
    /// The start pass could not list the step records.
    #[error("the start pass could not run: {0}")]
    Boot(#[from] BootError),
    /// `--notify-systemd` was given, and the service manager named no notify socket.
    #[error(
        "--notify-systemd was given, but no notify socket was set for the daemon (NOTIFY_SOCKET)"
    )]
    NoNotifySocket,
}

impl StartError {
    /// The exit status of the daemon for this error.
    pub fn exit_code(&self) -> u8 {
        match self {
            StartError::Config { .. } => EXIT_CONFIG,
            StartError::Locked { .. } => EXIT_LOCKED,
            StartError::Io { .. }
            | StartError::Serve(_)
            | StartError::Runtime(_)
            | StartError::OwnVersion(_)
            | StartError::Boot(_)
            | StartError::NoNotifySocket => EXIT_FAILED,
        }
    }
}

fn io_at(path: &Path) -> impl Fn(io::Error) -> StartError + '_ {
    move |source| StartError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A daemon that started: its lock is held, its admin socket bound.
pub struct Started {
    daemon: Arc<Daemon>,
    listener: UnixListener,
    socket_path: PathBuf,
    report: BootReport,
    /// Held while the daemon lives: one daemon per state directory.
    _lock: File,
}

impl Started {
    pub fn daemon(&self) -> &Arc<Daemon> {
        &self.daemon
    }

    /// What the boot work did.
    pub fn boot_report(&self) -> &BootReport {
        &self.report
    }
}

/// Takes the exclusive lock of the state directory, made mode 0700 when it is missing.
fn lock_state_dir(state_dir: &Path) -> Result<File, StartError> {
    files::make_private(state_dir).map_err(io_at(state_dir))?;
    let path = state_dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(io_at(&path))?;
    file.try_lock().map_err(|e| StartError::Locked {
        path: path.clone(),
        source: io::Error::from(e),
    })?;
    Ok(file)
}

/// Starts the daemon: the config, the lock, the boot work, the admin socket. `own_version` is
/// the daemon's own, which a manifest's `min_hostkeeper` is compared with. Nobody is told it is
/// ready ([`NoNotifier`]).
pub fn startup(
    settings: &Settings,
    ports: Ports,
    own_version: HostkeeperVersion,
) -> Result<Started, StartError> {
    startup_notifying(settings, ports, own_version, &NoNotifier)
}

/// [`startup`], telling `notifier` the daemon is ready once the boot work is over.
pub fn startup_notifying(
    settings: &Settings,
    ports: Ports,
    own_version: HostkeeperVersion,
    notifier: &dyn ReadyNotifier,
) -> Result<Started, StartError> {
    let config = DaemonConfig::load(&settings.config, settings.trusted_uid).map_err(|source| {
        StartError::Config {
            path: settings.config.clone(),
            source,
        }
    })?;
    let lock = lock_state_dir(&settings.state_dir)?;
    let dirs = Dirs {
        state: settings.state_dir.clone(),
        packages: settings.packages_dir.clone(),
    };
    let daemon = Arc::new(Daemon::new(config, dirs, ports, own_version));
    let mut report = BootReport::default();
    before_records(&daemon, &mut report);
    boot_pass(&daemon, &mut report)?;
    let listener = socket::bind(&settings.admin_socket).map_err(io_at(&settings.admin_socket))?;
    after_boot(&daemon, &mut report, notifier);
    Ok(Started {
        daemon,
        listener,
        socket_path: settings.admin_socket.clone(),
        report,
        _lock: lock,
    })
}

/// Before the step records are read: finish or undo a placed link left half done, and relink
/// what differs from `current` (design section 6.5). Nothing yet but its place in the order.
fn before_records(_daemon: &Daemon, report: &mut BootReport) {
    report.stages.push(Stage::BeforeRecords);
}

/// The step records against the boot id, each unfinished update run to its verdict, and the
/// watchers restarted (design section 6.5, [`Daemon::boot_pass`]).
fn boot_pass(daemon: &Daemon, report: &mut BootReport) -> Result<(), StartError> {
    Ok(daemon.boot_pass_into(report, &|_| false)?)
}

/// After the boot pass: the manifest's `boot` step of each service, then the readiness notice
/// (design section 6.5, [`Daemon::after_boot`]).
fn after_boot(daemon: &Daemon, report: &mut BootReport, notifier: &dyn ReadyNotifier) {
    daemon.after_boot(report, notifier);
}

/// Serves the admin socket of a started daemon, holding its lock, until the socket fails.
pub async fn serve(started: Started) -> Result<(), StartError> {
    let path = &started.socket_path;
    let incoming = socket::incoming(started.listener).map_err(io_at(path))?;
    // `started._lock` stays held until this returns.
    admin::serve(Arc::clone(&started.daemon), incoming)
        .await
        .map_err(StartError::from)
}
