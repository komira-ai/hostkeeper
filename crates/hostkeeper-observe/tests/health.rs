//! The health URL's checks, and the real HTTP probe driven over the in-memory transport: no socket
//! is opened.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::time::Duration;

use hostkeeper_observe::fake::{Chunk, FakeConnect};
use hostkeeper_observe::health::MAX_REPLY_BYTES;
use hostkeeper_observe::{
    Deadline, HealthClient, HealthReply, HealthUrl, HttpHealthClient, ObserveError, RunError,
    UrlRefusal,
};

const DEADLINE: Deadline = Deadline {
    connect: Duration::from_secs(1),
    read: Duration::from_secs(2),
};

fn url(text: &str) -> HealthUrl {
    HealthUrl::parse(text).expect("a valid URL")
}

fn bytes(text: &str) -> Chunk {
    Chunk::Bytes(text.as_bytes().to_vec())
}

/// Catches a URL that names a host to resolve, uses TLS, or carries credentials being probed.
#[test]
fn health_url_refuses_names_https_and_userinfo() {
    let cases = [
        ("https://127.0.0.1/healthz", UrlRefusal::Scheme),
        ("HTTP://127.0.0.1/healthz", UrlRefusal::Scheme),
        ("127.0.0.1/healthz", UrlRefusal::Scheme),
        ("http://user@127.0.0.1/healthz", UrlRefusal::Userinfo),
        ("http://user:pw@127.0.0.1:80/", UrlRefusal::Userinfo),
        ("http://tierstore/healthz", UrlRefusal::Host),
        ("http://localhost:9301/healthz", UrlRefusal::Host),
        ("http://:9301/healthz", UrlRefusal::Host),
        ("http://127.0.0.1:/healthz", UrlRefusal::Port),
        ("http://127.0.0.1:0/healthz", UrlRefusal::Port),
        ("http://127.0.0.1:65536/healthz", UrlRefusal::Port),
        ("http://127.0.0.1:+80/healthz", UrlRefusal::Port),
        ("http://127.0.0.1:{port}/healthz", UrlRefusal::Port),
        ("http://127.0.0.1/health z", UrlRefusal::Path),
        ("http://127.0.0.1/healthz\r\nX: y", UrlRefusal::Path),
        ("http://127.0.0.1/healthz#top", UrlRefusal::Path),
        ("http://127.0.0.1/h\u{e9}", UrlRefusal::Path),
    ];
    for (text, refusal) in cases {
        assert_eq!(HealthUrl::parse(text), Err(refusal), "{text}");
    }
    let tokens: Vec<&str> = [
        UrlRefusal::Scheme,
        UrlRefusal::Userinfo,
        UrlRefusal::Host,
        UrlRefusal::Port,
        UrlRefusal::Path,
    ]
    .into_iter()
    .map(UrlRefusal::token)
    .collect();
    assert_eq!(tokens, ["scheme", "userinfo", "host", "port", "path"]);
    assert_eq!(
        UrlRefusal::Host.to_string(),
        "the health URL's host is not 127.0.0.1 or [::1]"
    );
}

/// Catches a loopback check wider or narrower than the decision: `127.0.0.1` and `::1` (in any
/// spelling) are accepted; `localhost` (a name, never resolved), `127.0.0.2` (loopback, but not
/// the address the design and the manifest loader name), `0.0.0.0` (unspecified, not loopback)
/// and `[::ffff:127.0.0.1]` (an IPv4 address in IPv6 form, not `::1`) are refused.
#[test]
fn health_url_accepts_ipv4_and_ipv6_loopback_literals_only() {
    let accepted = [
        (
            "http://127.0.0.1:9301/healthz",
            "127.0.0.1:9301",
            "/healthz",
        ),
        ("http://[::1]:9301/healthz", "[::1]:9301", "/healthz"),
        ("http://[0:0:0:0:0:0:0:1]/", "[::1]:80", "/"),
        ("http://127.0.0.1", "127.0.0.1:80", "/"),
        ("http://127.0.0.1?deep=1", "127.0.0.1:80", "/?deep=1"),
        (
            "http://[::1]/ready?deep=1&x=%2F",
            "[::1]:80",
            "/ready?deep=1&x=%2F",
        ),
    ];
    for (text, addr, target) in accepted {
        let parsed = url(text);
        assert_eq!(
            parsed.addr(),
            addr.parse::<SocketAddr>().expect("an address"),
            "{text}"
        );
        assert_eq!(parsed.target(), target, "{text}");
        assert_eq!(parsed.to_string(), text);
    }
    for text in [
        "http://localhost/healthz",
        "http://127.0.0.2/healthz",
        "http://0.0.0.0/healthz",
        "http://[::]/healthz",
        "http://[::ffff:127.0.0.1]/healthz",
        "http://127.1/healthz",
        "http://[::1/healthz",
        "http://::1/healthz",
    ] {
        assert_eq!(HealthUrl::parse(text), Err(UrlRefusal::Host), "{text}");
    }
}

