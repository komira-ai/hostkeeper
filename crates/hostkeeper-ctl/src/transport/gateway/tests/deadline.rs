//! The deadline of a gateway call, on a clock the tests move by hand: the budget's arithmetic,
//! progress that is slow but never stalls, name lookup, address selection and the status of each
//! failure.

use std::cell::{Cell, RefCell};
use std::io::{self, Cursor, Read, Write};
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use bytes::Bytes;
use prost::Message as _;
use tonic::Code;
use tonic::metadata::MetadataMap;

use super::{MAKE, response};
use crate::testutil::{from_json, pool};
use crate::transport::gateway::deadline::{
    Clock, Deadline, DeadlineIo, Limited, connect_within, failed, first_connected, resolve_within,
};
use crate::transport::gateway::{exchange, method};

/// A clock that moves only when told to.
struct FakeClock(Cell<Duration>);

impl FakeClock {
    fn at(seconds: u64) -> FakeClock {
        FakeClock(Cell::new(Duration::from_secs(seconds)))
    }

    fn advance(&self, by: Duration) {
        self.0.set(self.0.get() + by);
    }
}

impl Clock for FakeClock {
    fn elapsed(&self) -> Duration {
        self.0.get()
    }
}

/// A connection whose peer sends `chunk` bytes at a read, each read taking `step` of the fake
/// clock; it keeps what it was limited to and what was written to it.
struct Dribble<'a> {
    clock: &'a FakeClock,
    reply: Cursor<Vec<u8>>,
    step: Duration,
    chunk: usize,
    limits: Rc<RefCell<Vec<Duration>>>,
    written: Rc<RefCell<Vec<u8>>>,
}

impl Limited for Dribble<'_> {
    fn limit(&mut self, left: Duration) -> io::Result<()> {
        self.limits.borrow_mut().push(left);
        Ok(())
    }
}

impl Read for Dribble<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.chunk.min(buf.len());
        let got = self.reply.read(&mut buf[..n])?;
        self.clock.advance(self.step);
        Ok(got)
    }
}

impl Write for Dribble<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A call of Widgets.Make on `reply` sent a byte at a time, a second per read, under a budget of
/// `seconds`; the status or reply, what the connection was limited to and what was written.
fn dribbled(seconds: u64, reply: &[u8]) -> (Result<Bytes, tonic::Status>, Vec<Duration>, Vec<u8>) {
    let clock = FakeClock::at(0);
    let limits = Rc::new(RefCell::new(Vec::new()));
    let written = Rc::new(RefCell::new(Vec::new()));
    let socket = Dribble {
        clock: &clock,
        reply: Cursor::new(reply.to_vec()),
        step: Duration::from_secs(1),
        chunk: 1,
        limits: limits.clone(),
        written: written.clone(),
    };
    let deadline = Deadline::new(&clock, Duration::from_secs(seconds));
    let mut guarded = DeadlineIo::new(socket, deadline);
    let make = method(&pool(), MAKE).expect("the method");
    let request = from_json("Widget", "{}").encode_to_vec();
    let result = exchange(
        &mut guarded,
        "h",
        &make,
        MAKE,
        Bytes::from(request),
        &MetadataMap::new(),
    );
    let (limits, written) = (limits.borrow().clone(), written.borrow().clone());
    (result, limits, written)
}

/// Catches a timeout that restarts at every byte, which lets a peer that sends one byte before
/// each timeout keep the call running: with five seconds and a byte a second the sixth read is
/// refused as DEADLINE_EXCEEDED, and each read and write was limited to what was left.
#[test]
fn a_peer_that_sends_a_byte_at_a_time_runs_out_of_the_budget() {
    let (result, limits, written) = dribbled(5, &response("200 OK", "{}"));
    let status = result.expect_err("over the budget");
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert_eq!(
        status.message(),
        "the gateway connection failed: the call took longer than --timeout-secs"
    );
    let seconds: Vec<u64> = limits.iter().map(Duration::as_secs).collect();
    // The write, the flush, then five reads at 0 to 4 seconds in; the sixth read had none left.
    assert_eq!(seconds, [5, 5, 5, 4, 3, 2, 1]);
    assert!(limits.iter().all(|left| left.subsec_nanos() == 0));
    assert!(!written.is_empty());

    // The same peer inside a larger budget is read to the end of its reply.
    let (result, limits, _) = dribbled(1000, &response("200 OK", "{}"));
    result.expect("within the budget");
    assert!(limits.iter().all(|left| *left >= Duration::from_secs(900)));
}

/// Catches a request written after the budget ran out: with no time at all nothing is written
/// and the call ends as DEADLINE_EXCEEDED.
#[test]
fn nothing_is_written_with_no_time_left() {
    let (result, limits, written) = dribbled(0, &response("200 OK", "{}"));
    let status = result.expect_err("no time");
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert!(limits.is_empty() && written.is_empty());
}

