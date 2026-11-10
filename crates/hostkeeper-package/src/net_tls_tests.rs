//! [`UreqTransport`] against a hermetic TLS server on 127.0.0.1 inside the test process: a
//! rustls server on a free port, thread per connection, with the committed test CA of
//! `tests/fixtures/tls/` (see its README). Nothing leaves the machine and nothing waits on a
//! clock: the server reports each request head on a channel before it answers, and a test reads
//! the heads after the server has been stopped and joined.

use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use rustls::{ServerConfig, ServerConnection, StreamOwned};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use super::*;
use crate::ports::Stop;
use crate::testing::FakeClock;

const CA_PEM: &[u8] = include_bytes!("../tests/fixtures/tls/ca.pem");
const OTHER_CA_PEM: &[u8] = include_bytes!("../tests/fixtures/tls/other-ca.pem");
const SERVER_PEM: &[u8] = include_bytes!("../tests/fixtures/tls/server.pem");
const SERVER_KEY_PEM: &[u8] = include_bytes!("../tests/fixtures/tls/server.key");

/// What the server answers: its port and the request head in, the raw response out.
type Reply = Arc<dyn Fn(u16, &str) -> Vec<u8> + Send + Sync>;

/// The DER of the one certificate in `pem`.
fn der_of(pem: &[u8]) -> Vec<u8> {
    CertificateDer::from_pem_slice(pem)
        .expect("a certificate")
        .to_vec()
}

fn server_config() -> Arc<ServerConfig> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(SERVER_PEM)
        .collect::<Result<_, _>>()
        .expect("the server certificate");
    let key = PrivateKeyDer::from_pem_slice(SERVER_KEY_PEM).expect("the server key");
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .expect("a server certificate that matches its key");
    Arc::new(config)
}

/// Serves one connection: the handshake, one request head (sent on `heads` before the answer),
/// the answer, a clean close. A client that stops the handshake or the head ends it with `Err`.
fn serve(
    stream: TcpStream,
    config: Arc<ServerConfig>,
    port: u16,
    reply: &Reply,
    heads: &Sender<String>,
) -> io::Result<()> {
    let connection = ServerConnection::new(config).expect("a server connection");
    let mut tls = StreamOwned::new(connection, stream);
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0_u8; 1];
        tls.read_exact(&mut byte)?;
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).expect("an ASCII request head");
    let response = reply(port, &head);
    heads.send(head).expect("the test listens for heads");
    drop(tls.write_all(&response));
    tls.conn.send_close_notify();
    drop(tls.flush());
    Ok(())
}

/// A TLS server on a free loopback port, until [`TlsServer::finish`].
struct TlsServer {
    port: u16,
    stop: Arc<AtomicBool>,
    accept: JoinHandle<()>,
    heads: Receiver<String>,
}

impl TlsServer {
    fn start(reply: Reply) -> TlsServer {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, heads) = channel();
        let config = server_config();
        let stopped = Arc::clone(&stop);
        let accept = std::thread::spawn(move || {
            let mut handlers = Vec::new();
            for stream in listener.incoming() {
                let stream = stream.expect("an accepted connection");
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let (config, reply, sender) = (Arc::clone(&config), reply.clone(), sender.clone());
                handlers.push(std::thread::spawn(move || {
                    drop(serve(stream, config, port, &reply, &sender));
                }));
            }
            for handler in handlers {
                handler.join().expect("a connection thread");
            }
        });
        TlsServer {
            port,
            stop,
            accept,
            heads,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("https://127.0.0.1:{}{path}", self.port)
    }

    /// Stops the server, waits for every connection to end, and returns the request heads it saw
    /// in the order it answered them.
    fn finish(self) -> Vec<String> {
        self.stop.store(true, Ordering::SeqCst);
        drop(TcpStream::connect(("127.0.0.1", self.port)).expect("the wake-up connection"));
        self.accept.join().expect("the accept thread");
        self.heads.try_iter().collect()
    }
}

/// The answers of the tests: a body with a cookie for any path, a redirect to another host for
/// `/moved-away`, one to this server for `/moved-here`, one to this server over plain `http://`
/// for `/moved-to-http`, and one to `/moved-here` for `/moved-again`.
fn standard_reply() -> Reply {
    Arc::new(|port: u16, head: &str| {
        let path = head.split_whitespace().nth(1).expect("a request target");
        let answer = match path {
            "/moved-away" => "302 Found\r\nLocation: https://other.example/pkg/1.tar".to_owned(),
            "/moved-here" => format!("302 Found\r\nLocation: https://127.0.0.1:{port}/pkg/1.tar"),
            "/moved-to-http" => format!("302 Found\r\nLocation: http://127.0.0.1:{port}/pkg/1.tar"),
            "/challenge" => {
                "401 Unauthorized\r\nWWW-Authenticate: Bearer realm=\"https://r.example/t\""
                    .to_owned()
            }
            "/moved-again" => format!("302 Found\r\nLocation: https://127.0.0.1:{port}/moved-here"),
            _ => "200 OK\r\nSet-Cookie: session=1\r\nContent-Length: 5".to_owned(),
        };
        let body = if path.starts_with("/moved") {
            ""
        } else {
            "hello"
        };
        format!("HTTP/1.1 {answer}\r\nConnection: close\r\n\r\n{body}").into_bytes()
    })
}

