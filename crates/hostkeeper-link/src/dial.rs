//! [`TcpTransport`]: the one way out of this crate to the network, the [`Transport`] the
//! daemon gives [`crate::JsonMtlsLink`]. It resolves the coordinator's name, connects, and
//! bounds the whole call (connect, handshake, request and reply) by [`CALL_SECS`] on the
//! monotonic clock it is given, so a coordinator that answers a byte at a time cannot hold the
//! loop. Nothing else of the crate names a socket: the link speaks to any [`Transport`], and the
//! tests give it an in-process one.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use hostkeeper_steps::Clock;

use crate::mtls::{Conn, Transport};

/// The most seconds one heartbeat call may take, from the connect to the end of the reply.
pub const CALL_SECS: u64 = 10;

/// The production [`Transport`]: TCP to the coordinator's address, bounded by [`CALL_SECS`].
pub struct TcpTransport {
    clock: Arc<dyn Clock>,
}

impl TcpTransport {
    pub fn new(clock: Arc<dyn Clock>) -> TcpTransport {
        TcpTransport { clock }
    }
}

impl Transport for TcpTransport {
    fn connect(&self, host: &str, port: u16) -> io::Result<Box<dyn Conn>> {
        let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
        let deadline = self.clock.mono_secs().saturating_add(CALL_SECS);
        connect_any(&addrs, Arc::clone(&self.clock), deadline)
            .map(|conn| Box::new(conn) as Box<dyn Conn>)
    }
}

/// A connection to the first of `addrs` that answers, bounded by `deadline` on `clock`.
pub(crate) fn connect_any(
    addrs: &[SocketAddr],
    clock: Arc<dyn Clock>,
    deadline: u64,
) -> io::Result<Bounded> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the name has no address");
    for addr in addrs {
        let left = left_of(clock.as_ref(), deadline)?;
        match TcpStream::connect_timeout(addr, left) {
            Ok(stream) => {
                return Ok(Bounded {
                    stream,
                    clock,
                    deadline,
                });
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// The time left before `deadline`: at least a second while some is left, and a time-out once
/// none is.
fn left_of(clock: &dyn Clock, deadline: u64) -> io::Result<Duration> {
    match deadline.saturating_sub(clock.mono_secs()) {
        0 => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the heartbeat took longer than its bound",
        )),
        secs => Ok(Duration::from_secs(secs)),
    }
}

/// A TCP connection whose every read and write waits at most until its deadline.
pub(crate) struct Bounded {
    stream: TcpStream,
    clock: Arc<dyn Clock>,
    deadline: u64,
}

impl Read for Bounded {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = left_of(self.clock.as_ref(), self.deadline)?;
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

impl Write for Bounded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = left_of(self.clock.as_ref(), self.deadline)?;
        self.stream.set_write_timeout(Some(left))?;
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
