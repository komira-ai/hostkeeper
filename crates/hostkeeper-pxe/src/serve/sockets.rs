//! The sockets of `serve`, bound before the privilege drop (the protocol ports are below 1024):
//! the DHCP port (67 by default), in proxy mode the proxyDHCP port (4011), the TFTP port (69)
//! and the HTTP port (80), each at the address its flag names. Every socket can share its port
//! with another socket bound to another address (`SO_REUSEADDR`), so a host's own DHCP client
//! does not stop it; the DHCP sockets may broadcast; and with `--interface` every socket takes
//! only the packets of that interface (`SO_BINDTODEVICE`).

use std::io;
use std::net::{SocketAddrV4, TcpListener, UdpSocket};

use socket2::{Domain, Protocol, Socket, Type};

/// The connections a listener queues before `serve` accepts them.
const BACKLOG: i32 = 128;

/// The sockets of a run.
#[derive(Debug)]
pub struct Bound {
    pub dhcp: UdpSocket,
    /// Proxy mode only.
    pub proxy: Option<UdpSocket>,
    pub tftp: UdpSocket,
    pub http: TcpListener,
}

/// Where each socket is bound, and on which interface.
#[derive(Clone, Debug)]
pub struct Addresses {
    pub dhcp: SocketAddrV4,
    /// `None` in router mode, which binds no proxyDHCP socket.
    pub proxy: Option<SocketAddrV4>,
    pub tftp: SocketAddrV4,
    pub http: SocketAddrV4,
    pub interface: Option<String>,
}

/// Why a socket could not be bound.
#[derive(Debug, thiserror::Error)]
#[error("cannot bind the {what} socket to {address}: {error}")]
pub struct BindError {
    what: &'static str,
    address: SocketAddrV4,
    error: io::Error,
}

impl Bound {
    /// Binds every socket `addresses` names.
    ///
    /// # Errors
    /// [`BindError`], naming the socket.
    pub fn bind(addresses: &Addresses) -> Result<Bound, BindError> {
        let interface = addresses.interface.as_deref();
        let udp = |what, address, broadcast| {
            udp(address, interface, broadcast).map_err(|error| BindError {
                what,
                address,
                error,
            })
        };
        Ok(Bound {
            dhcp: udp("DHCP", addresses.dhcp, true)?,
            proxy: addresses
                .proxy
                .map(|address| udp("proxyDHCP", address, true))
                .transpose()?,
            tftp: udp("TFTP", addresses.tftp, false)?,
            http: tcp(addresses.http, interface).map_err(|error| BindError {
                what: "HTTP",
                address: addresses.http,
                error,
            })?,
        })
    }
}

/// A socket of `kind`, set up as the module documentation says.
fn socket(kind: Type, protocol: Protocol, interface: Option<&str>) -> io::Result<Socket> {
    let socket = Socket::new(Domain::IPV4, kind, Some(protocol))?;
    socket.set_reuse_address(true)?;
    socket.bind_device(interface.map(str::as_bytes))?;
    socket.set_nonblocking(true)?;
    Ok(socket)
}

/// A UDP socket bound to `address`.
fn udp(address: SocketAddrV4, interface: Option<&str>, broadcast: bool) -> io::Result<UdpSocket> {
    let socket = socket(Type::DGRAM, Protocol::UDP, interface)?;
    socket.set_broadcast(broadcast)?;
    socket.bind(&address.into())?;
    Ok(socket.into())
}

/// A TCP listener at `address`.
fn tcp(address: SocketAddrV4, interface: Option<&str>) -> io::Result<TcpListener> {
    let socket = socket(Type::STREAM, Protocol::TCP, interface)?;
    socket.bind(&address.into())?;
    socket.listen(BACKLOG)?;
    Ok(socket.into())
}