fn timeouts() -> Timeouts {
    Timeouts {
        connect: Duration::from_secs(30),
        total: Duration::from_secs(60),
    }
}

/// The lower-cased header names of a request head.
fn header_names(head: &str) -> Vec<String> {
    head.lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.to_ascii_lowercase())
        .collect()
}

/// A sink that keeps what it is given.
#[derive(Default)]
struct Collect {
    announced: Vec<Option<u64>>,
    body: Vec<u8>,
}

impl Sink for Collect {
    fn begin(&mut self, content_length: Option<u64>) -> Result<(), Stop> {
        self.announced.push(content_length);
        Ok(())
    }

    fn chunk(&mut self, bytes: &[u8]) -> Result<(), Stop> {
        self.body.extend_from_slice(bytes);
        Ok(())
    }
}

/// With the test CA as its root the transport completes the handshake with the server's
/// certificate for 127.0.0.1 and returns the status, the announced length and the body.
///
/// Catches a client that does not use the roots it is given, and one that reads status, length
/// or body from the wrong place over a real TLS connection.
#[test]
fn ureq_tls_trusted_root_gives_status_length_and_body() {
    let server = TlsServer::start(standard_reply());
    let transport = UreqTransport::new(&[der_of(CA_PEM)]);
    let mut response = transport
        .open(&server.url("/pkg/1.tar"), timeouts())
        .expect("a response");
    assert_eq!(response.status, 200);
    assert_eq!(response.content_length, Some(5));
    let mut body = Vec::new();
    response.body.read_to_end(&mut body).expect("the body");
    assert_eq!(body, b"hello".to_vec());
    assert_eq!(server.finish().len(), 1);
}

/// With no roots, or only a root that did not sign the server's certificate, the handshake is
/// refused as no connection and the server never sees a request.
///
/// Catches a client that trusts the system's or any root when it is given none, and a
/// certificate accepted without a chain to a given root.
#[test]
fn ureq_tls_untrusted_server_is_no_connection_and_gets_no_request() {
    for roots in [Vec::new(), vec![der_of(OTHER_CA_PEM)]] {
        let server = TlsServer::start(standard_reply());
        let transport = UreqTransport::new(&roots);
        let result = transport.open(&server.url("/pkg/1.tar"), timeouts());
        assert_eq!(result.err(), Some(NetError::Connect));
        assert_eq!(server.finish(), Vec::<String>::new());
    }
}

/// A 302 with a `Location` header, to another host or to this server, comes back as the 302 and
/// the server saw exactly one request: the redirect is not followed.
///
/// Catches a client that follows a redirect header, to another host or to the same one.
#[test]
fn ureq_tls_redirect_headers_are_returned_and_not_followed() {
    for path in ["/moved-away", "/moved-here"] {
        let server = TlsServer::start(standard_reply());
        let transport = UreqTransport::new(&[der_of(CA_PEM)]);
        let response = transport
            .open(&server.url(path), timeouts())
            .expect("the redirect itself");
        assert_eq!(response.status, 302, "{path}");
        let heads = server.finish();
        assert_eq!(heads.len(), 1, "{path}");
        let first = heads.first().expect("the request");
        assert!(
            first.starts_with(&format!("GET {path} HTTP/1.1\r\n")),
            "{first}"
        );
    }
}

/// Two requests through one transport, after a response that set a cookie, ask for their path
/// with GET and carry no `Cookie`, `Authorization` or `Proxy-Authorization` header.
///
/// Catches a client that keeps cookies, sends credentials or goes through a proxy-style request.
#[test]
fn ureq_tls_requests_carry_no_cookie_or_credentials() {
    let server = TlsServer::start(standard_reply());
    let transport = UreqTransport::new(&[der_of(CA_PEM)]);
    for _ in 0..2 {
        let mut response = transport
            .open(&server.url("/pkg/1.tar"), timeouts())
            .expect("a response");
        let mut body = Vec::new();
        response.body.read_to_end(&mut body).expect("the body");
    }
    let heads = server.finish();
    assert_eq!(heads.len(), 2);
    for head in heads {
        assert_eq!(head.lines().next(), Some("GET /pkg/1.tar HTTP/1.1"));
        let names = header_names(&head);
        assert!(names.contains(&"host".to_owned()), "{head}");
        for refused in ["cookie", "authorization", "proxy-authorization"] {
            assert!(!names.contains(&refused.to_owned()), "{refused} in {head}");
        }
    }
}

