//! [`TcpTransport`], the one module that opens a socket, against a listener on the loopback
//! address with a port the system chose; its bound is read on the fake clock, which only the
//! test moves.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use hostkeeper_link::testing::FakeClock;
use hostkeeper_link::{CALL_SECS, TcpTransport, Transport};

/// Catches a transport that does not reach the address it is given, or that lets a call run
/// past its bound: it connects to the listener, carries bytes both ways, and refuses a read or
/// a write once [`CALL_SECS`] have passed on its clock since the connect, however the socket
/// stands; an address no one listens on is refused.
#[test]
fn the_tcp_transport_carries_a_call_and_ends_it_at_its_bound() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback listener");
    let port = listener.local_addr().expect("its address").port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut ping = [0_u8; 4];
        stream.read_exact(&mut ping).expect("the ping");
        stream.write_all(b"pong").expect("the pong");
        ping
    });
    let clock = Arc::new(FakeClock::new(0, 500));
    let transport = TcpTransport::new(clock.clone());
    let mut conn = transport.connect("127.0.0.1", port).expect("connected");
    conn.write_all(b"ping").expect("written");
    conn.flush().expect("flushed");
    let mut pong = [0_u8; 4];
    conn.read_exact(&mut pong).expect("read");
    assert_eq!(&pong, b"pong");
    assert_eq!(&server.join().expect("the server"), b"ping");

    assert_eq!(CALL_SECS, 10);
    clock.advance(CALL_SECS - 1);
    let mut rest = [0_u8; 1];
    assert_eq!(conn.read(&mut rest).expect("the end of the stream"), 0);
    clock.advance(1);
    assert_eq!(
        conn.read(&mut rest).expect_err("past the bound").kind(),
        ErrorKind::TimedOut
    );
    assert_eq!(
        conn.write(b"x").expect_err("past the bound").kind(),
        ErrorKind::TimedOut
    );

    assert!(transport.connect("127.0.0.1", 0).is_err());
}
