//! [`CoordinatorGet`] over the in-process transport: what it sends, how it streams a body to the
//! sink, how it tells the answers apart, and every way a call ends without a body. Each
//! connection is a real TLS session with the server of `tls`; the deadline is read on the fake
//! clock, which only the test moves.

use std::io;
use std::sync::Arc;

use hostkeeper_link::testing::{FakeCerts, FakeClock};
use hostkeeper_link::{
    CertState, CoordinatorGet, EndpointError, GetError, Got, HEAD_MAX, Limit, LinkError,
    ReplyFault, TrustFault,
};

use crate::tls::{Ca, Close, Farm, PipeTransport, Pki, config, config_text, serve};

/// A limit generous enough for the tests' bodies.
const ROOMY: Limit = Limit {
    max_bytes: 1 << 20,
    deadline_secs: 600,
};

/// A client of `farm`'s coordinator over `pki`, and what it runs on.
struct Bench {
    farm: Farm,
    transport: Arc<PipeTransport>,
    clock: Arc<FakeClock>,
    certs: Arc<FakeCerts>,
    get: CoordinatorGet,
    _pki: Pki,
}

impl Bench {
    fn new(name: &str) -> Bench {
        let farm = Farm::new();
        let pem = farm.ca.pem.clone();
        Bench::with_bundle(name, farm, &pem)
    }

    fn with_bundle(name: &str, farm: Farm, bundle: &str) -> Bench {
        let pki = Pki::with_bundle(name, bundle);
        let transport = Arc::new(PipeTransport::default());
        let clock = Arc::new(FakeClock::new(1_000, 500));
        let certs = Arc::new(FakeCerts::new(CertState::Enrolled(farm.host.identity())));
        let get = CoordinatorGet::new(
            &config(&farm.ca),
            &pki.root(),
            certs.clone(),
            transport.clone(),
            clock.clone(),
        )
        .expect("a client");
        Bench {
            farm,
            transport,
            clock,
            certs,
            get,
            _pki: pki,
        }
    }

    /// A GET of `/p` to a coordinator that answers `raw` and closes as `close` says: what the
    /// call came to, the bytes the sink got, and the request the server read.
    fn answered(
        &self,
        raw: Vec<u8>,
        close: Close,
        limit: Limit,
    ) -> (Result<Got, GetError>, Vec<u8>, Option<crate::tls::Request>) {
        let server = serve(&self.transport, self.farm.server(), close, move |_| raw);
        let mut body = Vec::new();
        let got = self.get.get("/p", limit, &mut |bytes| {
            body.extend_from_slice(bytes);
            Ok(())
        });
        (got, body, server.join().expect("the server"))
    }
}