/// Sent with `open_with`, a request carries the `Authorization` value it was given (and none when
/// it was given none), a 302 comes back unfollowed with its `Location` although the transport was
/// made to follow redirects, and a 401 comes back with its `WWW-Authenticate` value.
///
/// Catches a credential not sent, one sent when none was given, a redirect followed behind the
/// fetcher's back (so a credential could cross origins), and the two headers not read.
#[test]
fn ureq_tls_open_with_sends_the_header_and_returns_redirects_and_challenges() {
    let server = TlsServer::start(standard_reply());
    let transport = UreqTransport::following_redirects(&[der_of(CA_PEM)], 3);
    let moved = transport
        .open_with(&server.url("/moved-here"), Some("Basic dTpz"), timeouts())
        .expect("the redirect itself");
    assert_eq!(moved.status, 302);
    assert_eq!(moved.location, Some(server.url("/pkg/1.tar")));
    assert_eq!(moved.www_authenticate, None);
    let challenged = transport
        .open_with(&server.url("/challenge"), None, timeouts())
        .expect("the challenge");
    assert_eq!(challenged.status, 401);
    assert_eq!(
        challenged.www_authenticate.as_deref(),
        Some("Bearer realm=\"https://r.example/t\"")
    );
    assert_eq!(challenged.location, None);
    let heads = server.finish();
    assert_eq!(heads.len(), 2);
    assert!(
        heads[0].contains("authorization: Basic dTpz\r\n"),
        "{}",
        heads[0]
    );
    assert!(
        !header_names(&heads[1]).contains(&"authorization".to_owned()),
        "{}",
        heads[1]
    );
}

/// End to end through [`NetFetcher`]: the body of a 200 reaches the sink after its announced
/// length, and a 302 is [`NetError::Status`].
///
/// Catches an adapter and transport that do not work together over TLS: a body lost, a length
/// not announced, or a redirect delivered as a package.
#[test]
fn net_fetcher_over_tls_delivers_a_body_and_refuses_a_redirect() {
    let server = TlsServer::start(standard_reply());
    let clock = Arc::new(FakeClock::at(10));
    let shared: Arc<dyn Clock> = clock.clone();
    let fetcher = NetFetcher::new(UreqTransport::new(&[der_of(CA_PEM)]), shared);
    let limit = || Limit {
        connect_timeout: Duration::from_secs(30),
        deadline: clock.stamp(1000),
    };
    let mut sink = Collect::default();
    let got = fetcher.get(&Url::new(server.url("/pkg/1.tar")), &mut sink, limit());
    assert_eq!(got, Ok(Got::Complete));
    assert_eq!(sink.announced, vec![Some(5)]);
    assert_eq!(sink.body, b"hello".to_vec());
    let mut other = Collect::default();
    let refused = fetcher.get(&Url::new(server.url("/moved-here")), &mut other, limit());
    assert_eq!(refused, Err(NetError::Status(302)));
    assert!(other.body.is_empty());
    assert_eq!(server.finish().len(), 2);
}

/// Made with [`UreqTransport::following_redirects`], a 302 to this server is followed: the body
/// of the second request comes back, and the server saw both requests. A redirect to an
/// `http://` URL is no connection, and the server saw only the first request. A chain longer
/// than the bound comes back as its last redirect, unfollowed.
///
/// Catches a release-asset download that stops at a GitHub redirect, one that follows a redirect
/// off HTTPS, and one that follows redirects without bound.
#[test]
fn ureq_tls_following_redirects_follows_https_only_and_within_its_bound() {
    let server = TlsServer::start(standard_reply());
    let transport = UreqTransport::following_redirects(&[der_of(CA_PEM)], 1);
    let mut response = transport
        .open(&server.url("/moved-here"), timeouts())
        .expect("the followed response");
    assert_eq!(response.status, 200);
    let mut body = Vec::new();
    response.body.read_to_end(&mut body).expect("the body");
    assert_eq!(body, b"hello".to_vec());
    let refused = transport.open(&server.url("/moved-to-http"), timeouts());
    assert_eq!(refused.err(), Some(NetError::Connect));
    let bounded = transport
        .open(&server.url("/moved-again"), timeouts())
        .expect("the last redirect");
    assert_eq!(bounded.status, 302);
    let targets: Vec<String> = server
        .finish()
        .iter()
        .map(|head| head.lines().next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        targets,
        [
            "GET /moved-here HTTP/1.1",
            "GET /pkg/1.tar HTTP/1.1",
            "GET /moved-to-http HTTP/1.1",
            "GET /moved-again HTTP/1.1",
            "GET /moved-here HTTP/1.1",
        ]
    );
}
