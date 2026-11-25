//! The PROXY protocol header (versions 1 and 2) on the heartbeat listener, read from the proxies
//! `--heartbeat-proxy-protocol-from` names and from nothing else.
//!
//! A TCP passthrough in front of the heartbeat listener (a proxy that forwards the mutual-TLS
//! connection unopened) makes every connection's peer the proxy, and the coordinator would record
//! the proxy as the address of every host ([`crate::ssh_access`]). A proxy that sends a PROXY
//! protocol header before the client's bytes says who the client is. The listener reads one only
//! on a connection whose peer is a source the flag names ([`Proxies::trusts`]): there the header
//! is required, and the address of the connection from then on is the header's source. A
//! connection from any other peer is read as before, its first byte the TLS handshake's, so a
//! header it sends fails that handshake. Without the flag no header is read on any connection.
//!
//! The header ([`read`]) is version 2 (binary) or version 1 (text), as the
//! [specification](https://www.haproxy.org/download/3.0/doc/proxy-protocol.txt) defines them. A
//! header that says the proxy itself made the connection (version 2's `LOCAL`, version 1's
//! `UNKNOWN`) or names no IP source (version 2's `AF_UNSPEC` and `AF_UNIX`) is taken, and the
//! connection has no address: a heartbeat on it records none, so the proxy's own address is never
//! recorded. Nothing past the header is read here, so the TLS handshake starts at the first byte
//! after it.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt};

/// The first 12 bytes of a version 2 header.
const V2_SIGNATURE: [u8; 12] = *b"\r\n\r\n\0\r\nQUIT\n";

/// What a version 1 header starts with.
const V1_START: &[u8] = b"PROXY ";

/// The longest version 1 header, its `\r\n` included.
const V1_MAX: usize = 107;

/// A source `--heartbeat-proxy-protocol-from` names: an IP address (`192.0.2.10`, `2001:db8::1`)
/// or a network of them (`192.0.2.0/24`, `2001:db8::/32`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProxySource {
    /// The network's address: no bit past `prefix` is set.
    network: IpAddr,
    /// How many leading bits of an address must be the network's.
    prefix: u8,
}

/// Why a value of `--heartbeat-proxy-protocol-from` is no source.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProxySourceError {
    /// The part before the `/` is no IP address.
    #[error("{0:?} is no IP address")]
    Address(String),
    /// The part after the `/` is no number of bits of the address.
    #[error("{0:?} is no prefix length of the address (IPv4 1 to 32, IPv6 1 to 128)")]
    Prefix(String),
    /// The prefix is `/0`, which would trust every source.
    #[error("a /0 network trusts every source; name the proxy's address or its network")]
    Everything,
    /// An address has bits set past its prefix (`192.0.2.10/24`).
    #[error("{0} has bits set past its prefix; write the network's address")]
    HostBits(String),
    /// An IPv4 address written as IPv6 (`::ffff:192.0.2.10`), which a peer is never compared as.
    #[error("{0} is an IPv4 address written as IPv6; write it as IPv4")]
    Mapped(String),
}

impl FromStr for ProxySource {
    type Err = ProxySourceError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (address, prefix) = match text.split_once('/') {
            Some((address, prefix)) => (address, Some(prefix)),
            None => (text, None),
        };
        let network: IpAddr = address
            .parse()
            .map_err(|_| ProxySourceError::Address(address.to_owned()))?;
        if network.to_canonical() != network {
            return Err(ProxySourceError::Mapped(network.to_string()));
        }
        let prefix = match prefix {
            None => bits_of(network),
            Some(given) => {
                let bits = given
                    .bytes()
                    .all(|b| b.is_ascii_digit())
                    .then(|| given.parse::<u8>().ok())
                    .flatten()
                    .filter(|bits| *bits <= bits_of(network))
                    .ok_or_else(|| ProxySourceError::Prefix(given.to_owned()))?;
                if bits == 0 {
                    return Err(ProxySourceError::Everything);
                }
                bits
            }
        };
        if masked(network, prefix) != network {
            return Err(ProxySourceError::HostBits(text.to_owned()));
        }
        let source = Self { network, prefix };
        Ok(source)
    }
}