/// An HTTP/1.1 answer whose head is `head` (lines after the status line) and body `body`.
fn raw(status_line: &str, headers: &[&str], body: &[u8]) -> Vec<u8> {
    let mut out = format!("{status_line}\r\n");
    for header in headers {
        out.push_str(header);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

fn bad_reply(fault: ReplyFault) -> Result<Got, GetError> {
    Err(GetError::Link(LinkError::BadReply(fault)))
}

/// Proves a 200 is streamed: the request is a GET of the path with the coordinator's authority,
/// `Connection: close` and no body, made with the host's certificate over a connection bounded
/// by the call's own deadline; the sink gets every byte in order and in more than one chunk when
/// the body is long; and the call says how many.
#[test]
fn a_body_is_streamed_to_the_sink_with_the_hosts_certificate() {
    let bench = Bench::new("get-streams");
    let body: Vec<u8> = (0..200_000_u32).map(|n| (n % 251) as u8).collect();
    let server = serve(&bench.transport, bench.farm.server(), Close::Notify, {
        let body = body.clone();
        move |_| plain_200(&body)
    });
    let mut chunks = Vec::new();
    let got = bench.get.get(
        "/repositories/acme/dists/web/InRelease",
        ROOMY,
        &mut |bytes| {
            chunks.push(bytes.to_vec());
            Ok(())
        },
    );
    assert_eq!(got, Ok(Got::Body(200_000)));
    assert!(chunks.len() > 1, "a long body arrives in chunks");
    assert_eq!(chunks.concat(), body);
    let request = server.join().expect("the server").expect("a request");
    assert_eq!(
        request.head,
        "GET /repositories/acme/dists/web/InRelease HTTP/1.1\r\nHost: coordinator.test:8443\r\n\
         Accept: */*\r\nConnection: close"
    );
    assert!(request.body.is_empty());
    let cert =
        rustls::pki_types::CertificateDer::from_pem_slice(bench.farm.host.cert_pem.as_bytes())
            .expect("the host certificate");
    assert_eq!(request.peer, cert.as_ref());
    assert_eq!(
        bench.transport.dialed(),
        [("coordinator.test".to_owned(), 8443)]
    );
    assert_eq!(bench.transport.bounds(), [ROOMY.deadline_secs]);
}

use rustls::pki_types::pem::PemObject;

fn plain_200(body: &[u8]) -> Vec<u8> {
    raw(
        "HTTP/1.1 200 OK",
        &[
            "Content-Type: application/octet-stream",
            &format!("Content-Length: {}", body.len()),
        ],
        body,
    )
}

/// Proves an empty 200 is a body of nothing, and that the sink is not called for it.
#[test]
fn an_empty_body_is_a_200_and_the_sink_is_not_called() {
    let bench = Bench::new("get-empty");
    let (got, body, _) = bench.answered(plain_200(b""), Close::Notify, ROOMY);
    assert_eq!(got, Ok(Got::Body(0)));
    assert!(body.is_empty());
}

/// Proves a 404 is told from other statuses without reading a body or needing a length, and
/// that any status but 200 and 404 is a refusal naming it.
#[test]
fn a_404_is_not_found_and_another_status_is_named() {
    let bench = Bench::new("get-statuses");
    let (got, body, _) = bench.answered(
        raw("HTTP/1.1 404 Not Found", &[], b""),
        Close::Notify,
        ROOMY,
    );
    assert_eq!((got, body), (Ok(Got::NotFound), Vec::new()));
    for status in [403, 502, 301] {
        let answer = raw(
            &format!("HTTP/1.1 {status} X"),
            &["Content-Length: 2"],
            b"{}",
        );
        let (got, body, _) = bench.answered(answer, Close::Notify, ROOMY);
        assert_eq!((got, body), (Err(GetError::Status(status)), Vec::new()));
    }
}

/// Catches a head the call reads loosely: a 200 with no `Content-Length`, with two, with a
/// non-number, or with a `Transfer-Encoding`; a status line that is not HTTP/1.1 and a three
/// digit code; a header with no colon; a head that is not UTF-8. Each is a reply it refuses,
/// and no byte reaches the sink.
#[test]
fn a_malformed_head_is_refused_before_any_body_is_given() {
    let bench = Bench::new("get-malformed");
    let cases: [Vec<u8>; 9] = [
        raw("HTTP/1.1 200 OK", &[], b"abc"),
        raw(
            "HTTP/1.1 200 OK",
            &["Content-Length: 3", "Content-Length: 3"],
            b"abc",
        ),
        raw("HTTP/1.1 200 OK", &["Content-Length: three"], b"abc"),
        raw(
            "HTTP/1.1 200 OK",
            &["Transfer-Encoding: chunked"],
            b"3\r\nabc\r\n0\r\n\r\n",
        ),
        raw("HTTP/1.0 200 OK", &["Content-Length: 3"], b"abc"),
        raw("HTTP/1.1 2x0 OK", &["Content-Length: 3"], b"abc"),
        raw("HTTP/1.1 200 OK", &["no colon here"], b"abc"),
        b"HTTP/1.1 200 OK\r\nX: \xff\xfe\r\nContent-Length: 3\r\n\r\nabc".to_vec(),
        raw(
            "HTTP/1.1 404 Not Found",
            &["Transfer-Encoding: chunked"],
            b"",
        ),
    ];
    for answer in cases {
        let (got, body, _) = bench.answered(answer, Close::Notify, ROOMY);
        assert_eq!(got, bad_reply(ReplyFault::Malformed));
        assert!(body.is_empty());
    }
}

/// Catches a head read without a bound: a head that never ends within [`HEAD_MAX`], and one
/// whose end comes after it, are over the bound; an answer that ends before any head is a
/// connection that ended.
#[test]
fn a_head_over_the_bound_or_missing_is_refused() {
    let bench = Bench::new("get-head-bound");
    let endless = vec![b'x'; HEAD_MAX + 4096];
    let (got, _, _) = bench.answered(endless, Close::Notify, ROOMY);
    assert_eq!(got, bad_reply(ReplyFault::OverBound));
    let padding = format!("X-Pad: {}", "p".repeat(HEAD_MAX));
    let (got, _, _) = bench.answered(
        raw("HTTP/1.1 200 OK", &[&padding, "Content-Length: 0"], b""),
        Close::Notify,
        ROOMY,
    );
    assert_eq!(got, bad_reply(ReplyFault::OverBound));
    let (got, _, _) = bench.answered(Vec::new(), Close::Notify, ROOMY);
    assert_eq!(
        got,
        Err(GetError::Link(LinkError::Transport(
            io::ErrorKind::UnexpectedEof
        )))
    );
}

/// Catches a call that takes more than the caller allows: a length over the limit is refused
/// before any byte is given; a length at the limit is taken.
#[test]
fn a_length_over_the_limit_is_refused_and_one_at_it_is_taken() {
    let bench = Bench::new("get-limit");
    let limit = Limit {
        max_bytes: 5,
        deadline_secs: 60,
    };
    let (got, body, _) = bench.answered(plain_200(b"abcdef"), Close::Notify, limit);
    assert_eq!(got, Err(GetError::TooLong { length: 6, max: 5 }));
    assert!(body.is_empty());
    let (got, body, _) = bench.answered(plain_200(b"abcde"), Close::Notify, limit);
    assert_eq!((got, body), (Ok(Got::Body(5)), b"abcde".to_vec()));
}

/// Catches a short or long body taken as whole: a body that ends before its length, closed with
/// or without the TLS close, is truncated (the sink got what came); one longer than its length is
/// a malformed reply.
#[test]
fn a_body_that_is_not_its_length_is_refused() {
    let bench = Bench::new("get-length");
    let short = raw("HTTP/1.1 200 OK", &["Content-Length: 10"], b"abcde");
    for close in [Close::Notify, Close::Drop] {
        let (got, body, _) = bench.answered(short.clone(), close, ROOMY);
        assert_eq!(got, Err(GetError::Truncated));
        assert_eq!(body, b"abcde");
    }
    let long = raw("HTTP/1.1 200 OK", &["Content-Length: 3"], b"abcdef");
    let (got, _, _) = bench.answered(long, Close::Notify, ROOMY);
    assert_eq!(got, bad_reply(ReplyFault::Malformed));
}

/// Catches a sink error that is swallowed: a sink that refuses ends the call with its kind, and
/// nothing more is given.
#[test]
fn a_sink_that_refuses_ends_the_call() {
    let bench = Bench::new("get-sink");
    let server = serve(&bench.transport, bench.farm.server(), Close::Notify, |_| {
        plain_200(b"abcdef")
    });
    let mut calls = 0;
    let got = bench.get.get("/p", ROOMY, &mut |_| {
        calls += 1;
        Err(io::Error::from(io::ErrorKind::StorageFull))
    });
    server.join().expect("the server");
    assert_eq!(got, Err(GetError::Sink(io::ErrorKind::StorageFull)));
    assert_eq!(calls, 1);
}

/// Catches a call with no bound: a deadline that has passed before the head is read ends the
/// call; one that passes while the body comes (the clock moves in the sink) ends it before the
/// next read, with what came already given.
#[test]
fn the_deadline_is_read_on_the_injected_clock() {
    let bench = Bench::new("get-deadline");
    let now = Limit {
        max_bytes: 100,
        deadline_secs: 0,
    };
    let (got, _, _) = bench.answered(plain_200(b"abc"), Close::Notify, now);
    assert_eq!(got, Err(GetError::Deadline));

    let long = vec![7_u8; 300_000];
    let server = serve(&bench.transport, bench.farm.server(), Close::Notify, {
        let long = long.clone();
        move |_| plain_200(&long)
    });
    let clock = bench.clock.clone();
    let mut given = 0;
    let limit = Limit {
        max_bytes: 1 << 20,
        deadline_secs: 30,
    };
    let got = bench.get.get("/p", limit, &mut |bytes| {
        given += bytes.len();
        clock.advance(31);
        Ok(())
    });
    drop(server);
    assert_eq!(got, Err(GetError::Deadline));
    assert!(given > 0 && given < long.len());
}

/// Catches a path the call would send as it is given: one without a leading slash, with a space
/// or a line break, or longer than the bound is refused, and nothing is dialed.
#[test]
fn a_path_that_is_not_one_is_refused_and_nothing_is_dialed() {
    let bench = Bench::new("get-path");
    let long = format!("/{}", "a".repeat(3000));
    for path in ["p", "/a b", "/a\r\nHost: x", "/\u{e9}", long.as_str(), ""] {
        let got = bench.get.get(path, ROOMY, &mut |_| Ok(()));
        assert!(matches!(got, Err(GetError::Path(_))), "{path:?}");
    }
    assert!(bench.transport.dialed().is_empty());
}

/// Catches a host that calls without a certificate: not enrolled is no call, and the call goes
/// again once the source has a pair.
#[test]
fn a_host_that_is_not_enrolled_makes_no_call() {
    let bench = Bench::new("get-not-enrolled");
    let pair = bench.farm.host.identity();
    bench.certs.set(CertState::NotEnrolled {
        waiting_for: "/pki".into(),
    });
    assert_eq!(
        bench.get.get("/p", ROOMY, &mut |_| Ok(())),
        Err(GetError::NotEnrolled)
    );
    assert!(bench.transport.dialed().is_empty());
    bench.certs.set(CertState::Enrolled(pair));
    let (got, _, _) = bench.answered(plain_200(b"x"), Close::Notify, ROOMY);
    assert_eq!(got, Ok(Got::Body(1)));
}

/// Catches a client that trusts more than the pinned CA, or dials without trust: a server of
/// another CA, and a bundle with no pinned certificate, are refused as the heartbeat's link
/// refuses them; an address no one answers is a transport error; a bad client identity is
/// refused before anything is dialed.
#[test]
fn trust_and_reach_are_the_heartbeat_links() {
    let other = Ca::new("another ca");
    let bench = Bench::new("get-untrusted");
    let foreign = crate::tls::server_config(&other.server(crate::tls::COORDINATOR), &bench.farm.ca);
    let server = serve(&bench.transport, foreign, Close::Notify, |_| {
        plain_200(b"x")
    });
    let got = bench.get.get("/p", ROOMY, &mut |_| Ok(()));
    server.join().expect("the server");
    assert_eq!(
        got,
        Err(GetError::Link(LinkError::Untrusted(
            TrustFault::ServerCertificate
        )))
    );

    assert_eq!(
        bench.get.get("/p", ROOMY, &mut |_| Ok(())),
        Err(GetError::Link(LinkError::Transport(
            io::ErrorKind::ConnectionRefused
        )))
    );

    bench
        .certs
        .set(CertState::Enrolled(hostkeeper_link::ClientIdentity::new(
            b"not pem".to_vec(),
            b"not pem".to_vec(),
        )));
    assert_eq!(
        bench.get.get("/p", ROOMY, &mut |_| Ok(())),
        Err(GetError::Link(LinkError::Untrusted(
            TrustFault::ClientIdentity
        )))
    );

    let farm = Farm::new();
    let unpinned = Bench::with_bundle("get-unpinned", farm, &other.pem);
    assert_eq!(
        unpinned.get.get("/p", ROOMY, &mut |_| Ok(())),
        Err(GetError::Link(LinkError::Untrusted(TrustFault::NoPinnedCa)))
    );
    assert!(unpinned.transport.dialed().is_empty());
}

/// Catches a read error taken as the end of the stream: a record the client cannot open, before
/// the head and in the middle of the body, is a transport error (the sink got what came before),
/// not a truncated or a whole body.
#[test]
fn a_connection_that_fails_is_a_transport_error() {
    let bench = Bench::new("get-garbage");
    let broken = Err(GetError::Link(LinkError::Transport(
        io::ErrorKind::InvalidData,
    )));
    let (got, body, _) = bench.answered(Vec::new(), Close::Garbage, ROOMY);
    assert_eq!((got, body), (broken.clone(), Vec::new()));
    let (got, body, _) = bench.answered(
        raw("HTTP/1.1 200 OK", &["Content-Length: 10"], b"abc"),
        Close::Garbage,
        ROOMY,
    );
    assert_eq!((got, body), (broken, b"abc".to_vec()));
}

/// Catches a coordinator address with a path being dialed: the client refuses it as the
/// heartbeat's link does.
#[test]
fn an_address_with_a_path_is_refused_at_construction() {
    let farm = Farm::new();
    let pki = Pki::with_bundle("get-endpoint", &farm.ca.pem);
    let text = config_text("https://coordinator.test:8443/x", &farm.ca.sha256_hex());
    let parsed = hostkeeper_config::daemon::DaemonConfig::parse(&text).expect("a config");
    let refused = CoordinatorGet::new(
        &parsed,
        &pki.root(),
        Arc::new(FakeCerts::new(CertState::Enrolled(farm.host.identity()))),
        Arc::new(PipeTransport::default()),
        Arc::new(FakeClock::new(0, 0)),
    );
    assert!(matches!(refused, Err(EndpointError::Path(_))));
}