/// Catches a request that asks anything but a plain GET of the URL's path and query, or keeps the
/// connection open.
#[test]
fn the_request_is_one_get_that_closes_the_connection() {
    assert_eq!(
        url("http://[::1]:9301/ready?deep=1").request(),
        "GET /ready?deep=1 HTTP/1.1\r\nHost: [::1]:9301\r\nUser-Agent: hostkeeper\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    let peer = FakeConnect::default();
    peer.reply(vec![bytes("HTTP/1.1 200 OK\r\n\r\n")]);
    let client = HttpHealthClient::new(peer.clone());
    let target = url("http://127.0.0.1:9301/healthz");
    client.get(&target, DEADLINE).expect("an answer");
    assert_eq!(peer.written(), target.request().into_bytes());
    assert_eq!(peer.connected(), [(target.addr(), DEADLINE)]);
}

/// Catches the status line read wrongly: the code is taken from an HTTP/1.x status line, and what
/// follows the header block is counted, not judged.
#[test]
fn the_status_code_is_read_from_the_status_line() {
    let cases = [
        ("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", 200, 2),
        ("HTTP/1.0 204\r\n\r\n", 204, 0),
        ("HTTP/1.1 503 Service Unavailable\r\n\r\n", 503, 0),
        ("HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\n\r\n", 302, 0),
    ];
    for (reply, status, body_bytes_read) in cases {
        let peer = FakeConnect::default();
        peer.reply(vec![bytes(reply)]);
        let answer = HttpHealthClient::new(peer).get(&url("http://127.0.0.1/"), DEADLINE);
        assert_eq!(
            answer,
            Ok(HealthReply {
                status,
                body_bytes_read
            }),
            "{reply:?}"
        );
    }
}

/// Catches a reply that is not HTTP being read as an answer: no status line, a status line of
/// another protocol or version, a code that is not three digits, or a line with no end.
#[test]
fn a_reply_with_no_http_status_line_is_unreachable() {
    for reply in [
        "",
        "SSH-2.0-OpenSSH\r\n\r\n",
        "HTTP/2 200\r\n\r\n",
        "HTTP/1.1 20 OK\r\n\r\n",
        "HTTP/1.1 2000 OK\r\n\r\n",
        "HTTP/1.1 2x0 OK\r\n\r\n",
        "HTTP/1.1\r\n\r\n",
        "HTTP/1.1 200 OK",
        "\u{ff}\u{fe} 200\r\n\r\n",
        // Not UTF-8: the status line's bytes are replaced below.
        "HTTP/1.1 \u{80}00\r\n\r\n",
    ] {
        let peer = FakeConnect::default();
        // `\u{80}` is two bytes in UTF-8; a lone 0x80 byte is not UTF-8 at all.
        let raw = reply.as_bytes().iter().copied().filter(|b| *b != 0xc2);
        peer.reply(vec![Chunk::Bytes(raw.collect())]);
        let answer = HttpHealthClient::new(peer).get(&url("http://127.0.0.1/"), DEADLINE);
        assert_eq!(
            answer,
            Err(ObserveError::Unreachable {
                url: "http://127.0.0.1/".to_owned(),
                why: "the reply has no HTTP status line".to_owned(),
            }),
            "{reply:?}"
        );
    }
}

/// Catches a redirect being followed: a 3xx is the answer, and no second connection or request
/// is made.
#[test]
fn a_redirect_is_unhealthy_and_is_not_followed() {
    let peer = FakeConnect::default();
    peer.reply(vec![bytes(
        "HTTP/1.1 301 Moved Permanently\r\nLocation: http://127.0.0.1:9302/healthz\r\n\r\n",
    )]);
    peer.reply(vec![bytes("HTTP/1.1 200 OK\r\n\r\n")]);
    let target = url("http://127.0.0.1:9301/healthz");
    let answer = HttpHealthClient::new(peer.clone()).get(&target, DEADLINE);
    assert_eq!(answer.map(|reply| reply.status), Ok(301));
    assert_eq!(peer.connected().len(), 1);
    assert_eq!(peer.written(), target.request().into_bytes());
}

/// Catches a reply read without bound: a peer that would send 1 MiB of header is read to 4 KiB
/// and no further; the status line within it still counts.
#[test]
fn the_response_is_read_to_at_most_4_kib() {
    assert_eq!(MAX_REPLY_BYTES, 4096);
    let mut flood = b"HTTP/1.1 200 OK\r\n".to_vec();
    flood.resize(1024 * 1024, b'x');
    let peer = FakeConnect::default();
    peer.reply(vec![Chunk::Bytes(flood)]);
    let answer = HttpHealthClient::new(peer.clone()).get(&url("http://127.0.0.1/"), DEADLINE);
    assert_eq!(
        answer,
        Ok(HealthReply {
            status: 200,
            body_bytes_read: 0
        })
    );
    assert_eq!(peer.served(), MAX_REPLY_BYTES);

    // A reply that arrives in pieces is read until its header block ends, and no further.
    let pieces = FakeConnect::default();
    pieces.reply(vec![
        bytes("HTTP/1.1 2"),
        bytes("00 OK\r\n"),
        bytes("\r\nbody"),
        bytes("never read"),
    ]);
    let answer = HttpHealthClient::new(pieces.clone()).get(&url("http://127.0.0.1/"), DEADLINE);
    assert_eq!(
        answer,
        Ok(HealthReply {
            status: 200,
            body_bytes_read: 4
        })
    );
    assert_eq!(pieces.served(), "HTTP/1.1 200 OK\r\n\r\nbody".len());
}

/// Catches a transport failure judged wrongly: a peer that refuses, resets or times out did not
/// answer (unreachable); a failure on this side (no descriptor, a bad argument) means the probe
/// was not made; an interrupted read is read again; a failure after the status line arrived does
/// not hide the answer.
#[test]
fn transport_failures_are_told_apart() {
    let target = url("http://127.0.0.1:9301/healthz");
    let unreachable = |why: &str| ObserveError::Unreachable {
        url: target.to_string(),
        why: why.to_owned(),
    };
    let not_made = |why: &str| ObserveError::Probe {
        url: target.to_string(),
        why: why.to_owned(),
    };
    let refused = FakeConnect::default();
    refused.refuse(ErrorKind::ConnectionRefused);
    assert_eq!(
        HttpHealthClient::new(refused).get(&target, DEADLINE),
        Err(unreachable("connection refused"))
    );
    // A transport with no connection scripted refuses as well.
    assert_eq!(
        HttpHealthClient::new(FakeConnect::default()).get(&target, DEADLINE),
        Err(unreachable("connection refused"))
    );
    let local = FakeConnect::default();
    local.refuse(ErrorKind::InvalidInput);
    assert_eq!(
        HttpHealthClient::new(local).get(&target, DEADLINE),
        Err(not_made("invalid input parameter"))
    );

    let cases = [
        (
            vec![Chunk::Fail(ErrorKind::ConnectionReset)],
            Err(unreachable("connection reset")),
        ),
        (
            vec![bytes("HTTP/1.1 2"), Chunk::Fail(ErrorKind::WouldBlock)],
            Err(unreachable("operation would block")),
        ),
        (
            vec![
                Chunk::Fail(ErrorKind::Interrupted),
                bytes("HTTP/1.1 200 OK\r\n\r\n"),
            ],
            Ok(HealthReply {
                status: 200,
                body_bytes_read: 0,
            }),
        ),
        (
            vec![
                bytes("HTTP/1.1 500 Oops\r\n"),
                Chunk::Fail(ErrorKind::TimedOut),
            ],
            Ok(HealthReply {
                status: 500,
                body_bytes_read: 0,
            }),
        ),
    ];
    for (n, (chunks, want)) in cases.into_iter().enumerate() {
        let peer = FakeConnect::default();
        peer.reply(chunks);
        assert_eq!(
            HttpHealthClient::new(peer).get(&target, DEADLINE),
            want,
            "case {n}"
        );
    }
}

/// Catches every transport error that means the peer did not answer, and only those, being read
/// as unreachable.
#[test]
fn every_peer_failure_kind_is_unreachable() {
    let target = url("http://127.0.0.1/");
    let peer_kinds = [
        ErrorKind::ConnectionRefused,
        ErrorKind::ConnectionReset,
        ErrorKind::ConnectionAborted,
        ErrorKind::NotConnected,
        ErrorKind::BrokenPipe,
        ErrorKind::TimedOut,
        ErrorKind::WouldBlock,
        ErrorKind::UnexpectedEof,
        ErrorKind::HostUnreachable,
        ErrorKind::NetworkUnreachable,
        ErrorKind::AddrNotAvailable,
    ];
    for kind in peer_kinds {
        let peer = FakeConnect::default();
        peer.refuse(kind);
        let answer = HttpHealthClient::new(peer).get(&target, DEADLINE);
        assert!(
            matches!(answer, Err(ObserveError::Unreachable { .. })),
            "{kind:?}"
        );
    }
    for kind in [
        ErrorKind::PermissionDenied,
        ErrorKind::OutOfMemory,
        ErrorKind::Other,
    ] {
        let peer = FakeConnect::default();
        peer.refuse(kind);
        let answer = HttpHealthClient::new(peer).get(&target, DEADLINE);
        assert!(
            matches!(answer, Err(ObserveError::Probe { .. })),
            "{kind:?}"
        );
    }
}

/// Catches a request that could not be written being read as an answer: a peer that closed is
/// unreachable; a failure on this side means the probe was not made. Nothing is read after it.
#[test]
fn a_request_that_cannot_be_written_is_not_an_answer() {
    let target = url("http://127.0.0.1:9301/healthz");
    let closed = FakeConnect::default();
    closed.reply(vec![bytes("HTTP/1.1 200 OK\r\n\r\n")]);
    closed.fail_writes(ErrorKind::BrokenPipe);
    assert_eq!(
        HttpHealthClient::new(closed.clone()).get(&target, DEADLINE),
        Err(ObserveError::Unreachable {
            url: target.to_string(),
            why: "broken pipe".to_owned(),
        })
    );
    assert_eq!(closed.served(), 0);
    assert!(closed.written().is_empty());
    let local = FakeConnect::default();
    local.reply(vec![bytes("HTTP/1.1 200 OK\r\n\r\n")]);
    local.fail_writes(ErrorKind::InvalidInput);
    assert_eq!(
        HttpHealthClient::new(local).get(&target, DEADLINE),
        Err(ObserveError::Probe {
            url: target.to_string(),
            why: "invalid input parameter".to_owned(),
        })
    );
}

/// Catches an error message that loses what failed or why: each variant's whole text.
#[test]
fn every_error_says_what_failed_and_why() {
    let observe = [
        (
            ObserveError::Manager {
                unit: "u.service".to_owned(),
                why: "w".to_owned(),
            },
            "the service manager cannot tell the state of u.service: w",
        ),
        (
            ObserveError::Process {
                pid: 7,
                why: "w".to_owned(),
            },
            "process 7 cannot be read: w",
        ),
        (
            ObserveError::Unreachable {
                url: "U".to_owned(),
                why: "w".to_owned(),
            },
            "U did not answer: w",
        ),
        (
            ObserveError::Probe {
                url: "U".to_owned(),
                why: "w".to_owned(),
            },
            "the probe of U could not be made: w",
        ),
    ];
    for (error, text) in observe {
        assert_eq!(error.to_string(), text);
    }
    let run = [
        (RunError::Empty, "there is no program to run"),
        (
            RunError::Failed {
                program: "p".to_owned(),
                why: "w".to_owned(),
            },
            "cannot run p: w",
        ),
        (
            RunError::TimedOut {
                program: "p".to_owned(),
            },
            "p did not finish within its deadline and was killed",
        ),
    ];
    for (error, text) in run {
        assert_eq!(error.to_string(), text);
    }
    let refusals = [
        (UrlRefusal::Scheme, "the health URL is not http://"),
        (UrlRefusal::Userinfo, "the health URL has a user name"),
        (
            UrlRefusal::Host,
            "the health URL's host is not 127.0.0.1 or [::1]",
        ),
        (
            UrlRefusal::Port,
            "the health URL's port is not a number from 1 to 65535",
        ),
        (
            UrlRefusal::Path,
            "the health URL's path or query holds a character it may not",
        ),
    ];
    for (refusal, text) in refusals {
        assert_eq!(refusal.to_string(), text);
    }
}