impl ProxySource {
    /// Whether `ip` is this source, or an address of its network. An IPv4 address written as IPv6
    /// is compared as the IPv4 address it is.
    #[must_use]
    pub(crate) fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        ip.is_ipv4() == self.network.is_ipv4() && masked(ip, self.prefix) == self.network
    }
}

/// The number of bits of an address of `ip`'s family.
fn bits_of(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

/// `ip` with every bit past the first `prefix` cleared (`prefix` is at least 1).
fn masked(ip: IpAddr, prefix: u8) -> IpAddr {
    let prefix = u32::from(prefix);
    match ip {
        IpAddr::V4(v4) => {
            let mask = u32::MAX.checked_shr(prefix).map_or(u32::MAX, |rest| !rest);
            IpAddr::V4(Ipv4Addr::from(u32::from(v4) & mask))
        }
        IpAddr::V6(v6) => {
            let mask = u128::MAX
                .checked_shr(prefix)
                .map_or(u128::MAX, |rest| !rest);
            IpAddr::V6(Ipv6Addr::from(u128::from(v6) & mask))
        }
    }
}

/// The sources a listener reads a PROXY protocol header from: none by default.
#[derive(Clone, Debug, Default)]
pub(crate) struct Proxies(Arc<[ProxySource]>);

impl Proxies {
    /// The sources of `--heartbeat-proxy-protocol-from`.
    pub(crate) fn new(sources: &[ProxySource]) -> Self {
        Self(Arc::from(sources))
    }

    /// Whether a connection whose peer is `remote` starts with a header to read: its peer is one of
    /// the sources. A connection with no peer address (an in-memory one) never does.
    pub(crate) fn trusts(&self, remote: Option<SocketAddr>) -> bool {
        remote.is_some_and(|peer| self.0.iter().any(|source| source.contains(peer.ip())))
    }
}

/// Why a connection from a trusted proxy is refused before its TLS handshake.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Refused {
    /// The connection does not start with a header.
    #[error("the connection does not start with a PROXY protocol header")]
    NoHeader,
    /// The header breaks the specification in the way named.
    #[error("the PROXY protocol header {0}")]
    Malformed(&'static str),
    /// The connection ended or failed before the header did.
    #[error("the PROXY protocol header could not be read: {0}")]
    Io(#[from] std::io::Error),
}

/// Reads the header `stream` starts with, and nothing past it: the client's address it names, or
/// `None` when it names none (the proxy's own connection, or a source with no IP address).
///
/// # Errors
/// [`Refused`]: the stream does not start with a header, the header is malformed, or the stream
/// ends or fails first.
pub(crate) async fn read<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> Result<Option<SocketAddr>, Refused> {
    // Either version's header is longer than 12 bytes, so these are the header's.
    let mut start = [0_u8; 12];
    stream.read_exact(&mut start).await?;
    if start == V2_SIGNATURE {
        let mut fixed = [0_u8; 4];
        stream.read_exact(&mut fixed).await?;
        let [version_command, family, high, low] = fixed;
        let mut addresses = vec![0_u8; usize::from(u16::from_be_bytes([high, low]))];
        stream.read_exact(&mut addresses).await?;
        return version_2(version_command, family, &addresses).map_err(Refused::Malformed);
    }
    if !start.starts_with(V1_START) {
        return Err(Refused::NoHeader);
    }
    // A byte at a time: a read past the `\r\n` would take the handshake's first bytes.
    let mut line = start.to_vec();
    while !line.ends_with(b"\r\n") {
        if line.len() == V1_MAX {
            return Err(Refused::Malformed("is longer than 107 bytes"));
        }
        line.push(stream.read_u8().await?);
    }
    version_1(&line).map_err(Refused::Malformed)
}

/// The source a version 2 header names, from its version and command byte, its family byte and
/// the bytes of its addresses (TLVs after them are skipped).
pub(crate) fn version_2(
    version_command: u8,
    family: u8,
    addresses: &[u8],
) -> Result<Option<SocketAddr>, &'static str> {
    if version_command >> 4 != 2 {
        return Err("is of a version other than 2");
    }
    match version_command & 0x0F {
        // LOCAL: the proxy's own connection, such as its health check.
        0x0 => return Ok(None),
        0x1 => {}
        _ => return Err("has a command other than LOCAL and PROXY"),
    }
    match family {
        // TCP over IPv4: source, destination, source port, destination port.
        0x11 => {
            let Some(&[a, b, c, d, _, _, _, _, high, low]) = addresses.get(..10) else {
                return Err("is shorter than its IPv4 addresses");
            };
            let ip = Ipv4Addr::new(a, b, c, d);
            Ok(Some(SocketAddr::new(
                ip.into(),
                u16::from_be_bytes([high, low]),
            )))
        }
        // TCP over IPv6: as IPv4, each address 16 bytes.
        0x21 => {
            let (Some(source), Some(&[high, low])) = (addresses.get(..16), addresses.get(32..34))
            else {
                return Err("is shorter than its IPv6 addresses");
            };
            let mut octets = [0_u8; 16];
            octets.copy_from_slice(source);
            let ip = Ipv6Addr::from(octets);
            Ok(Some(SocketAddr::new(
                ip.into(),
                u16::from_be_bytes([high, low]),
            )))
        }
        // AF_UNSPEC, and a stream over AF_UNIX: no IP source.
        0x00 | 0x31 => Ok(None),
        _ => Err("names a family and protocol other than a stream over IPv4, IPv6 or UNIX"),
    }
}

/// The source a version 1 header names, from the whole line, its `\r\n` included.
pub(crate) fn version_1(line: &[u8]) -> Result<Option<SocketAddr>, &'static str> {
    let text = line
        .strip_suffix(b"\r\n")
        .and_then(|text| std::str::from_utf8(text).ok())
        .ok_or("is no line of text")?;
    let fields: Vec<&str> = text.split(' ').collect();
    match fields.as_slice() {
        // UNKNOWN: the proxy's own connection, or one it cannot name; the rest is ignored.
        ["PROXY", "UNKNOWN", ..] => Ok(None),
        [
            "PROXY",
            "TCP4",
            source,
            destination,
            source_port,
            destination_port,
        ] => {
            let (Ok(ip), Ok(_)) = (source.parse::<Ipv4Addr>(), destination.parse::<Ipv4Addr>())
            else {
                return Err("names an address that is no IPv4 address");
            };
            Ok(Some(SocketAddr::new(
                ip.into(),
                ports(source_port, destination_port)?,
            )))
        }
        [
            "PROXY",
            "TCP6",
            source,
            destination,
            source_port,
            destination_port,
        ] => {
            let (Ok(ip), Ok(_)) = (source.parse::<Ipv6Addr>(), destination.parse::<Ipv6Addr>())
            else {
                return Err("names an address that is no IPv6 address");
            };
            Ok(Some(SocketAddr::new(
                ip.into(),
                ports(source_port, destination_port)?,
            )))
        }
        _ => Err("is not `PROXY TCP4`, `PROXY TCP6` or `PROXY UNKNOWN` and its fields"),
    }
}

/// The source port of a version 1 header, both ports being decimal numbers below 65536.
fn ports(source: &str, destination: &str) -> Result<u16, &'static str> {
    let port = |text: &str| {
        text.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| text.parse::<u16>().ok())
            .flatten()
    };
    match (port(source), port(destination)) {
        (Some(source), Some(_)) => Ok(source),
        _ => Err("names a port that is no number from 0 to 65535"),
    }
}

#[cfg(test)]
#[path = "proxy_protocol_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "proxy_protocol_listen_tests.rs"]
mod listen_tests;