/// Catches the budget's arithmetic: it counts from the moment it was made, ends exactly when the
/// time is used, and a budget longer than a year is a year.
#[test]
fn the_budget_counts_down_from_when_it_was_made() {
    let clock = FakeClock::at(10);
    let deadline = Deadline::new(&clock, Duration::from_secs(5));
    assert_eq!(deadline.remaining(), Some(Duration::from_secs(5)));
    clock.advance(Duration::from_secs(2));
    assert_eq!(deadline.remaining(), Some(Duration::from_secs(3)));
    clock.advance(Duration::new(2, 999_999_999));
    assert_eq!(deadline.remaining(), Some(Duration::from_nanos(1)));
    clock.advance(Duration::from_nanos(1));
    assert_eq!(deadline.remaining(), None);
    clock.advance(Duration::from_secs(60));
    assert_eq!(deadline.remaining(), None);

    let clock = FakeClock::at(0);
    assert_eq!(
        Deadline::new(&clock, Duration::MAX).remaining(),
        Some(Duration::from_secs(365 * 24 * 60 * 60))
    );
    assert_eq!(Deadline::new(&clock, Duration::ZERO).remaining(), None);
}

/// Catches a socket's timeout and the budget's own taken for a lost connection, and a reset
/// taken for a timeout.
#[test]
fn a_timeout_is_deadline_exceeded_and_other_failures_are_unavailable() {
    for kind in [io::ErrorKind::TimedOut, io::ErrorKind::WouldBlock] {
        let status = failed(&io::Error::new(kind, "slow"), "step one");
        assert_eq!(status.code(), Code::DeadlineExceeded, "{kind:?}");
        assert_eq!(
            status.message(),
            "step one: the call took longer than --timeout-secs"
        );
    }
    let status = failed(
        &io::Error::new(io::ErrorKind::ConnectionReset, "reset"),
        "step one",
    );
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(status.message(), "step one: reset");
}

/// Two addresses of loopback that are never connected to.
fn two_addresses() -> [SocketAddr; 2] {
    [
        "127.0.0.1:1".parse().expect("an address"),
        "127.0.0.2:2".parse().expect("an address"),
    ]
}

/// Catches only the first resolved address tried: the first fails, the second is connected, and
/// each attempt is given the time left; and the last error reported when every address fails,
/// the budget's end between addresses, and a name with no address.
#[test]
fn every_resolved_address_is_tried_in_order() {
    let [first, second] = two_addresses();
    let clock = FakeClock::at(2);
    let deadline = Deadline::new(&clock, Duration::from_secs(5));

    let mut tried = Vec::new();
    let connected = first_connected(&[first, second], &deadline, |address, left| {
        tried.push((*address, left));
        if *address == first {
            Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "first refused",
            ))
        } else {
            Ok("connected")
        }
    });
    assert_eq!(connected.expect("the second works"), "connected");
    assert_eq!(
        tried,
        [
            (first, Duration::from_secs(5)),
            (second, Duration::from_secs(5))
        ]
    );

    let mut tried = Vec::new();
    let error = first_connected::<()>(&[first, second], &deadline, |address, _| {
        tried.push(*address);
        let which = if *address == first { "first" } else { "second" };
        Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!("{which} refused"),
        ))
    })
    .expect_err("all refused");
    assert_eq!(tried, [first, second]);
    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    assert_eq!(error.to_string(), "second refused");

    let error = first_connected::<()>(&[], &deadline, |_, _| unreachable!("no address"))
        .expect_err("no address");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(error.to_string(), "the name resolved to no address");

    // The first attempt uses up the budget, so the second is not made.
    let mut tried = Vec::new();
    let error = first_connected::<()>(&[first, second], &deadline, |address, left| {
        tried.push(*address);
        clock.advance(left);
        Err(io::Error::new(io::ErrorKind::TimedOut, "first timed out"))
    })
    .expect_err("the budget is spent");
    assert_eq!(tried, [first]);
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(error.to_string(), "the deadline has passed");
}

/// Catches a refused connection reported as anything but UNAVAILABLE with the port named, now
/// without binding a port and releasing it: the refusal is a result handed to the address
/// selection.
#[test]
fn a_refused_connection_is_unavailable() {
    let [first, second] = two_addresses();
    let clock = FakeClock::at(0);
    let deadline = Deadline::new(&clock, Duration::from_secs(30));
    let error = first_connected::<()>(&[first, second], &deadline, |_, _| {
        Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "refused for the test",
        ))
    })
    .expect_err("refused");
    let status = failed(&error, "cannot connect to 127.0.0.1:9");
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(
        status.message(),
        "cannot connect to 127.0.0.1:9: refused for the test"
    );
}

