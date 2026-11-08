//! The server over real sockets on loopback (port 0, chosen by the system), asked through the
//! real probe and through raw connections; and the probe against servers that misbehave.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;

use rustix::event::{PollFd, PollFlags, poll};

use hello::ports::{HttpProbe, Probe};
use hello::server::{Answer, MAX_HEAD, serve};

/// A listener on a port of the system's choosing, and that port.
fn listener() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("the address").port();
    (listener, port)
}

/// Serves in a thread that lives as long as the test binary: the server never returns.
fn serving(version: &'static str, fail_ready: bool) -> u16 {
    let (listener, port) = listener();
    thread::spawn(move || {
        serve(
            listener,
            &Answer {
                version,
                fail_ready,
            },
        )
    });
    port
}

/// A server that answers its first connection with `behave` and is gone.
fn once(behave: impl FnOnce(TcpStream) + Send + 'static) -> u16 {
    let (listener, port) = listener();
    thread::spawn(move || {
        let _ = listener.accept().map(|(stream, _)| behave(stream));
    });
    port
}

fn raw(port: u16, request: &[u8], half_close: bool) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(request).expect("send");
    if half_close {
        stream.shutdown(Shutdown::Write).expect("half close");
    }
    let mut answer = String::new();
    stream.read_to_string(&mut answer).expect("read");
    answer
}

/// Catches the probe and the server disagreeing on a request and its answer: the status and the
/// body of each path over a socket.
#[test]
fn the_probe_reads_what_the_server_answers() {
    let port = serving("4.5.6", false);
    assert_eq!(
        HttpProbe.get(port, "/readyz"),
        Ok((200, "ready\n".to_owned()))
    );
    assert_eq!(
        HttpProbe.get(port, "/version"),
        Ok((200, "4.5.6\n".to_owned()))
    );
    assert_eq!(
        HttpProbe.get(port, "/nothing"),
        Ok((404, "not found\n".to_owned()))
    );

    let port = serving("4.5.6", true);
    assert_eq!(
        HttpProbe.get(port, "/readyz"),
        Ok((503, "not ready\n".to_owned()))
    );
    assert_eq!(
        HttpProbe.get(port, "/version"),
        Ok((200, "4.5.6\n".to_owned()))
    );
}

/// Catches a method other than GET, bytes that are no request, a head cut short, and a head
/// over the limit being answered as a request, and a client that goes away taking the server
/// with it.
#[test]
fn the_server_answers_what_it_is_not_asked_and_survives_its_clients() {
    let port = serving("1.0.0", false);
    let head = |status: &str, body: &str| {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };

    assert_eq!(
        raw(port, b"POST /readyz HTTP/1.1\r\n\r\n", false),
        head("405 Method Not Allowed", "method not allowed\n")
    );
    assert_eq!(
        raw(port, b"hello\r\n\r\n", false),
        head("400 Bad Request", "bad request\n")
    );
    // The client sends nothing and closes its side: the head ends at the end of the stream.
    assert_eq!(
        raw(port, b"", true),
        head("400 Bad Request", "bad request\n")
    );
    assert_eq!(
        raw(port, b"GET /readyz HT", true),
        head("400 Bad Request", "bad request\n")
    );
    // One byte over the limit, no blank line.
    let long = vec![b'a'; MAX_HEAD + 1];
    assert_eq!(
        raw(port, &long, false),
        head("400 Bad Request", "bad request\n")
    );

    // A client that connects and goes away, and one that goes away after its request.
    drop(TcpStream::connect(("127.0.0.1", port)).expect("connect"));
    let mut gone = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    gone.write_all(b"GET /version HTTP/1.1\r\n\r\n")
        .expect("send");
    drop(gone);
    assert_eq!(
        HttpProbe.get(port, "/version"),
        Ok((200, "1.0.0\n".to_owned()))
    );
}

/// Blocks until `listener` has a connection ready to accept: `poll` with no timeout, so the wait
/// ends on the kernel's event, not on a clock. A connect that has returned to the client may not be
/// in the listener's queue yet (on macOS the server's side of the handshake can finish later).
fn wait_until_acceptable(listener: &TcpListener) {
    let mut fds = [PollFd::new(listener, PollFlags::IN)];
    let ready = poll(&mut fds, None).expect("poll the listener");
    assert_eq!(
        (ready, fds[0].revents()),
        (1, PollFlags::IN),
        "the listener is readable"
    );
}

