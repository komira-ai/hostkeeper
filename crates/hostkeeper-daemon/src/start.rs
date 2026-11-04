//! Start-up and serving (design sections 4.1 and 6.5).
//!
//! [`startup`] does, in this order: load the daemon config (a config that fails to load stops the
//! daemon with an error naming the key, and nothing on the host changes: no directory, no lock
//! file, no socket); take the exclusive lock on `<state-dir>/lock`, held for the daemon's life (a
//! second daemon on the same state directory gets [`StartError::Locked`]); [`before_records`] and
//! [`boot_pass`]; bind the admin socket, removing a stale socket only now that the lock is held;
//! [`after_boot`]. [`serve`] then serves the admin socket.
//!
//! [`before_records`], [`boot_pass`] and [`after_boot`] are the places of the boot work of
//! design section 6.5 (the placed links, the step records against the boot id, the manifest's
//! `boot` step and the readiness notice); they do nothing yet.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hostkeeper_config::daemon::{ConfigError, DaemonConfig};
use hostkeeper_config::manifest::{BadHostkeeperVersion, HostkeeperVersion};

use crate::admin;
use crate::executor::{Daemon, Dirs, Ports};
use crate::files;
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
            | StartError::OwnVersion(_) => EXIT_FAILED,
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
    /// Held while the daemon lives: one daemon per state directory.
    _lock: File,
}

impl Started {
    pub fn daemon(&self) -> &Arc<Daemon> {
        &self.daemon
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
/// the daemon's own, which a manifest's `min_hostkeeper` is compared with.
pub fn startup(
    settings: &Settings,
    ports: Ports,
    own_version: HostkeeperVersion,
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
    before_records(&daemon);
    boot_pass(&daemon);
    let listener = socket::bind(&settings.admin_socket).map_err(io_at(&settings.admin_socket))?;
    after_boot(&daemon);
    Ok(Started {
        daemon,
        listener,
        socket_path: settings.admin_socket.clone(),
        _lock: lock,
    })
}

/// Before the step records are read: finish or undo a placed link left half done, and relink
/// what differs from `current` (design section 6.5). Nothing yet.
fn before_records(_daemon: &Daemon) {}

/// The step records against the boot id, and each unfinished order run to its verdict (design
/// section 6.5). Nothing yet.
fn boot_pass(_daemon: &Daemon) {}

/// After the boot pass: the manifest's `boot` step of each service and the readiness notice
/// (design section 6.5). Nothing yet.
fn after_boot(_daemon: &Daemon) {}

/// Serves the admin socket of a started daemon, holding its lock, until the socket fails.
pub async fn serve(started: Started) -> Result<(), StartError> {
    let path = &started.socket_path;
    let incoming = socket::incoming(started.listener).map_err(io_at(path))?;
    // `started._lock` stays held until this returns.
    admin::serve(Arc::clone(&started.daemon), incoming)
        .await
        .map_err(StartError::from)
}