/// Catches a failed lookup, or every address failing, reported as anything but the status of that
/// step with its name: a lookup that fails is UNAVAILABLE, a refused address is UNAVAILABLE, an
/// address that times out is DEADLINE_EXCEEDED, and a good address is connected.
#[test]
fn connecting_reports_the_step_that_failed() {
    let [first, second] = two_addresses();
    let clock = FakeClock::at(0);
    let deadline = Deadline::new(&clock, Duration::from_secs(30));
    let what = "cannot connect to host.example:9";

    let status = connect_within::<()>(
        what,
        &deadline,
        || Err(io::Error::other("no such name")),
        |_, _| unreachable!("no address to try"),
    )
    .expect_err("no lookup");
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(
        status.message(),
        "cannot connect to host.example:9: no such name"
    );

    let status = connect_within::<()>(
        what,
        &deadline,
        move || Ok(vec![first, second]),
        |_, _| Err(io::Error::new(io::ErrorKind::ConnectionRefused, "refused")),
    )
    .expect_err("all refused");
    assert_eq!(status.code(), Code::Unavailable);
    assert_eq!(
        status.message(),
        "cannot connect to host.example:9: refused"
    );

    let status = connect_within::<()>(
        what,
        &deadline,
        move || Ok(vec![first]),
        |_, _| Err(io::Error::new(io::ErrorKind::TimedOut, "slow")),
    )
    .expect_err("timed out");
    assert_eq!(status.code(), Code::DeadlineExceeded);
    assert_eq!(
        status.message(),
        "cannot connect to host.example:9: the call took longer than --timeout-secs"
    );

    let connected = connect_within(
        what,
        &deadline,
        move || Ok(vec![second]),
        |address, _| Ok(*address),
    );
    assert_eq!(connected.expect("connected"), second);
}

/// Catches name resolution outside the budget: with no time left the lookup does not even run, a
/// lookup that does not answer in time is a timeout while it is still blocked, an answer and
/// an error come back as they are, and a lookup that dies is reported as such.
#[test]
fn name_resolution_runs_inside_the_budget() {
    let [first, second] = two_addresses();
    let clock = FakeClock::at(0);

    let ran = Arc::new(AtomicBool::new(false));
    let flag = ran.clone();
    let spent = Deadline::new(&clock, Duration::ZERO);
    let error = resolve_within(
        move || {
            flag.store(true, Ordering::SeqCst);
            Ok(Vec::new())
        },
        &spent,
    )
    .expect_err("no time");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(!ran.load(Ordering::SeqCst));

    let deadline = Deadline::new(&clock, Duration::from_secs(30));
    let found = resolve_within(move || Ok(vec![first, second]), &deadline).expect("addresses");
    assert_eq!(found, [first, second]);
    let error = resolve_within(
        || Err(io::Error::new(io::ErrorKind::NotFound, "no such name")),
        &deadline,
    )
    .expect_err("an error");
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert_eq!(error.to_string(), "no such name");

    // A lookup blocked until the test lets it go, against a budget of one nanosecond.
    let (release, held) = mpsc::channel::<()>();
    let hurried = Deadline::new(&clock, Duration::from_nanos(1));
    let error = resolve_within(
        move || {
            let _ = held.recv();
            Ok(Vec::new())
        },
        &hurried,
    )
    .expect_err("still blocked");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(error.to_string(), "the deadline has passed");
    drop(release);

    let error = resolve_within(
        || -> io::Result<Vec<SocketAddr>> { panic!("the lookup crashed, for the test") },
        &deadline,
    )
    .expect_err("a dead lookup");
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(
        error.to_string(),
        "the name lookup stopped without an answer"
    );
}

/// A connection whose reads fail with `kind`.
struct Failing(io::ErrorKind);

impl Read for Failing {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(self.0, "failed, for the test"))
    }
}

impl Write for Failing {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Catches a socket timeout while reading the response (WouldBlock on Unix, TimedOut elsewhere)
/// reported as UNAVAILABLE, and a reset reported as a timeout.
#[test]
fn a_socket_timeout_while_reading_is_deadline_exceeded() {
    let make = method(&pool(), MAKE).expect("the method");
    let request = Bytes::from(from_json("Widget", "{}").encode_to_vec());
    for (kind, code, message) in [
        (
            io::ErrorKind::WouldBlock,
            Code::DeadlineExceeded,
            "the gateway connection failed: the call took longer than --timeout-secs",
        ),
        (
            io::ErrorKind::TimedOut,
            Code::DeadlineExceeded,
            "the gateway connection failed: the call took longer than --timeout-secs",
        ),
        (
            io::ErrorKind::ConnectionReset,
            Code::Unavailable,
            "the gateway connection failed: failed, for the test",
        ),
    ] {
        let status = exchange(
            &mut Failing(kind),
            "h",
            &make,
            MAKE,
            request.clone(),
            &MetadataMap::new(),
        )
        .expect_err("failed");
        assert_eq!(
            (status.code(), status.message()),
            (code, message),
            "{kind:?}"
        );
    }
}
