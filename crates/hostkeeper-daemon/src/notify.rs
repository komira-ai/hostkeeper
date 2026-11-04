//! The readiness notice (design section 6.5): after the start pass, the daemon tells the service
//! manager it is ready, through the [`ReadyNotifier`] port.
//!
//! [`SystemdNotifier`] sends `READY=1` to the socket systemd names for a `Type=notify` unit. The
//! daemon has it only when it is started with the flag `--notify-systemd` (configuration is a
//! flag, AGENTS.md S2); the socket's address is the value systemd sets for the process, a
//! platform-set marker, which `main` reads and hands in ([`crate::cli`]). Without the flag the
//! daemon has [`NoNotifier`].

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};

/// Tells whoever started the daemon that it is ready. The daemon calls it exactly once, after
/// the start pass and the boot steps, whatever they ended as.
pub trait ReadyNotifier: Send + Sync {
    fn ready(&self) -> io::Result<()>;
}

/// The message of the readiness notice.
pub const READY: &[u8] = b"READY=1";

/// The readiness notice of systemd: `READY=1` in one datagram to the notify socket, a path, or
/// an abstract name after `@`.
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
}

impl ReadyNotifier for SystemdNotifier {
    fn ready(&self) -> io::Result<()> {
        let address = self.address()?;
        UnixDatagram::unbound()?.send_to_addr(READY, &address)?;
        Ok(())
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

/// The notifier of a daemon started without `--notify-systemd`: nobody waits for the notice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoNotifier;

impl ReadyNotifier for NoNotifier {
    fn ready(&self) -> io::Result<()> {
        Ok(())
    }
}
