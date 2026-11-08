//! The readiness notice (design section 6.5): after the start pass, the daemon tells the service
//! manager it is ready, through the [`ReadyNotifier`] port.
//!
//! [`SystemdNotifier`] sends `READY=1` to the socket systemd names for a `Type=notify` unit. The
//! daemon has it only when it is started with the flag `--notify-systemd` (configuration is a
//! flag, AGENTS.md S2); the socket's address is the value systemd sets for the process, a
//! platform-set marker, which `main` reads and hands in ([`crate::cli`]). Without the flag the
//! daemon has [`NoNotifier`].
//!
//! [`ReadyFile`] is the readiness notice of a daemon under OpenRC (`--ready-file <file>`): the
//! file is removed when the daemon starts, so that it never says an earlier process was ready,
//! and written, with the daemon's pid, once the boot work is over. The init script of the Alpine
//! package waits for it after the start (packaging/alpine/hostkeeper.initd).
//!
//! The watchdog notice: once the daemon is ready, its heartbeat loop says it is alive after each
//! heartbeat, through the [`Watchdog`] port ([`crate::links`]). [`SystemdWatchdog`] sends
//! `WATCHDOG=1` to the same notify socket, and only when systemd set `WATCHDOG_USEC` for the
//! process, which it does for a unit with `WatchdogSec=` (another platform-set marker `main`
//! reads and hands in); without it, and without `--notify-systemd`, the notice is sent nowhere.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::path::{Path, PathBuf};

/// Tells whoever started the daemon that it is ready. The daemon calls it exactly once, after
/// the start pass and the boot steps, whatever they ended as.
pub trait ReadyNotifier: Send + Sync {
    fn ready(&self) -> io::Result<()>;
}

/// The message of the readiness notice.
pub const READY: &[u8] = b"READY=1";

/// The readiness notice of systemd: `READY=1` in one datagram to the notify socket, a path, or
/// an abstract name after `@`. The datagram is sent without waiting: a socket whose queue is full
/// is an error the daemon reports, never a start that hangs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemdNotifier {
    socket: OsString,
}

impl SystemdNotifier {
    /// The notifier of the notify socket `socket`, as systemd gives its address.
    pub fn new(socket: impl Into<OsString>) -> SystemdNotifier {
        SystemdNotifier {
            socket: socket.into(),
        }
    }

    /// The address of the notify socket.
    fn address(&self) -> io::Result<SocketAddr> {
        let bytes = self.socket.as_bytes();
        match bytes.strip_prefix(b"@") {
            Some(name) => abstract_address(name),
            None => SocketAddr::from_pathname(OsStr::from_bytes(bytes)),
        }
    }

    /// Sends `message` in one datagram to the notify socket, without waiting.
    fn send(&self, message: &[u8]) -> io::Result<()> {
        let address = self.address()?;
        let socket = UnixDatagram::unbound()?;
        socket.set_nonblocking(true)?;
        socket.send_to_addr(message, &address)?;
        Ok(())
    }
}

impl ReadyNotifier for SystemdNotifier {
    fn ready(&self) -> io::Result<()> {
        self.send(READY)
    }
}

/// The address of the abstract socket `name` (Linux's; systemd may name one that way).
#[cfg(target_os = "linux")]
fn abstract_address(name: &[u8]) -> io::Result<SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    SocketAddr::from_abstract_name(name)
}

/// No other system has abstract sockets.
#[cfg(not(target_os = "linux"))]
fn abstract_address(_name: &[u8]) -> io::Result<SocketAddr> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "an abstract notify socket exists on Linux only",
    ))
}

/// The readiness notice as a file: the daemon's pid and a line feed, written through a temporary
/// file beside it (its name and `.tmp`) and renamed into place, so that whoever waits for the
/// file never reads half of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyFile {
    path: PathBuf,
}

impl ReadyFile {
    /// The notice of `path`, which is removed now (a missing file is fine): it says nothing of an
    /// earlier process. An error is a file that is there and cannot be removed.
    pub fn new(path: &Path) -> io::Result<ReadyFile> {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(ReadyFile {
            path: path.to_path_buf(),
        })
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ReadyNotifier for ReadyFile {
    fn ready(&self) -> io::Result<()> {
        let mut tmp = self.path.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        fs::write(&tmp, format!("{}\n", std::process::id()))?;
        fs::rename(&tmp, &self.path)
    }
}

/// The notifier of a daemon started without `--notify-systemd` or `--ready-file`: nobody waits
/// for the notice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoNotifier;

impl ReadyNotifier for NoNotifier {
    fn ready(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Tells the service manager the daemon's heartbeat loop is alive. The loop calls it after each
/// heartbeat and nowhere else, so a loop that hangs stops the notices and the service manager
/// restarts the daemon.
pub trait Watchdog: Send + Sync {
    fn alive(&self) -> io::Result<()>;
}

/// The message of the watchdog notice.
pub const WATCHDOG: &[u8] = b"WATCHDOG=1";

/// The watchdog notice of systemd: `WATCHDOG=1` in one datagram to the notify socket, sent the
/// way [`SystemdNotifier`] sends `READY=1`, when systemd runs a watchdog for the daemon; when it
/// runs none, the notice is sent nowhere and is `Ok`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemdWatchdog {
    /// The notify socket, when systemd runs a watchdog for the daemon.
    socket: Option<SystemdNotifier>,
}

impl SystemdWatchdog {
    /// The watchdog notice to the notify socket `socket`, as systemd gives its address.
    /// `watchdog_usec` is `WATCHDOG_USEC` as systemd set it for the process: systemd sets it
    /// when the unit has `WatchdogSec=`, and only then does the notice go out.
    pub fn new(socket: impl Into<OsString>, watchdog_usec: Option<&OsStr>) -> SystemdWatchdog {
        SystemdWatchdog {
            socket: watchdog_usec.map(|_| SystemdNotifier::new(socket)),
        }
    }
}

impl Watchdog for SystemdWatchdog {
    fn alive(&self) -> io::Result<()> {
        match &self.socket {
            Some(socket) => socket.send(WATCHDOG),
            None => Ok(()),
        }
    }
}

/// The watchdog of a daemon started without `--notify-systemd`: nobody watches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoWatchdog;

impl Watchdog for NoWatchdog {
    fn alive(&self) -> io::Result<()> {
        Ok(())
    }
}
