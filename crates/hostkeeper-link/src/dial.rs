//! [`TcpTransport`] and [`TcpDial`]: the one way out of this crate to the network, the
//! [`Transport`] the daemon gives [`crate::JsonMtlsLink`] and the [`GrpcDial`] it gives
//! [`crate::GrpcMtlsLink`]. [`TcpTransport`] connects to the address it is given (a name is
//! resolved), and bounds the whole connection (connect, handshake, request and reply) by the
//! seconds it is given, [`CALL_SECS`] by default, on the monotonic clock it is given, so a
//! coordinator that answers a byte at a time cannot hold the loop; the JSON link gives each
//! address it dials the time left of one [`CALL_SECS`] for the whole call. [`TcpDial`] connects
//! on the async side; the gRPC link bounds the whole call, connect included, by [`CALL_SECS`]. Nothing else of the crate names a socket: the links speak to any
//! [`Transport`] or [`GrpcDial`], and the tests give them in-process ones.
//!
//! The links dial addresses, which [`crate::Reach`] gives them; it looks the coordinator's name
//! up through a [`Lookup`]. [`SystemLookup`] is the system resolver's, and [`BackgroundLookup`]
//! runs any lookup on a thread of its own and waits a bounded time for it: a lookup that is still
//! running then (a resolver whose upstream is gone can take tens of seconds to give up) fails that
//! call, keeps running, and its answer is the next call's. The daemon gives the reach a
//! [`BackgroundLookup`] over [`SystemLookup`] that waits [`LOOKUP_SECS`].

use std::io::{self, Read, Write};
// The address types, which the reach and its fakes name through this module: it is the one
// source of the crate that names the standard library's network module.
pub(crate) use std::net::{IpAddr, SocketAddr};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hostkeeper_steps::Clock;

use crate::grpc::{AsyncConn, Dialing, GrpcDial};
use crate::mtls::{Conn, Transport};
use crate::reach::Lookup;

/// The most seconds one heartbeat call may take, from the connect to the end of the reply.
pub const CALL_SECS: u64 = 10;

/// The most seconds a call waits for the lookup of the coordinator's name before it dials the
/// other addresses it knows.
pub const LOOKUP_SECS: u64 = 5;

/// The system resolver: `getaddrinfo`, as the standard library calls it. It waits as long as the
/// resolver does; [`BackgroundLookup`] bounds that wait.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemLookup;

impl Lookup for SystemLookup {
    fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok((host, port).to_socket_addrs()?.collect())
    }
}

/// A lookup running on its own thread, and the name it is for.
struct Running {
    host: String,
    port: u16,
    answer: Receiver<io::Result<Vec<SocketAddr>>>,
}

/// A [`Lookup`] that runs `inner` on a thread of its own and waits at most its bound for it. A
/// lookup still running at the bound fails the call with [`io::ErrorKind::TimedOut`] and keeps
/// running; the next call for the same name waits for that one again instead of starting
/// another, so there is at most one lookup running at a time, and an answer that came late is
/// the next call's.
pub struct BackgroundLookup {
    inner: Arc<dyn Lookup>,
    wait: Duration,
    running: Mutex<Option<Running>>,
}

impl BackgroundLookup {
    /// Lookups through `inner`, each waited for at most `wait`.
    pub fn new(inner: Arc<dyn Lookup>, wait: Duration) -> BackgroundLookup {
        BackgroundLookup {
            inner,
            wait,
            running: Mutex::new(None),
        }
    }

    /// A lookup of `host` on `port` started on a new thread.
    fn start(&self, host: &str, port: u16) -> io::Result<Running> {
        let (send, answer) = mpsc::channel();
        let inner = Arc::clone(&self.inner);
        let name = host.to_owned();
        std::thread::Builder::new()
            .name("coordinator-lookup".to_owned())
            .spawn(move || {
                // The caller that stopped waiting may be gone: the answer is then nobody's.
                let _ = send.send(inner.lookup(&name, port));
            })?;
        Ok(Running {
            host: host.to_owned(),
            port,
            answer,
        })
    }
}

impl Lookup for BackgroundLookup {
    fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        let lookup = match running.take() {
            Some(earlier) if earlier.host == host && earlier.port == port => earlier,
            _ => self.start(host, port)?,
        };
        match lookup.answer.recv_timeout(self.wait) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => {
                *running = Some(lookup);
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the lookup is still running",
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(io::Error::other("the lookup ended with no answer"))
            }
        }
    }
}

/// The production [`Transport`]: TCP to the coordinator's address, bounded by [`CALL_SECS`].
pub struct TcpTransport {
    clock: Arc<dyn Clock>,
    secs: u64,
}

impl TcpTransport {
    pub fn new(clock: Arc<dyn Clock>) -> TcpTransport {
        TcpTransport {
            clock,
            secs: CALL_SECS,
        }
    }
}

impl Transport for TcpTransport {
    fn connect(&self, host: &str, port: u16) -> io::Result<Box<dyn Conn>> {
        self.connect_within(host, port, self.secs)
    }

    /// Bounded by `secs` from the connect, in place of [`CALL_SECS`].
    fn connect_within(&self, host: &str, port: u16, secs: u64) -> io::Result<Box<dyn Conn>> {
        let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs()?.collect();
        let deadline = self.clock.mono_secs().saturating_add(secs);
        connect_any(&addrs, Arc::clone(&self.clock), deadline)
            .map(|conn| Box::new(conn) as Box<dyn Conn>)
    }
}

/// The production [`GrpcDial`]: a TCP connection to the address it is given (a name would be
/// resolved when it is dialed; the gRPC link gives an address).
#[derive(Clone, Copy, Debug, Default)]
pub struct TcpDial;

impl GrpcDial for TcpDial {
    fn dial(&self, host: &str, port: u16) -> Dialing<'_> {
        let host = host.to_owned();
        Box::pin(async move {
            let stream = tokio::net::TcpStream::connect((host.as_str(), port)).await?;
            Ok(Box::new(stream) as Box<dyn AsyncConn>)
        })
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
            "the call took longer than its bound",
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
