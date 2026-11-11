//! The HTTPS transport to ABM ([`crate::abm::transport::HttpsTransport`]), against a server in
//! process: the dial answers with one end of an in-memory stream and the other end speaks TLS with
//! a certificate of the test's own for `mdmenrollment.apple.com`. No socket is opened and nothing
//! waits: the one test of a timeout runs on the runtime's paused clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::FutureExt as _;
use hostkeeper_oidc::{Dial, Roots, Stream};
use hyper::Method;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _, DuplexStream};
use tokio_rustls::TlsAcceptor;

use crate::abm::transport::{ABM_BASE, AbmRequest, AbmTransport, HttpsTransport, TransportError};
use crate::origin_https::{OriginTlsError, OriginUrl};

const HOST: &str = "mdmenrollment.apple.com";

/// What the server does after it has read a request.
#[derive(Clone)]
enum Then {
    /// Writes these bytes and closes.
    Write(Vec<u8>),
    /// Reads the request and closes without an answer.
    Close,
    /// Never answers.
    Hang,
}

/// What the server read of one request.
#[derive(Clone, Debug)]
struct Heard {
    head: String,
    body: Vec<u8>,
}

/// A server with a certificate of its own, and what it heard.
struct Server {
    acceptor: TlsAcceptor,
    root: CertificateDer<'static>,
    heard: Arc<Mutex<Vec<Heard>>>,
}

fn server_for(names: &[&str]) -> (TlsAcceptor, CertificateDer<'static>) {
    let certified = rcgen::generate_simple_self_signed(
        names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>(),
    )
    .expect("a certificate");
    let root = certified.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
        certified.signing_key.serialize_der(),
    ));
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![root.clone()], key)
        .expect("a server config");
    (TlsAcceptor::from(Arc::new(config)), root)
}

impl Server {
    fn new() -> Self {
        let (acceptor, root) = server_for(&[HOST]);
        Self {
            acceptor,
            root,
            heard: Arc::default(),
        }
    }

    fn roots(&self) -> Roots {
        Roots::Given(vec![self.root.clone()])
    }

    /// A dial whose connections this server answers with `then`.
    fn dial(&self, then: Then) -> Dial {
        let (acceptor, heard) = (self.acceptor.clone(), Arc::clone(&self.heard));
        Arc::new(move |_: String, _: u16| {
            let (client, server) = tokio::io::duplex(256 * 1024);
            tokio::spawn(serve(
                acceptor.clone(),
                server,
                then.clone(),
                Arc::clone(&heard),
            ));
            async move { Ok::<Stream, std::io::Error>(Box::new(client)) }.boxed()
        })
    }

    fn transport(&self, then: Then) -> HttpsTransport {
        let base = OriginUrl::parse(ABM_BASE).expect("a base");
        HttpsTransport::new(&base, self.roots())
            .expect("a transport")
            .with_dial(self.dial(then))
    }
}

async fn serve(
    acceptor: TlsAcceptor,
    server: DuplexStream,
    then: Then,
    heard: Arc<Mutex<Vec<Heard>>>,
) {
    let Ok(mut tls) = acceptor.accept(server).await else {
        return;
    };
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match tls.read(&mut byte).await {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .map(str::to_owned)
        })
        .and_then(|n| n.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0_u8; length];
    if tls.read_exact(&mut body).await.is_err() {
        return;
    }
    heard.lock().expect("heard").push(Heard { head, body });
    match then {
        Then::Write(bytes) => {
            let _ = tls.write_all(&bytes).await;
            let _ = tls.shutdown().await;
        }
        Then::Close => {}
        Then::Hang => futures::future::pending::<()>().await,
    }
}

