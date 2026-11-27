//! The real health probe: one HTTP/1.1 GET over a transport ([`Connect`]), read to at most
//! [`MAX_REPLY_BYTES`], with no redirect followed.
//!
//! The probe writes the request, reads until the header block ends, the cap is reached or the
//! peer closes, then drops the connection; only the status line is judged. When the status is not
//! 2xx it reads on, past the header block, until [`MAX_BODY_BYTES`] of the body are in, the cap is
//! reached, the peer closes or a read fails, and the reply carries those first bytes of the body
//! (a job agent's `503 not placeable: <reason>`); a read that fails there leaves the answer as it
//! is. A 3xx is an answer like any other (and is not healthy): its `Location` is never read. The
//! transport is a port so the tests drive this code with an in-memory peer; the daemon uses
//! [`TcpConnect`].

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};

use crate::error::ObserveError;
use crate::ports::{Deadline, HealthClient, HealthReply};
use crate::url::HealthUrl;

/// The most bytes of a reply the probe reads.
pub const MAX_REPLY_BYTES: usize = 4096;

/// The most bytes of the body of a reply that is not 2xx the probe keeps: the bound of the
/// heartbeat's `health_body`.
pub const MAX_BODY_BYTES: usize = hostkeeper_proto::health::MAX_HEALTH_BODY;

/// A connection: bytes both ways.
pub trait Duplex: Read + Write {}

impl<T: Read + Write> Duplex for T {}

/// Opens a connection to an address, its reads and writes bounded by the deadline.
pub trait Connect: Send + Sync {
    /// Connects to `addr`.
    fn connect(&self, addr: SocketAddr, deadline: Deadline) -> io::Result<Box<dyn Duplex>>;
}

/// The real transport: TCP, with the deadline as the connect timeout and as the timeout of each
/// read and write.
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpConnect;

impl Connect for TcpConnect {
    fn connect(&self, addr: SocketAddr, deadline: Deadline) -> io::Result<Box<dyn Duplex>> {
        let stream = TcpStream::connect_timeout(&addr, deadline.connect)?;
        stream.set_read_timeout(Some(deadline.read))?;
        stream.set_write_timeout(Some(deadline.read))?;
        Ok(Box::new(stream))
    }
}

/// The health probe over a transport.
#[derive(Clone, Debug, Default)]
pub struct HttpHealthClient<C> {
    connect: C,
}

impl<C: Connect> HttpHealthClient<C> {
    /// A probe that connects through `connect`.
    pub fn new(connect: C) -> HttpHealthClient<C> {
        HttpHealthClient { connect }
    }
}

impl<C: Connect> HealthClient for HttpHealthClient<C> {
    fn get(&self, url: &HealthUrl, deadline: Deadline) -> Result<HealthReply, ObserveError> {
        let failed = |e: io::Error| classify(url, &e);
        let mut stream = self.connect.connect(url.addr(), deadline).map_err(failed)?;
        stream
            .write_all(url.request().as_bytes())
            .and_then(|()| stream.flush())
            .map_err(failed)?;
        let mut reply = [0u8; MAX_REPLY_BYTES];
        let mut filled = 0;
        let mut stopped = None;
        while (filled < MAX_REPLY_BYTES) & !read_enough(&reply[..filled]) {
            match stream.read(&mut reply[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    stopped = Some(e);
                    break;
                }
            }
        }
        let reply = &reply[..filled];
        let Some(status) = status_code(reply) else {
            let why = stopped.unwrap_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the reply has no HTTP status line",
                )
            });
            return Err(ObserveError::Unreachable {
                url: url.to_string(),
                why: why.to_string(),
            });
        };
        let body_at = header_end(reply);
        let body_bytes_read = body_at.map_or(0, |end| filled - end);
        let body = match body_at {
            Some(end) if !is_2xx(status) => reply[end..filled.min(end + MAX_BODY_BYTES)].to_vec(),
            _ => Vec::new(),
        };
        Ok(HealthReply {
            status,
            body_bytes_read,
            body,
        })
    }
}

/// A transport error is [`ObserveError::Unreachable`] when it says the peer did not answer, and
/// [`ObserveError::Probe`] (the probe could not be made) otherwise.
fn classify(url: &HealthUrl, e: &io::Error) -> ObserveError {
    use io::ErrorKind as K;
    let peer = matches!(
        e.kind(),
        K::ConnectionRefused
            | K::ConnectionReset
            | K::ConnectionAborted
            | K::NotConnected
            | K::BrokenPipe
            | K::TimedOut
            | K::WouldBlock
            | K::UnexpectedEof
            | K::HostUnreachable
            | K::NetworkUnreachable
            | K::AddrNotAvailable
    );
    let (url, why) = (url.to_string(), e.to_string());
    if peer {
        ObserveError::Unreachable { url, why }
    } else {
        ObserveError::Probe { url, why }
    }
}

/// Whether the probe has read all it reads of `reply`: its header block, and for a status that
/// is not 2xx [`MAX_BODY_BYTES`] of the body after it.
fn read_enough(reply: &[u8]) -> bool {
    match (header_end(reply), status_code(reply)) {
        (None, _) => false,
        (Some(end), Some(status)) if !is_2xx(status) => reply.len() - end >= MAX_BODY_BYTES,
        (Some(_), _) => true,
    }
}

/// Whether `status` is 2xx.
fn is_2xx(status: u16) -> bool {
    (200..=299).contains(&status)
}

/// Where the header block ends (after its blank line), if it has.
fn header_end(reply: &[u8]) -> Option<usize> {
    reply
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| at + 4)
}

/// The status code of a reply that starts with an HTTP/1.0 or HTTP/1.1 status line.
fn status_code(reply: &[u8]) -> Option<u16> {
    let end = reply.windows(2).position(|w| w == b"\r\n")?;
    let line = std::str::from_utf8(&reply[..end]).ok()?;
    let (version, rest) = line.split_once(' ')?;
    let code = rest.split(' ').next().unwrap_or_default();
    let valid = matches!(version, "HTTP/1.0" | "HTTP/1.1")
        & (code.len() == 3)
        & code.bytes().all(|b| b.is_ascii_digit());
    code.parse().ok().filter(|_| valid)
}