/// Catches `serve` blocking on or dropping connections that are waiting when accept fails, and
/// returning `Ok`: it ends only with the error that stopped it. The connection is in the
/// listener's queue before `serve` starts, so its first accept returns it and its second, with the
/// queue empty, fails with `WouldBlock`.
#[test]
fn serve_answers_the_waiting_connections_and_returns_the_error_that_stopped_it() {
    let (listener, port) = listener();
    listener.set_nonblocking(true).expect("non-blocking");
    let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    client
        .write_all(b"GET /version HTTP/1.1\r\n\r\n")
        .expect("send");
    wait_until_acceptable(&listener);

    let stopped = serve(
        listener,
        &Answer {
            version: "7.8.9",
            fail_ready: false,
        },
    )
    .expect_err("serve never returns Ok");
    assert_eq!(stopped.kind(), std::io::ErrorKind::WouldBlock);
    let mut answer = String::new();
    client.read_to_string(&mut answer).expect("read");
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer:?}");
    assert!(answer.ends_with("\r\n\r\n7.8.9\n"), "{answer:?}");
}

/// Catches a probe that reports a port nobody listens on as an answer.
#[test]
fn the_probe_cannot_connect_to_a_closed_port() {
    let (listener, port) = listener();
    drop(listener);
    let why = HttpProbe.get(port, "/readyz").expect_err("nobody listens");
    assert!(
        why.starts_with(&format!("cannot connect to 127.0.0.1:{port}: ")),
        "{why}"
    );
}

/// Catches an answer the probe cannot read (no end of head, no HTTP/1.x status line, a status
/// that is not a number) being read as a status, and a good one (HTTP/1.0, a body that is not
/// UTF-8, no body) being refused.
#[test]
fn the_probe_refuses_what_is_not_an_http_answer_and_reads_what_is() {
    let reply = |bytes: &'static [u8]| {
        let port = once(move |mut stream| {
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(bytes);
        });
        HttpProbe.get(port, "/readyz")
    };
    let err = |got: Result<(u16, String), String>| got.expect_err("refused");

    assert!(err(reply(b"garbage")).contains("no end of head"));
    assert!(err(reply(b"")).contains("no end of head"));
    assert!(err(reply(b"HTTP/2 200 OK\r\n\r\n")).contains("no status"));
    assert!(err(reply(b"HTTP/1.1\r\n\r\n")).contains("no status"));
    assert!(err(reply(b"HTTP/1.1 two OK\r\n\r\n")).contains("no status"));
    assert!(err(reply(b"HTTP/1.1 70000 OK\r\n\r\n")).contains("no status"));
    assert_eq!(
        reply(b"HTTP/1.0 204 No Content\r\n\r\n"),
        Ok((204, String::new()))
    );
    assert_eq!(
        reply(b"HTTP/1.1 200 OK\r\nX: y\r\n\r\nb\xffdy"),
        Ok((200, "b\u{fffd}dy".to_owned()))
    );
}

/// Catches an answer that is read without bound: the probe keeps the first 64 KiB of it.
#[test]
fn the_probe_keeps_at_most_64_kib_of_an_answer() {
    let port = once(|mut stream| {
        let mut request = [0u8; 1024];
        let _ = stream.read(&mut request);
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n");
        let _ = stream.write_all(&vec![b'x'; 256 * 1024]);
    });
    let (status, body) = HttpProbe.get(port, "/readyz").expect("an answer");
    assert_eq!(status, 200);
    assert_eq!(body.len(), 64 * 1024 - "HTTP/1.1 200 OK\r\n\r\n".len());
}

/// Catches a connection reset in the middle of the exchange being read as an answer or as a
/// refused connection: it is a failure to talk.
#[test]
fn the_probe_reports_a_connection_reset() {
    // The server reads one byte of the request and closes with the rest unread: that resets.
    let port = once(|mut stream| {
        let mut one = [0u8; 1];
        let _ = stream.read_exact(&mut one);
    });
    let why = HttpProbe.get(port, "/readyz").expect_err("reset");
    assert!(
        why.starts_with(&format!("cannot talk to 127.0.0.1:{port}: ")),
        "{why}"
    );
}
