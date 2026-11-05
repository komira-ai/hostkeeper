//! The deadline of one gateway call. One budget, `--timeout-secs`, is shared by name resolution,
//! the connection of each address, the TLS handshake, the request write and every read of the
//! response: a stream wrapper works out the time left before each read and write, refuses with a
//! timeout when there is none, and otherwise limits the socket to what is left. A peer that
//! sends a byte before each timeout therefore cannot keep the call running past the budget.
//!
//! The time comes from a [`Clock`], so that tests advance it by hand.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tonic::Status;

/// A source of time that only moves forward.
pub(crate) trait Clock {
    /// The time since the clock began.
    fn elapsed(&self) -> Duration;
}

/// The operating system's monotonic clock.
pub(crate) struct Monotonic(Instant);

impl Monotonic {
    /// A clock that begins now.
    pub(crate) fn start() -> Monotonic {
        Monotonic(Instant::now())
    }
}

impl Clock for Monotonic {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

/// The longest budget kept; a longer `--timeout-secs` is one year.
const LONGEST: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// A budget of time that began when it was made.
#[derive(Clone, Copy)]
pub(crate) struct Deadline<'c> {
    clock: &'c dyn Clock,
    start: Duration,
    timeout: Duration,
}

impl<'c> Deadline<'c> {
    /// A budget of `timeout` (at most a year) starting now on `clock`.
    pub(crate) fn new(clock: &'c dyn Clock, timeout: Duration) -> Deadline<'c> {
        Deadline {
            clock,
            start: clock.elapsed(),
            timeout: timeout.min(LONGEST),
        }
    }

    /// The time left, or `None` when none is.
    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.timeout
            .checked_sub(self.clock.elapsed().saturating_sub(self.start))
            .filter(|left| !left.is_zero())
    }
}

/// The error of a budget that has run out.
fn expired() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "the deadline has passed")
}

/// Whether `e` is a timeout: the budget's own, or a socket's (`WouldBlock` on Unix).
fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

/// The status of a failed step: DEADLINE_EXCEEDED for a timeout, else UNAVAILABLE. `what` says
/// which step; the message is `<what>: <why>`.
pub(crate) fn failed(e: &io::Error, what: &str) -> Status {
    if is_timeout(e) {
        Status::deadline_exceeded(format!("{what}: the call took longer than --timeout-secs"))
    } else {
        Status::unavailable(format!("{what}: {e}"))
    }
}

/// A connection whose reads and writes can be limited to a time.
pub(crate) trait Limited {
    /// Makes a read or a write that waits longer than `left` fail with a timeout.
    fn limit(&mut self, left: Duration) -> io::Result<()>;
}

impl Limited for TcpStream {
    fn limit(&mut self, left: Duration) -> io::Result<()> {
        // Each fails only for a zero duration, which `left` never is.
        self.set_read_timeout(Some(left))
            .and_then(|()| self.set_write_timeout(Some(left)))
    }
}

/// A connection every read and write of which is held to a [`Deadline`].
pub(crate) struct DeadlineIo<'c, S> {
    inner: S,
    deadline: Deadline<'c>,
}

impl<'c, S: Limited> DeadlineIo<'c, S> {
    /// `inner` under `deadline`.
    pub(crate) fn new(inner: S, deadline: Deadline<'c>) -> DeadlineIo<'c, S> {
        DeadlineIo { inner, deadline }
    }

    /// Limits the connection to the time left; a timeout error when none is.
    fn arm(&mut self) -> io::Result<()> {
        let left = self.deadline.remaining().ok_or_else(expired)?;
        self.inner.limit(left)
    }
}

impl<S: Limited + Read> Read for DeadlineIo<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.arm()?;
        self.inner.read(buf)
    }
}

impl<S: Limited + Write> Write for DeadlineIo<'_, S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.arm()?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.arm()?;
        self.inner.flush()
    }
}

/// The addresses `resolve` returns, if it returns within the budget. The lookup runs on its own
/// thread, which is left to finish when the budget is spent first; with no time left it does not
/// run at all.
pub(crate) fn resolve_within<R>(resolve: R, deadline: &Deadline<'_>) -> io::Result<Vec<SocketAddr>>
where
    R: FnOnce() -> io::Result<Vec<SocketAddr>> + Send + 'static,
{
    let left = deadline.remaining().ok_or_else(expired)?;
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        // The receiver is gone when the budget ran out first.
        let _ = send.send(resolve());
    });
    match receive.recv_timeout(left) {
        Ok(addresses) => addresses,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(expired()),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "the name lookup stopped without an answer",
        )),
    }
}

/// What `attempt` makes of the first of `addresses` that it succeeds on, each tried in order
/// with the time left; the last error when all fail; a timeout when the budget runs out; and an
/// error of its own when there is no address.
pub(crate) fn first_connected<T>(
    addresses: &[SocketAddr],
    deadline: &Deadline<'_>,
    mut attempt: impl FnMut(&SocketAddr, Duration) -> io::Result<T>,
) -> io::Result<T> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "the name resolved to no address");
    for address in addresses {
        let left = deadline.remaining().ok_or_else(expired)?;
        match attempt(address, left) {
            Ok(connected) => return Ok(connected),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// What `attempt` makes of the first address that `resolve` returns and it succeeds on, within
/// the budget; a failure of the lookup or of every attempt is the status `failed` makes of it,
/// `what` saying which step.
pub(crate) fn connect_within<T>(
    what: &str,
    deadline: &Deadline<'_>,
    resolve: impl FnOnce() -> io::Result<Vec<SocketAddr>> + Send + 'static,
    attempt: impl FnMut(&SocketAddr, Duration) -> io::Result<T>,
) -> Result<T, Status> {
    let addresses = resolve_within(resolve, deadline).map_err(|e| failed(&e, what))?;
    first_connected(&addresses, deadline, attempt).map_err(|e| failed(&e, what))
}
