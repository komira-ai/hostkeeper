//! The readiness notice of a `Type=notify` unit: `READY=1` in one datagram to the socket systemd
//! names (a path, or an abstract name after `@`), sent once the start of `serve` is complete.
//! The datagram is sent without waiting, so a socket whose queue is full is an error the run
//! reports, never a start that hangs.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};

/// The message of the readiness notice.
pub(crate) const READY: &[u8] = b"READY=1";

/// The address of the notify socket `socket`, as systemd gives it.
fn address(socket: &OsStr) -> io::Result<SocketAddr> {
    let bytes = socket.as_bytes();
    match bytes.strip_prefix(b"@") {
        Some(name) => abstract_address(name),
        None => SocketAddr::from_pathname(OsStr::from_bytes(bytes)),
    }
}

/// Sends [`READY`] to `socket`.
///
/// # Errors
/// The socket's address is not valid, or the datagram could not be sent.
pub(crate) fn ready(socket: &OsStr) -> io::Result<()> {
    let address = address(socket)?;
    let sender = UnixDatagram::unbound()?;
    sender.set_nonblocking(true)?;
    sender.send_to_addr(READY, &address)?;
    Ok(())
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
        "abstract sockets exist on Linux only",
    ))
}