fn answer(status: &str, extra: &str, body: &[u8]) -> Then {
    let mut bytes = format!(
        "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    Then::Write(bytes)
}

fn request(method: Method, path: &str, body: Option<&[u8]>) -> AbmRequest {
    AbmRequest {
        method,
        path: path.to_owned(),
        headers: vec![("X-ADM-Auth-Session", "the-session".to_owned())],
        body: body.map(<[u8]>::to_vec),
        timeout: Duration::from_secs(30),
    }
}

/// Proves: a request goes to the host named in the base URL with its method, path, headers and
/// body (a JSON body with its content type, a request with no body with none), a `Connection:
/// close`, and the answer's status, body and `Retry-After` (when it is a number of seconds) come
/// back; a `Retry-After` that is a date is read as none; a path with a query reaches the server
/// whole.
///
/// Catches: a header that is dropped, a body sent without its type (which Apple answers with a
/// 4xx for every write), and a `Retry-After` read wrong, which would make the client wait for ever
/// or not at all.
#[tokio::test]
async fn a_request_and_its_answer_cross_tls_whole() {
    let server = Server::new();
    let transport = server.transport(answer(
        "429 Too Many Requests",
        "Retry-After: 7\r\n",
        b"slow down",
    ));
    let sent = transport
        .send(request(
            Method::POST,
            "/server/devices",
            Some(b"{\"limit\":1000}"),
        ))
        .await
        .expect("answered");
    assert_eq!(
        (sent.status, sent.retry_after, sent.body.as_slice()),
        (429, Some(7), &b"slow down"[..])
    );
    let heard = server.heard.lock().expect("heard")[0].clone();
    let mut lines = heard.head.lines();
    assert_eq!(lines.next(), Some("POST /server/devices HTTP/1.1"));
    let rest: Vec<String> = lines.map(str::to_ascii_lowercase).collect();
    for want in [
        "host: mdmenrollment.apple.com",
        "user-agent: hostkeeper-coordinator",
        "connection: close",
        "content-type: application/json;charset=utf8",
        "x-adm-auth-session: the-session",
        "content-length: 14",
    ] {
        assert!(rest.iter().any(|l| l == want), "{want} in {rest:?}");
    }
    assert_eq!(heard.body, b"{\"limit\":1000}");

    let transport = server.transport(answer(
        "200 OK",
        "Retry-After: Wed, 21 Oct 2026 07:28:00 GMT\r\n",
        b"{}",
    ));
    let sent = transport
        .send(request(Method::GET, "/profile?profile_uuid=ABC", None))
        .await
        .expect("answered");
    assert_eq!((sent.status, sent.retry_after), (200, None));
    let heard = server.heard.lock().expect("heard")[1].clone();
    assert_eq!(
        heard.head.lines().next(),
        Some("GET /profile?profile_uuid=ABC HTTP/1.1")
    );
    assert!(!heard.head.to_ascii_lowercase().contains("content-type"));
    assert!(heard.body.is_empty());
}

/// Proves: each way a request fails is its own message and none carries a header or a body: a
/// connection that is refused, a certificate the roots do not trust, an answer cut off, a header
/// value that cannot be sent, an answer over the limit, and a server that never answers within the
/// request's time (on the paused clock).
///
/// Catches: an error that prints the session or the signature, and a request that waits for ever.
#[tokio::test(start_paused = true)]
async fn each_failure_of_a_request_is_named_without_its_content() {
    let server = Server::new();
    let refusing: Dial = Arc::new(|_: String, _: u16| {
        async { Err::<Stream, _>(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)) }
            .boxed()
    });
    let base = OriginUrl::parse(ABM_BASE).expect("a base");
    let refused = HttpsTransport::new(&base, server.roots())
        .expect("a transport")
        .with_dial(refusing)
        .send(request(Method::GET, "/account", None))
        .await;
    assert_eq!(
        refused.map(drop),
        Err(TransportError("ABM: the connection failed".to_owned()))
    );

    // Another certificate than the roots trust.
    let (other_acceptor, _) = server_for(&[HOST]);
    let untrusted = Server {
        acceptor: other_acceptor,
        root: server.root.clone(),
        heard: Arc::default(),
    };
    let handshake = untrusted
        .transport(Then::Close)
        .send(request(Method::GET, "/account", None))
        .await;
    assert_eq!(
        handshake.map(drop),
        Err(TransportError("ABM: the TLS handshake failed".to_owned()))
    );

    let cut = server
        .transport(Then::Close)
        .send(request(Method::GET, "/account", None))
        .await;
    assert_eq!(
        cut.map(drop),
        Err(TransportError("ABM: the request failed".to_owned()))
    );

    let mut odd = request(Method::GET, "/account", None);
    odd.headers = vec![("X-ADM-Auth-Session", "line\nbreak".to_owned())];
    let header = server.transport(Then::Close).send(odd).await;
    assert_eq!(
        header.map(drop),
        Err(TransportError("ABM: a header value failed".to_owned()))
    );

    let big = server
        .transport(answer("200 OK", "", &[b'x'; 64]))
        .with_max_answer(8)
        .send(request(Method::GET, "/account", None))
        .await;
    assert_eq!(
        big.map(drop),
        Err(TransportError("ABM: reading the answer failed".to_owned()))
    );

    let mut slow = request(Method::GET, "/account", None);
    slow.timeout = Duration::from_secs(5);
    let hung = server.transport(Then::Hang).send(slow).await;
    assert_eq!(
        hung.map(drop),
        Err(TransportError("ABM: the request timed out".to_owned()))
    );
}

/// Proves: a transport is not made with roots that hold no certificate.
///
/// Catches: a coordinator that starts talking to Apple with nothing to check its certificate by.
#[test]
fn a_transport_needs_roots() {
    let base = OriginUrl::parse(ABM_BASE).expect("a base");
    let made = HttpsTransport::new(&base, Roots::Given(Vec::new()));
    assert!(matches!(made, Err(OriginTlsError::NoRoots)));
}
